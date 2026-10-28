use super::*;
use crate::{
    security::{SecretBox, token_digest},
    state::AppState,
    traffic::batches,
};
use axum::{
    Json,
    extract::{Path, Query, State},
};

async fn fixture(users: usize, nodes: usize) -> AppState {
    let pool = crate::traffic::batch_tests::fixture(users).await;
    if nodes > 2 {
        sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT 'n-'||lpad(i::text,3,'0'),'Node '||i,'127.0.0.1',22,'root','password','unused','example.test',443,':443','hash-n-'||i,'unused','unused','{}',now(),now() FROM generate_series(1,$1)i")
            .bind((nodes-2) as i32).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) SELECT u.id,n.id,u.id||n.id,'unused',now() FROM users u CROSS JOIN nodes n WHERE n.id LIKE 'n-%'")
            .execute(&pool).await.unwrap();
    }
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(&pool).await.unwrap();
    AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    )
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn restrict(state: &AppState, user: &str) {
    sqlx::query("UPDATE users SET enabled=false WHERE id=$1")
        .bind(user)
        .execute(&state.pool)
        .await
        .unwrap();
    let user = user.to_owned();
    db::write(&state.pool, move |tx| {
        let user = user.clone();
        Box::pin(async move { admit_on(tx, &user).await })
    })
    .await
    .unwrap();
}
async fn claim(state: &AppState, owner: &str) -> maintenance::Work {
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE kind=$1 AND state='queued'").bind(KIND).execute(&state.pool).await.unwrap();
    maintenance::claim(&state.pool, KIND, owner)
        .await
        .unwrap()
        .unwrap()
}
async fn scan_claim(state: &AppState) -> maintenance::Work {
    maintenance::ensure(
        &state.pool,
        "test-access-scan",
        "traffic-maintenance",
        json!({"operation":"access_expiry"}),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE kind='traffic-maintenance'").execute(&state.pool).await.unwrap();
    maintenance::ensure(
        &state.pool,
        "test-access-scan",
        "traffic-maintenance",
        json!({"operation":"access_expiry"}),
    )
    .await
    .unwrap();
    maintenance::claim(&state.pool, "traffic-maintenance", "scanner")
        .await
        .unwrap()
        .unwrap()
}
async fn bill(state: &AppState, id: &str, tx: i64) -> Result<()> {
    batches::reserve(&state.pool, id, "a", None).await?;
    batches::seal(
        &state.pool,
        id,
        "service",
        &serde_json::Map::from_iter([("user-0001".into(), json!({"tx":tx,"rx":0}))]),
    )
    .await?;
    batches::drain(&state.pool, id).await?;
    Ok(())
}
#[tokio::test]
async fn due_user_scans_are_bounded_skip_locked_rows_and_do_not_duplicate_admission() {
    let state = fixture(301, 2).await;
    sqlx::query("UPDATE users SET enabled=mod(split_part(id,'-',2)::int,3)<>0,expires_at=CASE WHEN mod(split_part(id,'-',2)::int,3)=1 THEN clock_timestamp()-interval '1 second' END,quota_bytes=CASE WHEN mod(split_part(id,'-',2)::int,3)=2 THEN 1 END,usage_bytes=CASE WHEN mod(split_part(id,'-',2)::int,3)=2 THEN 1 ELSE 0 END").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO users(id,name,expires_at,created_at,updated_at) VALUES('healthy','Healthy',clock_timestamp()+interval '1 hour',now(),now())").execute(&state.pool).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id='user-0001' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let legacy = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    for expected in [128, 256, 300] {
        let work = scan_claim(&state).await;
        scan_work(&state.pool, &work).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='access-revocation'"
            )
            .await,
            expected
        );
    }
    held.rollback().await.unwrap();
    let work = scan_claim(&state).await;
    scan_work(&state.pool, &work).await.unwrap();
    let work = scan_claim(&state).await;
    scan_work(&state.pool, &work).await.unwrap();
    legacy.rollback().await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='access-revocation'"
        )
        .await,
        301
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM users WHERE access_kick_enqueued_at IS NOT NULL"
        )
        .await,
        301
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM users WHERE id='healthy' AND access_kick_enqueued_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs").await,
        0,
        "scan admits durable work without unbounded node fan-out"
    );
}
#[tokio::test]
async fn wide_user_fanout_recovers_pages_without_duplicates_and_keeps_deleting_node_obligations() {
    let state = fixture(1, 257).await;
    sqlx::query("UPDATE nodes SET state='deleting' WHERE id='n-128'")
        .execute(&state.pool)
        .await
        .unwrap();
    restrict(&state, "user-0001").await;
    let old = claim(&state, "one").await;
    dispatch_work(&state.pool, &old).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM kick_requests").await,
        128
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='access-revocation' AND state='queued'"
        )
        .await,
        1
    );
    let next = claim(&state, "two").await;
    assert_eq!(old.id, next.id);
    assert!(next.leases[0].generation > old.leases[0].generation);
    assert!(dispatch_work(&state.pool, &old).await.is_err());
    dispatch_work(&state.pool, &next).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM kick_requests").await,
        256
    );
    let next = claim(&state, "three").await;
    dispatch_work(&state.pool, &next).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM kick_requests").await,
        257
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
        256
    );
    assert_eq!(count(&state,"SELECT count(*) FROM kick_requests WHERE node_id='n-128' AND state='active' AND latest_job_id IS NULL").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM kick_requests WHERE generation<>1"
        )
        .await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='access-revocation' AND state='complete'").await,1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
}
#[tokio::test]
async fn billing_rolls_back_usage_checkpoint_and_marker_when_durable_revocation_cannot_be_saved() {
    let state = fixture(1, 2).await;
    sqlx::query("UPDATE users SET quota_bytes=5")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_access_work() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='access-revocation' THEN RAISE EXCEPTION 'reject durable revocation'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_access_work BEFORE INSERT ON background_work FOR EACH ROW EXECUTE FUNCTION reject_access_work();").execute(&state.pool).await.unwrap();
    assert!(bill(&state, "quota", 10).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM users WHERE usage_bytes=0 AND access_kick_enqueued_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM traffic_records").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM traffic_baselines").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM traffic_sample_items WHERE processed_at IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT processed_count FROM traffic_sample_batches WHERE sample_id='quota'"
        )
        .await,
        0
    );
    sqlx::query("DROP TRIGGER reject_access_work ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    batches::drain(&state.pool, "quota").await.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM users WHERE usage_bytes=10 AND access_kick_enqueued_at IS NOT NULL").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='access-revocation'"
        )
        .await,
        1
    );
    drain_for_test(&state.pool).await;
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
        2
    );
}
#[tokio::test]
async fn fanout_retries_native_serialization_with_one_set_of_jobs_events_and_cursor() {
    let state = fixture(1, 2).await;
    restrict(&state, "user-0001").await;
    let work = claim(&state, "one").await;
    sqlx::raw_sql("CREATE SEQUENCE access_ack_attempt; CREATE FUNCTION retry_access_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='access-revocation' AND NEW.state='complete' AND nextval('access_ack_attempt')=1 THEN RAISE EXCEPTION 'retry access acknowledgement' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_access_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_access_ack();").execute(&state.pool).await.unwrap();
    dispatch_work(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT last_value FROM access_ack_attempt").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM job_events WHERE event_type='job.queued'"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM kick_requests WHERE generation=1"
        )
        .await,
        2
    );
    let cursor: Option<String> = sqlx::query_scalar(
        "SELECT payload->>'after_node' FROM background_work WHERE kind='access-revocation'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(cursor.as_deref(), Some("b"));
}
#[tokio::test]
async fn renewal_and_a_new_quota_restriction_supersede_the_old_cursor_and_keep_permanent_reasons() {
    let state = fixture(1, 129).await;
    restrict(&state, "user-0001").await;
    let old = claim(&state, "old").await;
    dispatch_work(&state.pool, &old).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    crate::api::enqueue_job_with_payload_in_tx(
        &mut tx,
        "kick",
        Some("a"),
        None,
        json!({"user_id":"user-0001","kick_reason":"credentials_revoked"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let _ = crate::api::users::patch(
        State(state.clone()),
        Path("user-0001".into()),
        Json(serde_json::from_value(json!({"expected_revision":1,"enabled":true})).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM users WHERE access_kick_enqueued_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM kick_requests WHERE reasons ? 'access_restricted'"
        )
        .await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM kick_requests WHERE state='active' AND reasons ? 'credentials_revoked'").await,1);
    let _ = crate::api::users::patch(
        State(state.clone()),
        Path("user-0001".into()),
        Json(serde_json::from_value(json!({"expected_revision":2,"quota_bytes":1})).unwrap()),
    )
    .await
    .unwrap();
    bill(&state, "new-quota", 2).await.unwrap();
    // The older queued page remains first. It must acknowledge supersession
    // before the new marker's work can continue, without visiting the old tail.
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE id=$1")
        .bind(&old.id)
        .execute(&state.pool)
        .await
        .unwrap();
    let stale = maintenance::claim(&state.pool, KIND, "stale")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale.id, old.id);
    dispatch_work(&state.pool, &stale).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE node_id='n-127'").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE error_code='restriction_superseded'"
        )
        .await,
        1
    );
    let new = claim(&state, "new").await;
    assert_ne!(new.id, old.id);
    assert_ne!(
        new.payload["restriction_marker"],
        old.payload["restriction_marker"]
    );
    dispatch_work(&state.pool, &new).await.unwrap();
    drain_for_test(&state.pool).await;
    assert_eq!(count(&state,"SELECT count(*) FROM kick_requests WHERE state='active' AND reasons ? 'access_restricted'").await,129);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE kind='kick' AND status='queued'"
        )
        .await,
        129
    );
}
#[tokio::test]
async fn user_deletion_keeps_explicit_revocations_and_does_not_resume_a_stale_user_page() {
    let state = fixture(1, 2).await;
    restrict(&state, "user-0001").await;
    let _ = crate::api::users::delete(
        State(state.clone()),
        Path("user-0001".into()),
        Query(crate::api::nodes::RevisionQuery {
            expected_revision: Some(1),
        }),
    )
    .await
    .unwrap();
    drain_for_test(&state.pool).await;
    assert_eq!(count(&state, "SELECT count(*) FROM users").await, 0);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM kick_requests WHERE state='active' AND reasons ? 'user_deleted'"
        )
        .await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE error_code='restriction_superseded'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn a_busy_parent_defers_only_its_user_without_skipping_the_node_cursor() {
    let state = fixture(2, 2).await;
    sqlx::query("DELETE FROM node_assignments WHERE user_id='user-0002' AND node_id='a'")
        .execute(&state.pool)
        .await
        .unwrap();
    restrict(&state, "user-0001").await;
    restrict(&state, "user-0002").await;
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1 AND payload->>'user_id'='user-0001'").bind(KIND).execute(&state.pool).await.unwrap();
    let old = maintenance::claim(&state.pool, KIND, "old")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.payload["user_id"], "user-0001");
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='a' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    assert!(dispatch_work(&state.pool, &old).await.is_err());
    old.retry(&state.pool).await.unwrap();
    let other = maintenance::claim(&state.pool, KIND, "other")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other.payload["user_id"], "user-0002");
    dispatch_work(&state.pool, &other).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE node_id='b' AND payload_json->>'user_id'='user-0002'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM kick_requests WHERE user_id='user-0001'"
        )
        .await,
        0
    );
    held.rollback().await.unwrap();
    drain_for_test(&state.pool).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM kick_requests WHERE user_id='user-0001'"
        )
        .await,
        2
    );
}
#[tokio::test]
async fn scan_ack_failure_rolls_back_all_markers_and_durable_children() {
    let state = fixture(3, 2).await;
    sqlx::query("UPDATE users SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    let work = scan_claim(&state).await;
    sqlx::raw_sql("CREATE FUNCTION reject_access_scan_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='traffic-maintenance' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject scan acknowledgement'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_access_scan_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_access_scan_ack();").execute(&state.pool).await.unwrap();
    assert!(scan_work(&state.pool, &work).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM users WHERE access_kick_enqueued_at IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='access-revocation'"
        )
        .await,
        0
    );
    sqlx::query("DROP TRIGGER reject_access_scan_ack ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    scan_work(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM users WHERE access_kick_enqueued_at IS NOT NULL"
        )
        .await,
        3
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='access-revocation'"
        )
        .await,
        3
    );
}
#[tokio::test]
async fn authoritative_auth_rejects_quota_before_fanout_while_monitoring_credentials_still_work() {
    let state = fixture(1, 2).await;
    sqlx::query("UPDATE nodes SET node_token_hash=$1 WHERE id='a'")
        .bind(token_digest("node-token"))
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE node_assignments SET credential_hash=$1 WHERE node_id='a'")
        .bind(token_digest("user-token"))
        .execute(&state.pool)
        .await
        .unwrap();
    let auth = |token: &str| {
        crate::api::subscriptions::hy2_auth(
            State(state.clone()),
            Path(("a".into(), "node-token".into())),
            Json(serde_json::from_value(json!({"addr":"","auth":token,"tx":0})).unwrap()),
        )
    };
    assert_eq!(
        serde_json::to_value(auth("user-token").await.unwrap().0).unwrap()["ok"],
        true
    );
    sqlx::query("UPDATE users SET quota_bytes=5")
        .execute(&state.pool)
        .await
        .unwrap();
    bill(&state, "quota", 10).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='access-revocation' AND state='queued'"
        )
        .await,
        1
    );
    assert_eq!(
        serde_json::to_value(auth("user-token").await.unwrap().0).unwrap()["ok"],
        false
    );
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id='a'")
        .execute(&state.pool)
        .await
        .unwrap();
    crate::proxy_monitor::token_tests::install_fixture(&state, "a", "probe-token").await;
    assert_eq!(
        serde_json::to_value(auth("probe-token").await.unwrap().0).unwrap()["ok"],
        true
    );
}

