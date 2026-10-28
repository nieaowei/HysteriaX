use super::ownership_tests::{claim, count, enqueue, fixture, operation};
use super::*;
use crate::execution;

fn app(pool: &PgPool) -> AppState {
    AppState::new(
        pool.clone(),
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    )
}
fn output() -> JobOutput {
    JobOutput {
        stage: "done".into(),
        result: json!({"verified":true}),
        node_state: None,
        deployed_revision: None,
        deployed_config: None,
        deployed_sha256: None,
        published_connection: None,
        delete_node: false,
    }
}
async fn complete(
    state: &AppState,
    claimed: &ClaimedJob,
    out: JobOutput,
) -> Option<Result<(), sqlx::Error>> {
    execution::supervise(
        state.pool.clone(),
        claimed.leases.clone(),
        succeed(state, &claimed.job, out),
    )
    .await
}
async fn snapshot(pool: &PgPool) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('jobs',(SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j),'nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM job_events e))").fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn success_uses_database_time_without_waiting_for_unrelated_jobs() {
    let pool = fixture().await;
    let state = app(&pool);
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    enqueue(&pool, "two", Some("node-b"), None).await;
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id='two' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), complete(&state, &one, output()))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(sqlx::query_scalar::<_,bool>("SELECT status='succeeded' AND finished_at=updated_at AND updated_at=(SELECT created_at FROM job_events WHERE job_id='one' AND event_type='job.succeeded') FROM jobs WHERE id='one'").fetch_one(&pool).await.unwrap());
    held.rollback().await.unwrap();
}

#[tokio::test]
async fn event_failure_rolls_back_success_and_native_retry_has_one_receipt_event() {
    let pool = fixture().await;
    let state = app(&pool);
    enqueue(&pool, "one", None, None).await;
    let one = claim(&pool, "owner").await;
    let before = snapshot(&pool).await;
    sqlx::raw_sql("CREATE FUNCTION reject_success() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.succeeded' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_success BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_success();").execute(&pool).await.unwrap();
    assert!(complete(&state, &one, output()).await.unwrap().is_err());
    assert_eq!(before, snapshot(&pool).await);
    sqlx::raw_sql("DROP TRIGGER reject_success ON job_events; CREATE SEQUENCE success_attempts; CREATE FUNCTION retry_success() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.succeeded' AND nextval('success_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='retry'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_success BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION retry_success();").execute(&pool).await.unwrap();
    complete(&state, &one, output()).await.unwrap().unwrap();
    assert_eq!(
        count(&pool, "SELECT last_value FROM success_attempts").await,
        2
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.succeeded'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn wrong_scope_or_changed_claimed_source_cannot_publish_success() {
    let pool = fixture().await;
    let state = app(&pool);
    enqueue(&pool, "one", Some("node-a"), Some("dns-zone:one")).await;
    let one = claim(&pool, "owner").await;
    let before = snapshot(&pool).await;
    for mode in 0..6 {
        let mut job = one.job.clone();
        match mode {
            0 => job.id = "other".into(),
            1 => job.kind = "deploy".into(),
            2 => job.node_id = Some("node-b".into()),
            3 => job.target_revision = Some(3),
            4 => job.payload = json!({"changed":true}),
            _ => job.attempts += 1,
        }
        assert!(
            execution::supervise(
                pool.clone(),
                one.leases.clone(),
                succeed(&state, &job, output())
            )
            .await
            .unwrap()
            .is_err()
        );
        assert_eq!(before, snapshot(&pool).await);
    }
    let mut missing = one.leases.clone();
    missing.retain(|l| !l.resource.starts_with("dns-zone:"));
    assert!(
        execution::supervise(pool.clone(), missing, succeed(&state, &one.job, output()))
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
}

#[tokio::test]
async fn unknown_remote_result_becomes_reconciliation_instead_of_false_success_or_node_publication()
{
    let pool = fixture().await;
    let state = app(&pool);
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    let op = operation(&pool, &one).await;
    let nodes: Value = sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n")
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut out = output();
    out.node_state = Some("deployed".into());
    complete(&state, &one, out).await.unwrap().unwrap();
    assert_eq!(
        nodes,
        sqlx::query_scalar::<_, Value>("SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n")
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE status='failed' AND stage='reconciling'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.succeeded'"
        )
        .await,
        0
    );
    finish_execution(&pool, &one.job, &one.leases)
        .await
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM resource_leases WHERE work_id='one' AND blocked"
        )
        .await,
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM execution_operations WHERE operation_id=$1"
        )
        .bind(op)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "reconciling"
    );
}

