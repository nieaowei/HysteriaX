use super::*;
use crate::background::maintenance;

async fn unlock(state: &AppState) {
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn prepare(state: &AppState, ca: &str, key: &str) -> PreparedAuthorityPublication {
    prepare_authority_publication(state, actor().0, ca.into(), publish_request(1, key))
        .await
        .unwrap()
}
async fn commit(state: &AppState, p: PreparedAuthorityPublication) -> Result<Value, ApiError> {
    let (actor, key, digest) = (
        p.actor.clone(),
        p.input.idempotency_key.clone(),
        p.digest.clone(),
    );
    managed_transaction(
        &state.pool,
        &actor,
        &key,
        "ca.publish",
        &digest,
        move |tx| {
            let p = p.clone();
            Box::pin(async move { publish_authority_on(tx, p).await })
        },
    )
    .await
}
async fn work(state: &AppState, ca: &str, owner: &str) -> maintenance::Work {
    maintenance::ensure(
        &state.pool,
        "test-publication",
        "ca-publication-test",
        json!({"lease_resources":[format!("ca:{ca}")]}),
    )
    .await
    .unwrap();
    maintenance::claim(&state.pool, "ca-publication-test", owner)
        .await
        .unwrap()
        .unwrap()
}
async fn commit_work(
    state: &AppState,
    work: &maintenance::Work,
    p: PreparedAuthorityPublication,
) -> Result<Value, ApiError> {
    let work = work.clone();
    db::serializable::<_, ApiError, _>(&state.pool, move |tx| {
        let (work, p) = (work.clone(), p.clone());
        Box::pin(async move {
            work.guard_on(tx).await?;
            let receipt = publish_authority_on(tx, p).await?;
            work.finish_on(tx, false, 0, None).await?;
            Ok(receipt)
        })
    })
    .await
}

#[tokio::test]
async fn preparation_has_no_writes_and_does_not_wait_for_the_old_global_lock() {
    let state = state().await;
    let ca = generated(&state).await;
    let legacy = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    let p = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        prepare(&state, &ca, "prepare-only"),
    )
    .await
    .unwrap();
    let material = p.material.unwrap();
    let payload: Value =
        serde_json::from_str(&state.secrets.decrypt(&material.payload_enc).unwrap()).unwrap();
    Authority::from_payload(&payload, Utc::now()).unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_versions").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_rotations").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_operations WHERE operation='ca.publish'"
        )
        .await,
        0
    );
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn native_retry_preserves_the_exact_prepared_cipher_batch_and_rotation_identifiers() {
    let state = state().await;
    let ca = generated(&state).await;
    let _ = node_flow::managed_node(&state, &ca).await;
    unlock(&state).await;
    let p = prepare(&state, &ca, "retry-prepared").await;
    let m = p.material.as_ref().unwrap().clone();
    sqlx::raw_sql("CREATE SEQUENCE prepared_publication_attempts; CREATE FUNCTION retry_prepared_publication() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='ca.version_prepared' AND nextval('prepared_publication_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_prepared_publication BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_prepared_publication();").execute(&state.pool).await.unwrap();
    let receipt = commit(&state, p).await.unwrap();
    assert_eq!(receipt["batch_id"], m.batch_id);
    assert_eq!(receipt["rotation_id"], m.rotation_id);
    let stored: String = sqlx::query_scalar(
        "SELECT payload_enc FROM credential_versions WHERE credential_id=$1 AND version=2",
    )
    .bind(&ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(stored, m.payload_enc);
    assert_eq!(
        count(
            &state,
            "SELECT last_value FROM prepared_publication_attempts"
        )
        .await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_rotations").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.version_prepared'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn publication_and_receipt_roll_back_with_work_confirmation_then_commit_together() {
    let state = state().await;
    let ca = generated(&state).await;
    unlock(&state).await;
    let p = prepare(&state, &ca, "atomic-ack").await;
    let m = p.material.as_ref().unwrap().clone();
    let work = work(&state, &ca, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_publication_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='ca-publication-test' AND NEW.state='complete' THEN RAISE EXCEPTION 'confirmation failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_publication_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_publication_ack();").execute(&state.pool).await.unwrap();
    assert!(commit_work(&state, &work, p.clone()).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_versions").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_ca_generations").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_operations WHERE operation='ca.publish'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.version_prepared'"
        )
        .await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='ca-publication-test' AND state='running'").await,1);
    sqlx::query("DROP TRIGGER reject_publication_ack ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    let result = commit_work(&state, &work, p).await.unwrap();
    assert_eq!(result["batch_id"], m.batch_id);
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_operations WHERE operation='ca.publish'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner='owner'"
        )
        .await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='ca-publication-test' AND state='complete'").await,1);
}

#[tokio::test]
async fn same_key_prepared_racers_publish_one_winning_material_and_replay_one_receipt() {
    let state = state().await;
    let ca = generated(&state).await;
    unlock(&state).await;
    let (a, b) = tokio::join!(
        prepare(&state, &ca, "same-prepared"),
        prepare(&state, &ca, "same-prepared")
    );
    let (ma, mb) = (
        a.material.as_ref().unwrap().clone(),
        b.material.as_ref().unwrap().clone(),
    );
    let (a, b) = tokio::join!(commit(&state, a), commit(&state, b));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a, b);
    let stored: String = sqlx::query_scalar(
        "SELECT payload_enc FROM credential_versions WHERE credential_id=$1 AND version=2",
    )
    .bind(&ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(stored == ma.payload_enc || stored == mb.payload_enc);
    assert!(a["batch_id"] == ma.batch_id || a["batch_id"] == mb.batch_id);
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_versions").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_operations WHERE operation='ca.publish'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.version_prepared'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn replaced_execution_and_expired_material_cannot_publish() {
    let state = state().await;
    let ca = generated(&state).await;
    unlock(&state).await;
    let p = prepare(&state, &ca, "old-owner").await;
    let old = work(&state, &ca, "old").await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = maintenance::claim(&state.pool, "ca-publication-test", "next")
        .await
        .unwrap()
        .unwrap();
    assert!(commit_work(&state, &old, p).await.is_err());
    let mut p = prepare(&state, &ca, "expired-material").await;
    p.material.as_mut().unwrap().expires_at = Utc::now() - Duration::seconds(1);
    assert_eq!(
        commit_work(&state, &next, p).await.unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_versions").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        0
    );
    let p = prepare(&state, &ca, "fresh-owner").await;
    commit_work(&state, &next, p).await.unwrap();
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        2
    );
}

#[tokio::test]
async fn replay_needs_no_new_material_and_keeps_the_original_receipt_after_the_ca_changes() {
    let state = state().await;
    let ca = generated(&state).await;
    unlock(&state).await;
    let p = prepare(&state, &ca, "replay-prepared").await;
    let result = commit(&state, p).await.unwrap();
    sqlx::query("UPDATE credentials SET archived=true WHERE id=$1")
        .bind(&ca)
        .execute(&state.pool)
        .await
        .unwrap();
    let p = prepare(&state, &ca, "replay-prepared").await;
    assert!(p.material.is_none());
    assert!(p.source.is_none());
    assert_eq!(p.replay, Some(result.clone()));
    assert_eq!(commit(&state, p).await.unwrap(), result);
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_versions").await,
        2
    );
    let conflict =
        prepare_authority_publication(&state, actor().0, ca, publish_request(2, "replay-prepared"))
            .await;
    assert_eq!(conflict.err().unwrap().status, StatusCode::CONFLICT);
}