#[tokio::test]
async fn deleting_the_sampled_node_keeps_the_users_due_revocation_for_other_nodes() {
    let state = fixture(1, 2).await;
    sqlx::query("UPDATE users SET quota_bytes=5")
        .execute(&state.pool)
        .await
        .unwrap();
    bill(&state, "quota", 10).await.unwrap();
    // Model the final database deletion after captured counters were billed.
    // Work is user-owned and must survive the source node's FK cascades.
    sqlx::query("DELETE FROM nodes WHERE id='a'")
        .execute(&state.pool)
        .await
        .unwrap();
    drain_for_test(&state.pool).await;
    assert_eq!(count(&state,"SELECT count(*) FROM users WHERE usage_bytes=10 AND access_kick_enqueued_at IS NOT NULL").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM kick_requests WHERE node_id='b' AND reasons ? 'access_restricted'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE kind='kick' AND node_id='b'"
        )
        .await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='access-revocation' AND state='complete'").await,1);
}

#[tokio::test]
async fn reused_wall_clock_markers_cannot_reactivate_an_old_restriction_page() {
    let state = fixture(1, 129).await;
    restrict(&state, "user-0001").await;
    let old = claim(&state, "old").await;
    dispatch_work(&state.pool, &old).await.unwrap();
    let marker: DateTime<Utc> =
        serde_json::from_value(old.payload["restriction_marker"].clone()).unwrap();
    sqlx::query("UPDATE users SET enabled=true,access_kick_enqueued_at=NULL WHERE id='user-0001'")
        .execute(&state.pool)
        .await
        .unwrap();
    // Model a later restriction whose wall clock exactly equals the older one.
    sqlx::query("UPDATE users SET enabled=false,access_kick_enqueued_at=$1 WHERE id='user-0001'")
        .bind(marker)
        .execute(&state.pool)
        .await
        .unwrap();
    let resumed = claim(&state, "resumed").await;
    dispatch_work(&state.pool, &resumed).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE node_id='n-127'").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE error_code='restriction_superseded'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT generation FROM user_access_restriction_epochs WHERE user_id='user-0001'"
        )
        .await,
        3
    );
    sqlx::query("UPDATE users SET access_kick_enqueued_at=NULL WHERE id='user-0001'")
        .execute(&state.pool)
        .await
        .unwrap();
    restrict(&state, "user-0001").await;
    drain_for_test(&state.pool).await;
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE node_id='n-127'").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT generation FROM user_access_restriction_epochs WHERE user_id='user-0001'"
        )
        .await,
        5
    );
}