#[tokio::test]
async fn takeover_and_cancellation_stop_late_success_before_any_result_write() {
    let pool = fixture().await;
    let state = app(&pool);
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "old").await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let mut next = vec![];
    for l in &one.leases {
        next.push(
            execution::acquire(&mut tx, &l.resource, "next", "one")
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    let before = snapshot(&pool).await;
    assert!(complete(&state, &one, output()).await.is_none());
    assert_eq!(before, snapshot(&pool).await);
    sqlx::query("UPDATE jobs SET status='cancelled' WHERE id='one'")
        .execute(&pool)
        .await
        .unwrap();
    let before = snapshot(&pool).await;
    assert!(
        execution::supervise(pool.clone(), next, succeed(&state, &one.job, output()))
            .await
            .is_none()
    );
    assert_eq!(before, snapshot(&pool).await);
}

#[tokio::test]
async fn incomplete_or_wrong_deployment_target_cannot_publish_a_connection_or_delete_node() {
    let pool = fixture().await;
    let state = app(&pool);
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    let before = snapshot(&pool).await;
    for mode in 0..4 {
        let mut out = output();
        match mode {
            0 => out.published_connection = Some(json!({"public_host":"wrong"})),
            1 => out.deployed_revision = Some(2),
            2 => out.deployed_config = Some("cipher".into()),
            _ => out.delete_node = true,
        }
        assert!(complete(&state, &one, out).await.unwrap().is_err());
        assert_eq!(before, snapshot(&pool).await);
    }
}

#[tokio::test]
async fn verified_deployment_retries_atomically_preserving_ciphertext_and_a_later_desired_edit() {
    let pool = fixture().await;
    let state = app(&pool);
    let cipher = state.secrets.encrypt("{}").unwrap();
    sqlx::query("UPDATE nodes SET desired_config_enc=$1 WHERE id='node-a'")
        .bind(&cipher)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES('history','node-a',1,$1,'digest',clock_timestamp())").bind(&cipher).execute(&pool).await.unwrap();
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    sqlx::query(
        "UPDATE nodes SET desired_revision=2,public_host='future.example.test' WHERE id='node-a'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let mut out = output();
    out.node_state = Some("deployed".into());
    out.deployed_revision = Some(1);
    out.deployed_config = Some(cipher.clone());
    out.deployed_sha256 = Some("digest".into());
    out.published_connection = Some(json!({"public_host":"executed.example.test"}));
    let before = snapshot(&pool).await;
    sqlx::raw_sql("CREATE FUNCTION reject_deployed_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.succeeded' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_deployed_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_deployed_event();").execute(&pool).await.unwrap();
    assert!(complete(&state, &one, out.clone()).await.unwrap().is_err());
    assert_eq!(before, snapshot(&pool).await);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM config_versions WHERE deployed_success"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_deployed_event ON job_events; CREATE SEQUENCE deployed_attempts; CREATE FUNCTION retry_deployed_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.succeeded' AND nextval('deployed_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='retry'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_deployed_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION retry_deployed_event();").execute(&pool).await.unwrap();
    complete(&state, &one, out).await.unwrap().unwrap();
    let node=sqlx::query("SELECT desired_revision,deployed_revision,public_host,deployed_config_enc,published_connection FROM nodes WHERE id='node-a'").fetch_one(&pool).await.unwrap();
    assert_eq!(node.get::<i64, _>("desired_revision"), 2);
    assert_eq!(node.get::<i64, _>("deployed_revision"), 1);
    assert_eq!(node.get::<String, _>("public_host"), "future.example.test");
    assert_eq!(node.get::<String, _>("deployed_config_enc"), cipher);
    assert_eq!(
        node.get::<Value, _>("published_connection"),
        json!({"public_host":"executed.example.test"})
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM config_versions WHERE deployed_success"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.succeeded'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT last_value FROM deployed_attempts").await,
        2
    );
    assert_eq!(count(&pool,"SELECT count(*) FROM credential_reference_dirty WHERE (kind='node' AND source_id='node-a') OR (kind='history' AND source_id='history')").await,0);
}

