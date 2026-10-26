use super::*;
use crate::{api::managed_mtls::CreateAuthority, security::SecretBox};
use axum::{
    Json,
    extract::{Extension, State},
};
async fn fixture(n: i32) -> AppState {
    let state = AppState::new(
        db::test_pool_with_max_connections(8).await,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    crate::credentials::migration::migrate(&state.pool, &state.secrets)
        .await
        .unwrap();
    let (_,Json(ca))=managed_mtls::create(State(state.clone()),Extension(AdminActor{id:"test".into()}),Json(serde_json::from_value::<CreateAuthority>(json!({"name":"CA","source":"generated","idempotency_key":"ca-maintenance-fixture"})).unwrap())).await.unwrap();
    let ca = ca["id"].as_str().unwrap();
    if n > 1 {
        sqlx::query("INSERT INTO credentials(id,name,kind,created_at,updated_at) SELECT 'ca-'||lpad(i::text,4,'0'),'CA','ca_identity',now(),now() FROM generate_series(1,$1)i").bind(n-1).execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) SELECT c.id,1,v.payload_enc,v.metadata,now() FROM credentials c CROSS JOIN credential_versions v WHERE c.id<>$1 AND v.credential_id=$1 AND v.version=1").bind(ca).execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO mtls_authorities(credential_id,source) SELECT id,'generated' FROM credentials WHERE id<>$1").bind(ca).execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO mtls_ca_generations(ca_id,ca_version,state,expires_at,published_at) SELECT c.id,1,'published',g.expires_at,now() FROM credentials c CROSS JOIN mtls_ca_generations g WHERE c.id<>$1 AND g.ca_id=$1 AND g.ca_version=1").bind(ca).execute(&state.pool).await.unwrap();
    }
    sqlx::query("UPDATE mtls_authorities SET next_action_at=clock_timestamp()-interval '1 day'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    state
}
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn scan(state: &AppState) -> Work {
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1")
        .bind(SCAN_KIND)
        .execute(&state.pool)
        .await
        .unwrap();
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN_KIND, json!({}))
        .await
        .unwrap();
    maintenance::claim(&state.pool, SCAN_KIND, "admit")
        .await
        .unwrap()
        .unwrap()
}
async fn admit(state: &AppState) {
    let work = scan(state).await;
    admit_work(&state.pool, &work).await.unwrap();
}
async fn claim(state: &AppState, owner: &str) -> Work {
    sqlx::query(
        "UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1 AND state='queued'",
    )
    .bind(KIND)
    .execute(&state.pool)
    .await
    .unwrap();
    maintenance::claim(&state.pool, KIND, owner)
        .await
        .unwrap()
        .unwrap()
}
async fn prepare(state: &AppState, work: &Work) -> PreparedAuthorityPublication {
    let t = Target::from_work(work).unwrap();
    let input = PublishAuthority {
        expected_revision: t.revision,
        certificate: None,
        private_key: None,
        replace_rotation_id: None,
        idempotency_key: t.key(),
    };
    managed_mtls::prepare_authority_publication(
        state,
        AdminActor {
            id: "system:mtls-maintenance".into(),
        },
        t.ca,
        input,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn due_admission_is_bounded_and_busy_authorities_are_revisited_without_resetting_a_live_owner()
 {
    let state = fixture(301).await;
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query(
        "SELECT credential_id FROM mtls_authorities ORDER BY credential_id LIMIT 1 FOR UPDATE",
    )
    .fetch_one(&mut *held)
    .await
    .unwrap();
    for expected in [100, 200, 300] {
        admit(&state).await;
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-ca-publication'"
            )
            .await,
            expected
        );
    }
    let work = claim(&state, "live").await;
    assert_eq!(work.leases.len(), 2);
    admit(&state).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE owner='live' AND state='running'"
        )
        .await,
        1
    );
    held.rollback().await.unwrap();
    admit(&state).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-ca-publication'"
        )
        .await,
        301
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE version=2"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn publication_confirmation_is_atomic_and_native_conflict_publishes_one_system_receipt() {
    let state = fixture(1).await;
    admit(&state).await;
    let work = claim(&state, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_ca_work_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-ca-publication' AND NEW.state='complete' THEN RAISE EXCEPTION 'CA confirmation failed'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_ca_work_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_ca_work_ack();").execute(&state.pool).await.unwrap();
    assert!(publish_work(&state, &work).await.is_err());
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_versions").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_operations WHERE actor_id='system:mtls-maintenance'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_ca_work_ack ON background_work; CREATE SEQUENCE ca_work_attempts; CREATE FUNCTION retry_ca_work_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-ca-publication' AND NEW.state='complete' AND nextval('ca_work_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_ca_work_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_ca_work_ack();").execute(&state.pool).await.unwrap();
    publish_work(&state, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT last_value FROM ca_work_attempts").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_operations WHERE actor_id='system:mtls-maintenance' AND operation='ca.publish'").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner='owner'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
}

#[tokio::test]
async fn takeover_rejects_old_preparation_and_current_source_changes_do_not_publish() {
    let state = fixture(1).await;
    admit(&state).await;
    let old = claim(&state, "old").await;
    let p = prepare(&state, &old).await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = claim(&state, "next").await;
    assert!(commit_work(&state.pool, &old, Some(p)).await.is_err());
    sqlx::query("UPDATE credentials SET revision=revision+1")
        .execute(&state.pool)
        .await
        .unwrap();
    publish_work(&state, &next).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_versions").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE error_code='ca_publication_superseded'"
        )
        .await,
        1
    );
    admit(&state).await;
    let refreshed = claim(&state, "refreshed").await;
    assert_eq!(refreshed.payload["expected_revision"], 2);
    publish_work(&state, &refreshed).await.unwrap();
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        2
    );
}