#[tokio::test]
async fn maintenance_backfills_the_due_queue_without_changing_existing_user_or_assignment_data() {
    let pool = crate::traffic::batch_tests::fixture(301).await;
    sqlx::query("UPDATE users SET enabled=split_part(id,'-',2)::int<=100,expires_at=CASE WHEN split_part(id,'-',2)::int<=100 THEN clock_timestamp()+interval '1 hour' END,quota_bytes=CASE WHEN split_part(id,'-',2)::int>200 THEN 1 END,usage_bytes=CASE WHEN split_part(id,'-',2)::int>200 THEN 1 ELSE 0 END").execute(&pool).await.unwrap();
    let snapshot = "SELECT jsonb_build_object('users',(SELECT jsonb_agg(to_jsonb(u) ORDER BY id) FROM users u),'nodes',(SELECT jsonb_agg(to_jsonb(n)-'traffic_capture_mode' ORDER BY id) FROM nodes n),'assignments',(SELECT jsonb_agg(to_jsonb(a) ORDER BY user_id,node_id) FROM node_assignments a))";
    let before: serde_json::Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
    db::run_migrations(&pool).await.unwrap();
    assert!(assert_ready(&pool).await.is_err());
    initialize(&pool).await.unwrap();
    initialize(&pool).await.unwrap();
    assert_ready(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, serde_json::Value>(snapshot)
            .fetch_one(&pool)
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_access_checks")
            .fetch_one(&pool)
            .await
            .unwrap(),
        301
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM user_access_checks WHERE next_action_at<=clock_timestamp()"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        201
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM background_work WHERE kind='access-revocation'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
}
#[tokio::test]
async fn source_changes_schedule_checks_and_ordinary_below_quota_usage_does_not_churn_the_due_row()
{
    let state = fixture(1, 2).await;
    assert_eq!(
        count(&state, "SELECT count(*) FROM user_access_checks").await,
        0
    );
    sqlx::query("UPDATE users SET expires_at=clock_timestamp()+interval '1 hour'")
        .execute(&state.pool)
        .await
        .unwrap();
    let version: String = sqlx::query_scalar("SELECT xmin::text FROM user_access_checks")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET usage_bytes=100")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT xmin::text FROM user_access_checks")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        version
    );
    sqlx::query("UPDATE users SET quota_bytes=50")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM user_access_checks WHERE next_action_at<=clock_timestamp()"
        )
        .await,
        1
    );
    let work = scan_claim(&state).await;
    scan_work(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM user_access_checks").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='access-revocation'"
        )
        .await,
        1
    );
    sqlx::query("UPDATE users SET usage_bytes=0,access_kick_enqueued_at=NULL")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM user_access_checks c JOIN users u ON u.id=c.user_id AND c.next_action_at=u.expires_at WHERE c.next_action_at>clock_timestamp()").await,1);
    drain_for_test(&state.pool).await;
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}