#[tokio::test]
async fn uncertain_uninstall_does_not_purge_history_or_remove_the_node_before_reconciliation() {
    let pool = fixture().await;
    let state = app(&pool);
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('user','User',clock_timestamp(),clock_timestamp())").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) VALUES('history','node-a','user','remote',0,0,10,20,clock_timestamp())").execute(&pool).await.unwrap();
    enqueue(&pool, "one", Some("node-a"), None).await;
    sqlx::query("UPDATE jobs SET kind='uninstall' WHERE id='one'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET state='deleting' WHERE id='node-a'")
        .execute(&pool)
        .await
        .unwrap();
    let one = claim(&pool, "owner").await;
    operation(&pool, &one).await;
    let mut out = output();
    out.delete_node = true;
    out.stage = "uninstalled".into();
    complete(&state, &one, out).await.unwrap().unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM traffic_records WHERE id='history'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM nodes WHERE id='node-a' AND state='deleting'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE id='one' AND stage='reconciling'"
        )
        .await,
        1
    );
    assert_eq!(count(&pool,"SELECT count(*) FROM audit_records WHERE action IN ('node.deleted','node.record_removed')").await,0);
}

#[tokio::test]
async fn a_native_conflict_inside_dns_cleanup_retries_the_whole_completion_without_duplicate_audit_or_jobs()
 {
    let (state, ssh, _, _) = crate::dns::tests::fixture().await;
    let (node, records) = crate::dns::tests::allocated_node(&state, &ssh).await;
    sqlx::query("UPDATE nodes SET state='deleting',deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    crate::api::enqueue_job_in_tx(&mut tx, "uninstall", Some(&node), Some(1))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let claimed = claim(&state.pool, "uninstall-owner").await;
    // This error travels through ApiError in the nested enqueue helper. The
    // completion boundary must preserve its retry classification until rollback.
    sqlx::raw_sql("CREATE SEQUENCE dns_cleanup_attempts; CREATE FUNCTION retry_dns_cleanup_enqueue() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.queued' AND NEW.payload_json->>'kind'='dns-record-delete' AND nextval('dns_cleanup_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='retry nested cleanup'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_dns_cleanup_enqueue BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION retry_dns_cleanup_enqueue();").execute(&state.pool).await.unwrap();
    let mut out = output();
    out.delete_node = true;
    out.stage = "uninstalled".into();
    complete(&state, &claimed, out).await.unwrap().unwrap();
    assert_eq!(
        count(
            &state.pool,
            "SELECT count(*) FROM nodes WHERE state='deleting'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state.pool,
            "SELECT count(*) FROM audit_records WHERE action='node.deleted'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state.pool,
            "SELECT count(*) FROM jobs WHERE kind='dns-record-delete' AND status='queued'"
        )
        .await,
        records.len() as i64
    );
    assert_eq!(count(&state.pool,"SELECT count(*) FROM dns_operations o JOIN dns_records r ON r.id=o.resource_id WHERE o.action='record-delete' AND o.payload->'source_revision'=to_jsonb(r.revision)").await,records.len() as i64);
    assert_eq!(
        count(&state.pool, "SELECT last_value FROM dns_cleanup_attempts").await,
        records.len() as i64 + 1
    );
    assert_eq!(
        count(
            &state.pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.succeeded'"
        )
        .await,
        1
    );
}