#[tokio::test]
async fn error_and_one_hour_retry_are_guarded_and_atomic_with_confirmation() {
    let state = fixture(1).await;
    admit(&state).await;
    let work = claim(&state, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_ca_error_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-ca-publication' AND NEW.state='complete' THEN RAISE EXCEPTION 'confirmation failed'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_ca_error_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_ca_error_ack();").execute(&state.pool).await.unwrap();
    assert!(record_error(&state.pool, &work).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_authorities WHERE maintenance_error IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.publish_request_failed'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_authorities WHERE next_action_at<clock_timestamp()"
        )
        .await,
        1
    );
    sqlx::query("DROP TRIGGER reject_ca_error_ack ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    record_error(&state.pool, &work).await.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_authorities WHERE maintenance_error='CA publication failed; retry scheduled' AND next_action_at>clock_timestamp()+interval '59 minutes'").await,1);
    assert_eq!(count(&state,"SELECT count(*) FROM audit_records WHERE actor='system:mtls-maintenance' AND action='ca.publish_request_failed'").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner='owner'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn admission_ack_failure_leaves_no_authority_work_and_mismatched_candidates_are_rejected() {
    let state = fixture(2).await;
    let scan = scan(&state).await;
    sqlx::raw_sql("CREATE FUNCTION reject_ca_admission_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-ca-admission' AND NEW.state='complete' THEN RAISE EXCEPTION 'confirmation failed'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_ca_admission_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_ca_admission_ack();").execute(&state.pool).await.unwrap();
    assert!(admit_work(&state.pool, &scan).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-ca-publication'"
        )
        .await,
        0
    );
    sqlx::query("DROP TRIGGER reject_ca_admission_ack ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    admit_work(&state.pool, &scan).await.unwrap();
    let work = claim(&state, "owner").await;
    let target = Target::from_work(&work).unwrap();
    let other: String =
        sqlx::query_scalar("SELECT credential_id FROM mtls_authorities WHERE credential_id<>$1")
            .bind(&target.ca)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let input = PublishAuthority {
        expected_revision: 1,
        certificate: None,
        private_key: None,
        replace_rotation_id: None,
        idempotency_key: "wrong-work".into(),
    };
    let p = managed_mtls::prepare_authority_publication(
        &state,
        AdminActor {
            id: "system:mtls-maintenance".into(),
        },
        other,
        input,
    )
    .await
    .unwrap();
    assert!(commit_work(&state.pool, &work, Some(p)).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE version=2"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn archived_imported_or_no_longer_due_authorities_drop_old_material_and_errors() {
    for change in [
        "UPDATE credentials SET archived=true",
        "UPDATE mtls_authorities SET source='imported'",
        "UPDATE mtls_authorities SET next_action_at=clock_timestamp()+interval '1 day'",
    ] {
        let state = fixture(1).await;
        admit(&state).await;
        let work = claim(&state, "owner").await;
        let candidate = prepare(&state, &work).await;
        sqlx::query(change).execute(&state.pool).await.unwrap();
        commit_work(&state.pool, &work, Some(candidate))
            .await
            .unwrap();
        assert_eq!(
            count(&state, "SELECT count(*) FROM credential_versions").await,
            1
        );
        assert_eq!(
            count(&state, "SELECT published_version FROM mtls_authorities").await,
            1
        );
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_operations WHERE operation='ca.publish'"
            )
            .await,
            0
        );
        admit(&state).await;
        assert!(
            maintenance::claim(&state.pool, KIND, "unexpected")
                .await
                .unwrap()
                .is_none()
        );
        // A delayed preparation error cannot overwrite this newer source or its
        // retry deadline, nor acknowledge a completed work under its old lease.
        assert!(record_error(&state.pool, &work).await.is_err());
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_authorities WHERE maintenance_error IS NOT NULL"
            )
            .await,
            0
        );
    }
}

#[tokio::test]
async fn a_source_change_before_error_confirmation_is_superseded_without_audit_or_backoff() {
    let state = fixture(1).await;
    admit(&state).await;
    let work = claim(&state, "owner").await;
    sqlx::query("UPDATE credentials SET revision=revision+1")
        .execute(&state.pool)
        .await
        .unwrap();
    record_error(&state.pool, &work).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM mtls_authorities WHERE maintenance_error IS NOT NULL OR next_action_at>clock_timestamp()").await, 0);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.publish_request_failed'"
        )
        .await,
        0
    );
    assert_eq!(count(&state, "SELECT count(*) FROM background_work WHERE error_code='ca_publication_superseded' AND state='complete'").await, 1);
}

#[tokio::test]
async fn an_empty_due_range_uses_the_authority_deadline_index() {
    let state = fixture(1001).await;
    sqlx::query("UPDATE mtls_authorities SET next_action_at=clock_timestamp()+interval '1 day'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("ANALYZE mtls_authorities; ANALYZE credentials; ANALYZE background_work; ANALYZE mtls_rotations;")
        .execute(&state.pool).await.unwrap();
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE, FORMAT JSON) {DUE}"
    )))
    .bind(now)
    .bind(PAGE)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(plan.to_string().contains("mtls_authorities_due"), "{plan}");
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
    assert_eq!(
        count(&state, "SELECT count(*) FROM background_work").await,
        0
    );
}
