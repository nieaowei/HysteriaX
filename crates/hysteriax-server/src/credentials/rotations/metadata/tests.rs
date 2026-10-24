use super::*;
async fn fixture(pending: bool) -> (AppState, String, String, String) {
    crate::api::managed_mtls::tests::rotation_flow::metadata_fixture(pending).await
}
async fn claim(state: &AppState, owner: &str) -> Work {
    maintenance::claim(&state.pool, KIND, owner)
        .await
        .unwrap()
        .unwrap()
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn node(state: &AppState, id: &str) -> NodeSource {
    sqlx::query_as(NODE)
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
fn candidate(p: Preparation) -> Candidate {
    match p {
        Preparation::Ready(c) => c,
        _ => panic!("expected prepared node metadata"),
    }
}

#[tokio::test]
async fn preparation_has_zero_writes_while_the_old_lock_is_held_and_commits_the_exact_prepared_snapshot()
 {
    let (state, ca, rotation, id) = fixture(false).await;
    let work = claim(&state, "owner").await;
    let before = node(&state, &id).await;
    let total = count(&state, "SELECT count(*) FROM config_versions").await;
    let held = db::begin_write(&state.pool).await.unwrap();
    let p = tokio::time::timeout(std::time::Duration::from_secs(2), prepare(&state, &work))
        .await
        .unwrap()
        .unwrap();
    held.rollback().await.unwrap();
    assert!(node(&state, &id).await == before);
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        total
    );
    let plan = candidate(p.clone());
    let changes = plan.changes.as_ref().unwrap();
    let live = changes.live.as_ref().unwrap();
    commit(&state, &work, p).await.unwrap();
    let after = node(&state, &id).await;
    assert_eq!(after.desired_revision, 2);
    assert_eq!(after.deployed_revision, Some(2));
    assert_eq!(after.desired_config_enc, changes.desired_cipher);
    assert_eq!(
        after.deployed_config_enc.as_deref(),
        Some(live.config_cipher.as_str())
    );
    let saved: (String, String) =
        sqlx::query_as("SELECT config_enc,content_sha256 FROM config_versions WHERE id=$1")
            .bind(&live.history.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(saved.0, live.history.cipher);
    assert_eq!(saved.1, live.history.digest);
    let config: Value =
        serde_json::from_str(&state.secrets.decrypt(&after.desired_config_enc).unwrap()).unwrap();
    assert_eq!(config["mtls"]["ca_credential_version"], 2);
    assert_eq!(
        after.deployed_content_sha256,
        before.deployed_content_sha256
    );
    assert_eq!(after.mtls_activation_id, before.mtls_activation_id);
    assert!(!pending_on(&state.pool, &rotation).await.unwrap());
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
    let desired: bool = sqlx::query_scalar(
        "SELECT desired FROM mtls_node_trust WHERE node_id=$1 AND ca_id=$2 AND ca_version=2",
    )
    .bind(&id)
    .bind(ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(desired);
}

#[tokio::test]
async fn pending_desired_edits_keep_a_separate_snapshot_and_all_reference_pins() {
    let (state, _, _, id) = fixture(true).await;
    let work = claim(&state, "owner").await;
    let before = node(&state, &id).await;
    assert_eq!(before.desired_revision, 2);
    assert_eq!(before.deployed_revision, Some(1));
    let p = prepare(&state, &work).await.unwrap();
    let plan = candidate(p.clone());
    let changes = plan.changes.as_ref().unwrap();
    assert_eq!(changes.desired_revision, 4);
    assert_eq!(changes.live.as_ref().unwrap().history.revision, 3);
    assert_eq!(changes.desired_history.as_ref().unwrap().revision, 4);
    commit(&state, &work, p).await.unwrap();
    let after = node(&state, &id).await;
    assert_eq!(after.desired_revision, 4);
    assert_eq!(after.deployed_revision, Some(3));
    let name: String = sqlx::query_scalar("SELECT name FROM nodes WHERE id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(name, "Pending name");
    for h in [
        &changes.live.as_ref().unwrap().history,
        changes.desired_history.as_ref().unwrap(),
    ] {
        let edges:i64=sqlx::query_scalar("SELECT count(*) FROM credential_reference_edges WHERE kind='history' AND source_id=$1 AND node_id=$2").bind(&h.id).bind(&id).fetch_one(&state.pool).await.unwrap();
        assert!(edges >= 2);
        let dirty:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credential_reference_dirty WHERE kind='history' AND source_id=$1)").bind(&h.id).fetch_one(&state.pool).await.unwrap();
        assert!(!dirty);
    }
}

#[tokio::test]
async fn confirmation_failure_rolls_back_nodes_snapshots_refs_and_native_retry_reuses_fixed_material()
 {
    let (state, _, _, id) = fixture(true).await;
    let work = claim(&state, "owner").await;
    let p = prepare(&state, &work).await.unwrap();
    let before = node(&state, &id).await;
    let total = count(&state, "SELECT count(*) FROM config_versions").await;
    sqlx::raw_sql("CREATE FUNCTION reject_node_metadata() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-node-publication-metadata' AND NEW.state='complete' THEN RAISE EXCEPTION 'confirmation rejected'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_node_metadata BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_node_metadata();").execute(&state.pool).await.unwrap();
    assert!(commit(&state, &work, p.clone()).await.is_err());
    assert!(node(&state, &id).await == before);
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        total
    );
    sqlx::raw_sql("DROP TRIGGER reject_node_metadata ON background_work; CREATE SEQUENCE metadata_attempts; CREATE FUNCTION retry_node_metadata() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-node-publication-metadata' AND NEW.state='complete' AND nextval('metadata_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_node_metadata BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_node_metadata();").execute(&state.pool).await.unwrap();
    let plan = candidate(p.clone());
    commit(&state, &work, p).await.unwrap();
    assert_eq!(
        count(&state, "SELECT last_value FROM metadata_attempts").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        total + 2
    );
    for h in [
        &plan
            .changes
            .as_ref()
            .unwrap()
            .live
            .as_ref()
            .unwrap()
            .history,
        plan.changes
            .as_ref()
            .unwrap()
            .desired_history
            .as_ref()
            .unwrap(),
    ] {
        let cipher: String =
            sqlx::query_scalar("SELECT config_enc FROM config_versions WHERE id=$1")
                .bind(&h.id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(cipher, h.cipher);
    }
}

#[tokio::test]
async fn takeover_rejects_old_prepared_results_and_source_edits_require_fresh_preparation() {
    let (state, _, _, id) = fixture(false).await;
    let old = claim(&state, "old").await;
    let p = prepare(&state, &old).await.unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = claim(&state, "next").await;
    assert!(commit(&state, &old, p).await.is_err());
    let p = prepare(&state, &next).await.unwrap();
    let _ = crate::api::nodes::patch(
        axum::extract::State(state.clone()),
        axum::extract::Path(id.clone()),
        axum::Json(
            serde_json::from_value(json!({"expected_revision":1,"name":"Concurrent edit"}))
                .unwrap(),
        ),
    )
    .await
    .unwrap();
    commit(&state, &next, p).await.unwrap();
    assert_eq!(node(&state, &id).await.desired_revision, 2);
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-node-publication-metadata' AND state='queued' AND error_code='node_metadata_preparation_required'").await,1);
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1")
        .bind(KIND)
        .execute(&state.pool)
        .await
        .unwrap();
    let fresh = claim(&state, "fresh").await;
    let p = prepare(&state, &fresh).await.unwrap();
    commit(&state, &fresh, p).await.unwrap();
    assert_eq!(node(&state, &id).await.desired_revision, 4);
    let name: String = sqlx::query_scalar("SELECT name FROM nodes WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(name, "Concurrent edit");
}

#[tokio::test]
async fn removed_archived_expired_or_superseded_sources_cannot_publish_prepared_node_metadata() {
    for change in [
        "UPDATE nodes SET state='deleting'",
        "UPDATE nodes SET state='delete_failed'",
        "UPDATE credentials SET archived=true WHERE kind='ca_identity'",
        "UPDATE mtls_rotations SET outcome='superseded_expired_publication',phase='complete'",
        "UPDATE mtls_ca_generations SET expires_at=clock_timestamp()-interval '1 second' WHERE ca_version=2",
    ] {
        let (state, _, _, id) = fixture(false).await;
        let work = claim(&state, "owner").await;
        let p = prepare(&state, &work).await.unwrap();
        let before = node(&state, &id).await;
        let total = count(&state, "SELECT count(*) FROM config_versions").await;
        sqlx::query(change).execute(&state.pool).await.unwrap();
        commit(&state, &work, p).await.unwrap();
        let after = node(&state, &id).await;
        assert_eq!(after.desired_revision, before.desired_revision);
        assert_eq!(after.desired_config_enc, before.desired_config_enc);
        assert_eq!(
            count(&state, "SELECT count(*) FROM config_versions").await,
            total
        );
    }
}

#[tokio::test]
async fn lost_proof_or_another_candidate_scope_cannot_rewrite_configuration() {
    let (state, _, _, id) = fixture(false).await;
    let work = claim(&state, "owner").await;
    let p = prepare(&state, &work).await.unwrap();
    let before = node(&state, &id).await;
    sqlx::query("UPDATE mtls_probe_versions SET discarded=true WHERE ca_version=2")
        .execute(&state.pool)
        .await
        .unwrap();
    commit(&state, &work, p).await.unwrap();
    assert!(node(&state, &id).await == before);
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE error_code='node_metadata_proof_changed' AND state='complete'").await,1);
    let (state, _, _, id) = fixture(false).await;
    let work = claim(&state, "owner").await;
    let (other, _, _, _) = fixture(false).await;
    let other_work = claim(&other, "other").await;
    let other_p = prepare(&other, &other_work).await.unwrap();
    let before = node(&state, &id).await;
    assert!(commit(&state, &work, other_p).await.is_err());
    assert!(node(&state, &id).await == before);
}

#[tokio::test]
async fn a_new_activation_generation_invalidates_prepared_results_even_with_the_same_active_id() {
    let (state, _, _, id) = fixture(false).await;
    let work = claim(&state, "owner").await;
    let p = prepare(&state, &work).await.unwrap();
    sqlx::query("UPDATE nodes SET mtls_generation=mtls_generation+1 WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    commit(&state, &work, p).await.unwrap();
    assert_eq!(node(&state, &id).await.desired_revision, 1);
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-node-publication-metadata' AND error_code='node_metadata_preparation_required' AND state='queued'").await,1);
}

#[tokio::test]
async fn only_the_node_and_two_new_history_sources_are_flushed_despite_a_large_dirty_history_backlog()
 {
    let (state, _, _, id) = fixture(true).await;
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,deployed_success,created_at) SELECT gen_random_uuid()::text,node_id,i,config_enc,content_sha256,false,created_at FROM config_versions CROSS JOIN generate_series(10,310)i WHERE node_id=$1 AND revision=1").bind(&id).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,deployed_success,created_at) SELECT gen_random_uuid()::text,node_id,400,config_enc,content_sha256,false,created_at FROM config_versions WHERE node_id=$1 AND revision=2").bind(&id).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE nodes SET desired_revision=400 WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    let dirty = count(
        &state,
        "SELECT count(*) FROM credential_reference_dirty WHERE kind='history'",
    )
    .await;
    assert_eq!(dirty, 302);
    let work = claim(&state, "owner").await;
    let p = prepare(&state, &work).await.unwrap();
    commit(&state, &work, p).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_reference_dirty WHERE kind='history'"
        )
        .await,
        dirty
    );
    assert_eq!(node(&state, &id).await.desired_revision, 402);
    assert_eq!(node(&state, &id).await.deployed_revision, Some(401));
}

async fn admission_fixture(n: i32) -> (AppState, String, String) {
    let (state, ca, rotation) = super::super::trust::tests::fixture(n).await;
    sqlx::query("UPDATE mtls_ca_generations SET state='retired' WHERE ca_version=1")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_ca_generations SET state='published',published_at=clock_timestamp() WHERE ca_version=2").execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE mtls_authorities SET published_version=2")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_rotations SET phase='probes',published_at=clock_timestamp()")
        .execute(&state.pool)
        .await
        .unwrap();
    let (r, c) = (rotation.clone(), ca.clone());
    db::write::<_, anyhow::Error, _>(&state.pool, move |tx| {
        let (r, c) = (r.clone(), c.clone());
        Box::pin(async move { enqueue_on(tx, &r, &c, 2).await })
    })
    .await
    .unwrap();
    (state, ca, rotation)
}
async fn claim_admission(state: &AppState) -> Work {
    sqlx::query(
        "UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1 AND state='queued'",
    )
    .bind(ADMIT)
    .execute(&state.pool)
    .await
    .unwrap();
    maintenance::claim(&state.pool, ADMIT, "admit")
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn three_hundred_one_node_work_items_are_admitted_in_bounded_pages_and_healthy_nodes_make_progress()
 {
    let (state, _, rotation) = admission_fixture(301).await;
    for expected in [100, 200, 300, 301] {
        let work = claim_admission(&state).await;
        admit(&state.pool, &work).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-node-publication-metadata'"
            )
            .await,
            expected
        );
    }
    let lease = db::write::<_, anyhow::Error, _>(&state.pool, |tx| {
        Box::pin(async move {
            Ok(
                crate::execution::acquire(tx, "node:member-0001", "busy", "other-node-job")
                    .await?
                    .unwrap(),
            )
        })
    })
    .await
    .unwrap();
    let work = claim(&state, "healthy").await;
    assert_ne!(work.payload["node_id"], "member-0001");
    let p = prepare(&state, &work).await.unwrap();
    assert!(candidate(p.clone()).changes.is_none());
    commit(&state, &work, p).await.unwrap();
    assert!(lease.renew(&state.pool).await.unwrap());
    assert!(pending_on(&state.pool, &rotation).await.unwrap());
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}

