use super::*;
use crate::{background::maintenance, execution, security::SecretBox};

async fn fixture() -> AppState {
    let pool = db::test_pool_with_max_connections(8).await;
    let state = AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    for id in ["a", "b"] {
        sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES($1,$1,'127.0.0.1',22,'root','password','unused','example.test',443,':443',$1,'unused','unused','{}',now(),now())")
            .bind(id).execute(&state.pool).await.unwrap();
    }
    state
}
async fn number(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn claim(state: &AppState, operation: &str, owner: &str) -> maintenance::Work {
    let key = format!("jobs:{operation}");
    maintenance::ensure(
        &state.pool,
        &key,
        "jobs-maintenance",
        json!({"operation":operation}),
    )
    .await
    .unwrap();
    // An isolated test controls successive due times, without wall-clock sleeps.
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE kind='jobs-maintenance'").execute(&state.pool).await.unwrap();
    maintenance::ensure(
        &state.pool,
        &key,
        "jobs-maintenance",
        json!({"operation":operation}),
    )
    .await
    .unwrap();
    maintenance::claim(&state.pool, "jobs-maintenance", owner)
        .await
        .unwrap()
        .unwrap()
}
async fn running(state: &AppState, id: &str, kind: &str) {
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,attempts,available_at,created_at,updated_at) VALUES($1,$2,'running','working',2,now(),now(),now())").bind(id).bind(kind).execute(&state.pool).await.unwrap();
}
#[tokio::test]
async fn expired_execution_recovery_is_bounded_skips_locked_jobs_and_keeps_live_owners() {
    let state = fixture().await;
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,attempts,available_at,created_at,updated_at) SELECT 'db-'||lpad(i::text,3,'0'),'record-remove','running','working',2,now(),now(),now() FROM generate_series(1,301)i").execute(&state.pool).await.unwrap();
    running(&state, "live", "record-remove").await;
    sqlx::query("INSERT INTO resource_leases(resource_key,owner,work_id,generation,expires_at) VALUES('job:live','healthy','live',1,clock_timestamp()+interval '1 hour')").execute(&state.pool).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id='db-001' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let old = claim(&state, "recovery", "one").await;
    maintenance_work(&state, &old).await.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM jobs WHERE status='queued'").await,
        256
    );
    assert!(maintenance_work(&state, &old).await.is_err());
    let next = claim(&state, "recovery", "two").await;
    maintenance_work(&state, &next).await.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM jobs WHERE status='queued'").await,
        300
    );
    held.rollback().await.unwrap();
    let next = claim(&state, "recovery", "three").await;
    maintenance_work(&state, &next).await.unwrap();
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM jobs WHERE status='queued' AND attempts=2"
        )
        .await,
        301
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM jobs WHERE id='live' AND status='running'"
        )
        .await,
        1
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM job_events WHERE event_type='job.recovered'"
        )
        .await,
        301
    );
}
#[tokio::test]
async fn recovery_and_acknowledgement_roll_back_together_and_reject_replaced_owners() {
    let state = fixture().await;
    running(&state, "database-work", "record-remove").await;
    let old = claim(&state, "recovery", "old").await;
    sqlx::raw_sql("CREATE FUNCTION reject_job_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state='complete' THEN RAISE EXCEPTION 'reject maintenance acknowledgement'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_job_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_job_ack();").execute(&state.pool).await.unwrap();
    assert!(maintenance_work(&state, &old).await.is_err());
    assert_eq!(
        number(&state, "SELECT count(*) FROM jobs WHERE status='running'").await,
        1
    );
    assert_eq!(number(&state, "SELECT count(*) FROM job_events").await, 0);
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM background_work WHERE state='running'"
        )
        .await,
        1
    );
    sqlx::query("DROP TRIGGER reject_job_ack ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    let new = maintenance::claim(&state.pool, "jobs-maintenance", "new")
        .await
        .unwrap()
        .unwrap();
    assert!(maintenance_work(&state, &old).await.is_err());
    assert_eq!(
        number(&state, "SELECT count(*) FROM jobs WHERE status='running'").await,
        1
    );
    maintenance_work(&state, &new).await.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM jobs WHERE status='queued'").await,
        1
    );
    assert_eq!(number(&state, "SELECT count(*) FROM job_events").await, 1);
}
#[tokio::test]
async fn claimed_recovery_quarantines_uncertain_and_legacy_remote_work_without_replaying() {
    let state = fixture().await;
    running(&state, "uncertain", "dns-record-create").await;
    running(&state, "legacy", "deploy").await;
    running(&state, "database", "record-remove").await;
    sqlx::query("UPDATE jobs SET resource_key='dns:zone' WHERE id='uncertain'")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let old = execution::acquire(&mut tx, "dns:zone", "old", "uncertain")
        .await
        .unwrap()
        .unwrap();
    old.dispatch(&mut tx, "create", &json!({"digest":"test"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    let work = claim(&state, "recovery", "recovery").await;
    maintenance_work(&state, &work).await.unwrap();
    assert_eq!(number(&state,"SELECT count(*) FROM jobs WHERE id='uncertain' AND status='failed' AND stage='reconciling'").await,1);
    assert_eq!(number(&state,"SELECT count(*) FROM jobs WHERE id='legacy' AND status='failed' AND stage='needs_attention'").await,1);
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM jobs WHERE id='database' AND status='queued'"
        )
        .await,
        1
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM execution_operations WHERE state='reconciling'"
        )
        .await,
        1
    );
    assert_eq!(number(&state,"SELECT count(*) FROM resource_leases WHERE resource_key IN ('dns:zone','job:uncertain','job:legacy') AND blocked").await,3);
    let mut tx = state.pool.begin().await.unwrap();
    assert!(
        execution::acquire(&mut tx, "dns:zone", "new", "new-work")
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
    let next = claim(&state, "recovery", "next").await;
    maintenance_work(&state, &next).await.unwrap();
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM job_events WHERE event_type='job.reconciliation_required'"
        )
        .await,
        2
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM execution_operations").await,
        1
    );
}
#[tokio::test]
async fn kick_pages_skip_busy_nodes_and_obligations_and_resume_the_persisted_cursor() {
    let state = fixture().await;
    sqlx::query("INSERT INTO kick_requests(node_id,user_id,reasons,state) SELECT 'a','user-'||lpad(i::text,3,'0'),'{\"user_deleted\":true}','active' FROM generate_series(1,205)i").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO kick_requests(node_id,user_id,reasons,state) VALUES('b','deleted-user','{\"credentials_revoked\":true}','active')").execute(&state.pool).await.unwrap();
    let mut node = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='a' FOR UPDATE")
        .fetch_one(&mut *node)
        .await
        .unwrap();
    let work = claim(&state, "kick_schedule", "one").await;
    maintenance_work(&state, &work).await.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM jobs WHERE node_id='b'").await,
        1
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM jobs WHERE node_id='a'").await,
        0
    );
    let cursor: Option<String> = sqlx::query_scalar(
        "SELECT payload->>'node_after' FROM background_work WHERE kind='jobs-maintenance'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(cursor.as_deref(), Some("b"));
    node.rollback().await.unwrap();
    let mut obligations = state.pool.begin().await.unwrap();
    sqlx::query(
        "SELECT user_id FROM kick_requests WHERE node_id='a' AND user_id<='user-100' FOR UPDATE",
    )
    .fetch_all(&mut *obligations)
    .await
    .unwrap();
    let wrap = claim(&state, "kick_schedule", "wrap").await;
    maintenance_work(&state, &wrap).await.unwrap();
    let work = claim(&state, "kick_schedule", "two").await;
    maintenance_work(&state, &work).await.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM jobs WHERE node_id='a'").await,
        100,
        "locked first 100 obligations must be skipped before LIMIT"
    );
    obligations.rollback().await.unwrap();
    for _ in 0..8 {
        let work = claim(&state, "kick_schedule", "later").await;
        maintenance_work(&state, &work).await.unwrap();
    }
    assert_eq!(
        number(&state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
        206
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM job_events WHERE event_type='job.queued'"
        )
        .await,
        206
    );
    assert_eq!(number(&state,"SELECT count(*) FROM kick_requests r JOIN jobs j ON j.id=r.latest_job_id AND j.node_id=r.node_id AND j.payload_json->>'user_id'=r.user_id").await,206);
}
#[tokio::test]
async fn kick_schedule_retry_commits_one_child_event_pointer_and_cursor() {
    let state = fixture().await;
    sqlx::query("INSERT INTO kick_requests(node_id,user_id,reasons,state) VALUES('a','deleted','{\"user_deleted\":true}','active')").execute(&state.pool).await.unwrap();
    let work = claim(&state, "kick_schedule", "one").await;
    sqlx::raw_sql("CREATE SEQUENCE job_ack_attempt; CREATE FUNCTION retry_job_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state='complete' AND nextval('job_ack_attempt')=1 THEN RAISE EXCEPTION 'retry acknowledgement' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_job_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_job_ack();").execute(&state.pool).await.unwrap();
    maintenance_work(&state, &work).await.unwrap();
    assert_eq!(
        number(&state, "SELECT last_value FROM job_ack_attempt").await,
        2
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
        1
    );
    assert_eq!(number(&state, "SELECT count(*) FROM job_events").await, 1);
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM kick_requests r JOIN jobs j ON j.id=r.latest_job_id"
        )
        .await,
        1
    );
    let cursor: Option<String> =
        sqlx::query_scalar("SELECT payload->>'node_after' FROM background_work")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(cursor.as_deref(), Some("a"));
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM background_work WHERE state='complete'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn trusted_node_recovery_uses_a_pure_retry_transaction() {
    let state = fixture().await;
    sqlx::query("INSERT INTO kick_requests(node_id,user_id,reasons,state) VALUES('a','deleted','{\"user_deleted\":true}','waiting_recovery'),('a','permanent','{\"user_deleted\":true}','needs_attention')").execute(&state.pool).await.unwrap();
    sqlx::raw_sql("CREATE SEQUENCE node_recovery_attempt; CREATE FUNCTION retry_node_recovery() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='kick' AND nextval('node_recovery_attempt')=1 THEN RAISE EXCEPTION 'retry recovery' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_node_recovery BEFORE INSERT ON jobs FOR EACH ROW EXECUTE FUNCTION retry_node_recovery();").execute(&state.pool).await.unwrap();
    crate::kick_requests::resume_node(&state.pool, "a")
        .await
        .unwrap();
    assert_eq!(
        number(&state, "SELECT last_value FROM node_recovery_attempt").await,
        2
    );
    assert_eq!(number(&state, "SELECT count(*) FROM jobs").await, 1);
    assert_eq!(number(&state,"SELECT count(*) FROM kick_requests WHERE user_id='permanent' AND state='needs_attention' AND latest_job_id IS NULL").await,1);
}

#[tokio::test]
async fn concurrent_trusted_recovery_schedules_one_active_child_without_legacy_serialization() {
    let state = fixture().await;
    sqlx::query("INSERT INTO kick_requests(node_id,user_id,reasons,state) VALUES('a','deleted','{\"user_deleted\":true}','waiting_recovery')").execute(&state.pool).await.unwrap();
    let (a, b) = tokio::join!(
        crate::kick_requests::resume_node(&state.pool, "a"),
        crate::kick_requests::resume_node(&state.pool, "a")
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
        1
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM job_events WHERE event_type='job.queued'"
        )
        .await,
        1
    );
    assert_eq!(number(&state,"SELECT count(*) FROM kick_requests r JOIN jobs j ON j.id=r.latest_job_id WHERE r.generation=1 AND r.state='active' AND j.payload_json->>'kick_generation'='1'").await,1);
}