#[tokio::test]
async fn an_empty_due_range_uses_the_queue_index_without_scanning_future_users() {
    let state = fixture(1000, 2).await;
    sqlx::query("UPDATE users SET expires_at=clock_timestamp()+interval '1 hour'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("ANALYZE users; ANALYZE user_access_checks;")
        .execute(&state.pool)
        .await
        .unwrap();
    let cutoff: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) {DUE_USERS_SQL}"
    )))
    .bind(cutoff)
    .bind(USER_BATCH)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(
        plan.to_string().contains("user_access_checks_due_idx"),
        "empty due ranges must use the native range index: {plan}"
    );
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
    assert_eq!(
        count(&state, "SELECT count(*) FROM user_access_checks").await,
        1000
    );
}

#[tokio::test]
async fn renewal_waiting_behind_a_page_retries_its_old_snapshot_and_clears_the_new_obligations() {
    let state = fixture(1, 129).await;
    // Give deterministic row-lock barriers headroom under test-host load.
    let mut connections = Vec::new();
    for _ in 0..8 {
        let mut connection = state.pool.acquire().await.unwrap();
        sqlx::query("SET lock_timeout='5s'")
            .execute(&mut *connection)
            .await
            .unwrap();
        connections.push(connection);
    }
    drop(connections);
    restrict(&state, "user-0001").await;
    let first = claim(&state, "first").await;
    dispatch_work(&state.pool, &first).await.unwrap();
    let tail = claim(&state, "tail").await;
    let mut user_gate = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id='user-0001' FOR UPDATE")
        .fetch_one(&mut *user_gate)
        .await
        .unwrap();
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *user_gate)
        .await
        .unwrap();
    let app = state.clone();
    let page = tokio::spawn(async move { dispatch_work(&app.pool, &tail).await });
    let wait_for_blocker = |blocker: i32| {
        let pool = state.pool.clone();
        async move {
            tokio::time::timeout(std::time::Duration::from_secs(2),async {
                loop {
                    let pid:Option<i32>=sqlx::query_scalar("SELECT pid FROM pg_stat_activity WHERE datname=current_database() AND $1=ANY(pg_blocking_pids(pid)) AND pid<>pg_backend_pid() ORDER BY pid LIMIT 1").bind(blocker).fetch_optional(&pool).await.unwrap();
                    if let Some(pid)=pid {break pid;}
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }).await.unwrap()
        }
    };
    // The page now holds its tail node while waiting for the user's row.
    let page_pid = wait_for_blocker(gate_pid).await;
    let app = state.clone();
    let renewal = tokio::spawn(async move {
        crate::api::users::patch(
            State(app),
            Path("user-0001".into()),
            Json(serde_json::from_value(json!({"expected_revision":1,"enabled":true})).unwrap()),
        )
        .await
    });
    // Renewal's SERIALIZABLE snapshot predates the page's new kick row, and
    // its parent-node lock waits behind that page. SSI must refresh the snapshot.
    let _ = wait_for_blocker(page_pid).await;
    user_gate.rollback().await.unwrap();
    page.await.unwrap().unwrap();
    let _ = renewal.await.unwrap().unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM kick_requests WHERE reasons ? 'access_restricted'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE kind='kick' AND status='queued'"
        )
        .await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM users WHERE enabled AND access_kick_enqueued_at IS NULL AND revision=2").await,1);
}