#[tokio::test]
async fn node_admission_acknowledgement_failure_rolls_back_the_entire_page_and_native_retry_is_unique()
 {
    let (state, _, _) = admission_fixture(101).await;
    let work = claim_admission(&state).await;
    sqlx::raw_sql("CREATE FUNCTION reject_node_admission() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-node-publication-admission' AND NEW.state<>'running' THEN RAISE EXCEPTION 'confirmation rejected'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_node_admission BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_node_admission();").execute(&state.pool).await.unwrap();
    assert!(admit(&state.pool, &work).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-node-publication-metadata'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_node_admission ON background_work; CREATE SEQUENCE node_admission_attempts; CREATE FUNCTION retry_node_admission() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-node-publication-admission' AND NEW.state<>'running' AND nextval('node_admission_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_node_admission BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_node_admission();").execute(&state.pool).await.unwrap();
    admit(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT last_value FROM node_admission_attempts").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-node-publication-metadata'"
        )
        .await,
        100
    );
}

#[tokio::test]
async fn work_completion_is_idempotent_and_node_deletion_finishes_without_recreating_any_snapshot()
{
    let (state, _, _, id) = fixture(false).await;
    let work = claim(&state, "owner").await;
    let p = prepare(&state, &work).await.unwrap();
    commit(&state, &work, p.clone()).await.unwrap();
    let after = node(&state, &id).await;
    let total = count(&state, "SELECT count(*) FROM config_versions").await;
    assert!(commit(&state, &work, p).await.is_err());
    assert!(node(&state, &id).await == after);
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        total
    );
    let (state, _, _, id) = fixture(false).await;
    let work = claim(&state, "owner").await;
    let p = prepare(&state, &work).await.unwrap();
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    let total = count(&state, "SELECT count(*) FROM config_versions").await;
    commit(&state, &work, p).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM nodes").await, 0);
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        total
    );
}

#[tokio::test]
async fn migration_0058_to_0059_preserves_all_snapshots_publication_state_and_queued_metadata() {
    let pool = db::test_pool_through_version(4, 58).await;
    let (state, _, rotation, node) =
        crate::api::managed_mtls::tests::rotation_flow::publication_fixture_with_pool(pool, true)
            .await;
    sqlx::query("INSERT INTO background_work(id,kind,payload,state) VALUES('legacy-metadata','mtls-node-publication-metadata',jsonb_build_object('rotation_id',$1::text,'node_id',$2::text),'queued')")
        .bind(&rotation).bind(&node).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO resource_leases(resource_key,owner,work_id,generation,expires_at) VALUES($1,'legacy','legacy-metadata',7,clock_timestamp()+interval '1 minute')")
        .bind(format!("node:{node}")).execute(&state.pool).await.unwrap();
    let snapshot = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY id) FROM config_versions v),'activations',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM mtls_activations a),'trust',(SELECT jsonb_agg(to_jsonb(t) ORDER BY node_id,ca_id,ca_version) FROM mtls_node_trust t),'work',(SELECT jsonb_agg(to_jsonb(w) ORDER BY id) FROM background_work w),'leases',(SELECT jsonb_agg(to_jsonb(l)-'epoch' ORDER BY resource_key) FROM resource_leases l))";
    let before: Value = sqlx::query_scalar(snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    for _ in 0..2 {
        db::run_migrations(&state.pool).await.unwrap();
        assert!(
            before
                == sqlx::query_scalar::<_, Value>(snapshot)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap(),
            "metadata status index must preserve every source byte and ownership field"
        );
    }
    assert_eq!(count(&state,"SELECT count(*) FROM pg_indexes WHERE schemaname=current_schema() AND indexname='background_work_rotation_status'").await,1);
}

#[tokio::test]
async fn healthy_metadata_owner_prevents_premature_probe_switch_and_rotation_completion() {
    let (state, _, rotation, id) = fixture(false).await;
    let work = claim(&state, "healthy-metadata-owner").await;
    let p = prepare(&state, &work).await.unwrap();
    super::super::advance(&state, &rotation).await.unwrap();
    let phase: String = sqlx::query_scalar("SELECT phase FROM mtls_rotations WHERE id=$1")
        .bind(&rotation)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(phase, "probes");
    assert!(pending_on(&state.pool, &rotation).await.unwrap());
    assert_eq!(node(&state, &id).await.desired_revision, 1);
    let owner: String =
        sqlx::query_scalar("SELECT owner FROM resource_leases WHERE resource_key=$1")
            .bind(format!("work:{}", work.id))
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(owner, "healthy-metadata-owner");
    commit(&state, &work, p).await.unwrap();
    super::super::advance(&state, &rotation).await.unwrap();
    let phase: String = sqlx::query_scalar("SELECT phase FROM mtls_rotations WHERE id=$1")
        .bind(&rotation)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(phase, "complete");
    assert_eq!(node(&state, &id).await.desired_revision, 2);
}

#[tokio::test]
async fn pending_status_seeks_only_its_rotation_and_keeps_failed_work_pending() {
    let (state, _, rotation) = admission_fixture(0).await;
    let work = claim_admission(&state).await;
    admit(&state.pool, &work).await.unwrap();
    sqlx::query("INSERT INTO background_work(id,kind,payload,state) SELECT 'unrelated-'||i,$1,jsonb_build_object('rotation_id','other-'||i),'complete' FROM generate_series(1,1001)i")
        .bind(KIND).execute(&state.pool).await.unwrap();
    sqlx::query("ANALYZE background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(!pending_on(&state.pool, &rotation).await.unwrap());
    let plan: Value = sqlx::query_scalar("EXPLAIN (ANALYZE,FORMAT JSON) SELECT id FROM background_work WHERE kind=ANY($1::text[]) AND payload->>'rotation_id'=$2 AND cycle_id IS NULL AND node_id IS NULL AND state<>'complete'")
        .bind([ADMIT,KIND]).bind(&rotation).fetch_one(&state.pool).await.unwrap();
    assert!(
        plan.to_string().contains("background_work_rotation_status"),
        "{plan}"
    );
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
    sqlx::query("INSERT INTO background_work(id,kind,payload,state) VALUES('failed-metadata',$1,jsonb_build_object('rotation_id',$2::text),'failed')")
        .bind(KIND).bind(&rotation).execute(&state.pool).await.unwrap();
    assert!(pending_on(&state.pool, &rotation).await.unwrap());
}
