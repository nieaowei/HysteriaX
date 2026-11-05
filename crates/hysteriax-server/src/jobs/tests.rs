use serde_json::json;
use sqlx::{PgPool, Row};

use super::{JobInput, recover_interrupted, safe_error};
use crate::security::SecretBox;

async fn test_pool() -> PgPool {
    crate::db::test_pool().await
}

async fn insert_running_job(pool: &PgPool) {
    sqlx::query("INSERT INTO jobs (id, kind, status, stage, available_at, created_at, updated_at, started_at, attempts) VALUES ('job-running', 'sync', 'running', 'installing', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 2)")
            .execute(pool)
            .await
            .unwrap();
}

#[tokio::test]
async fn live_instances_keep_ownership_and_expiry_recovers_only_database_work() {
    let pool = crate::db::test_pool_with_max_connections(4).await;
    sqlx::query("INSERT INTO jobs(id,kind,resource_key,status,stage,available_at,created_at,updated_at) VALUES('one','sync','zone:a','queued','queued',now(),now(),now()),('two','sync','zone:a','queued','queued',now(),now(),now()),('three','sync','zone:b','queued','queued',now(),now(),now())")
            .execute(&pool).await.unwrap();
    let first = super::claim_next_for(&pool, Some("instance-a"))
        .await
        .unwrap()
        .unwrap();
    let second = super::claim_next_for(&pool, Some("instance-b"))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.job.id, second.job.id);
    assert!(
        super::claim_next_for(&pool, Some("instance-c"))
            .await
            .unwrap()
            .is_none()
    );
    recover_interrupted(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE status='running'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1")
            .bind(&first.job.id).execute(&pool).await.unwrap();
    recover_interrupted(&pool).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
        .bind(&first.job.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "queued");
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
        .bind(&second.job.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "running");
    assert!(
        super::claim_next_for(&pool, Some("instance-c"))
            .await
            .unwrap()
            .is_some()
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(first.leases[0].guard(&mut tx).await.is_err());
}

#[tokio::test]
async fn ambiguous_kick_suspends_obligation_and_never_enters_automatic_retry() {
    let state = crate::kick_requests::tests::fixture().await;
    let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
    let claimed = super::claim_next_for(&state.pool, Some("instance-a"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.job.id, id);
    let mut tx = state.pool.begin().await.unwrap();
    claimed.leases[0]
        .dispatch(&mut tx, "kick", &json!({"digest":"request"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    super::fail(
        &state.pool,
        &claimed.job,
        "response lost",
        true,
        false,
        false,
        true,
    )
    .await
    .unwrap();
    let stage: String = sqlx::query_scalar("SELECT stage FROM jobs WHERE id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(stage, "reconciling");
    let obligation: String = sqlx::query_scalar("SELECT state FROM kick_requests")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(obligation, "needs_attention");
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&id).execute(&state.pool).await.unwrap();
    recover_interrupted(&state.pool).await.unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT bool_and(blocked) FROM resource_leases WHERE work_id=$1"
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn expired_dispatch_is_quarantined_without_replaying_or_unblocking_resource() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    sqlx::query("INSERT INTO jobs(id,kind,resource_key,status,stage,available_at,created_at,updated_at) VALUES('ambiguous','sync','zone:a','queued','queued',now(),now(),now()),('unrelated','sync','zone:b','queued','queued',now(),now(),now())")
            .execute(&pool).await.unwrap();
    let claimed = super::claim_next_for(&pool, Some("instance-a"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.job.id, "ambiguous");
    let mut tx = pool.begin().await.unwrap();
    claimed.leases[0]
        .dispatch(&mut tx, "mutate", &json!({"digest":"request"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id='ambiguous'").execute(&pool).await.unwrap();
    recover_interrupted(&pool).await.unwrap();
    let state: (String, String) =
        sqlx::query_as("SELECT status,stage FROM jobs WHERE id='ambiguous'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, ("failed".into(), "reconciling".into()));
    let blocked: bool = sqlx::query_scalar(
        "SELECT bool_and(blocked) FROM resource_leases WHERE work_id='ambiguous'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(blocked);
    assert_eq!(
        super::claim_next_for(&pool, Some("instance-b"))
            .await
            .unwrap()
            .unwrap()
            .job
            .id,
        "unrelated"
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(claimed.leases[0].guard(&mut tx).await.is_err());
}

#[tokio::test]
async fn unowned_legacy_remote_work_requires_attention_instead_of_replay() {
    let pool = test_pool().await;
    insert_running_job(&pool).await;
    recover_interrupted(&pool).await.unwrap();
    let state: (String, String) =
        sqlx::query_as("SELECT status,stage FROM jobs WHERE id='job-running'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, ("failed".into(), "needs_attention".into()));
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT blocked FROM resource_leases WHERE resource_key='job:job-running'"
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn completion_retries_transaction_failure_without_duplicate_events() {
    let pool = test_pool().await;
    let state = crate::state::AppState::new(
        pool.clone(),
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    insert_running_job(&pool).await;
    sqlx::raw_sql("CREATE SEQUENCE completion_attempt; CREATE FUNCTION reject_first_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('completion_attempt') = 1 THEN RAISE EXCEPTION 'injected lock contention' USING ERRCODE='55P03'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_completion BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_first_completion();").execute(&pool).await.unwrap();
    let job = JobInput {
        id: "job-running".into(),
        kind: "sync".into(),
        node_id: None,
        target_revision: None,
        payload: json!({}),
        attempts: 2,
    };
    let output = crate::deployment::JobOutput {
        stage: "done".into(),
        result: json!({}),
        node_state: None,
        deployed_revision: None,
        deployed_config: None,
        deployed_sha256: None,
        published_connection: None,
        delete_node: false,
    };
    super::retry_persistence(|| super::persist_success(&state, &job, output.clone()))
        .await
        .unwrap();
    // Retrying an uncertain commit is also idempotent.
    super::succeed(&state, &job, output).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id='job-running'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "succeeded");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM job_events WHERE job_id='job-running'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn node_removal_interrupts_only_its_worker_and_preserves_cancellation() {
    let state = crate::kick_requests::tests::fixture().await;
    let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
    let job = super::claim_next(&state.pool).await.unwrap().unwrap();
    let receiver = state.removed_nodes.subscribe();
    let wait = super::wait_for_node_removal(&state, &job, receiver);
    tokio::pin!(wait);
    state.removed_nodes.send("other-node".into()).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut wait)
            .await
            .is_err()
    );
    crate::api::nodes::remove_record(
        axum::extract::State(state.clone()),
        axum::extract::Path("node".into()),
        axum::extract::Query(crate::api::nodes::RevisionQuery {
            expected_revision: Some(1),
        }),
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), &mut wait)
        .await
        .unwrap();
    assert!(!super::job_is_running(&state.pool, &id).await);
    // A stale result/failure cannot overwrite the terminal cancelled history.
    super::fail(&state.pool, &job, "stale timeout", true, false, false, true)
        .await
        .unwrap();
    let row: (String, String) = sqlx::query_as("SELECT status,stage FROM jobs WHERE id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(row, ("cancelled".into(), "node_removed".into()));
    assert!(super::claim_next(&state.pool).await.unwrap().is_none());
}

#[tokio::test]
async fn kick_continuation_reuses_id_releases_leases_and_does_not_spend_retry_budget() {
    let state = crate::kick_requests::tests::fixture().await;
    let users: Vec<String> = (0..2200).map(|i| format!("user-{i:05}")).collect();
    let mut tx = state.pool.begin().await.unwrap();
    let id = crate::kick_requests::enqueue_many(&mut tx, "node", &users, "access_restricted")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let first = super::claim_next_for(&state.pool, Some("batch-owner"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.job.attempts, 1);
    Box::pin(super::execute_claimed(state.clone(), first)).await;
    let row: (String, String, i64) =
        sqlx::query_as("SELECT status,stage,attempts FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(row, ("queued".into(), "batch_continue".into(), 1));
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL OR blocked",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(held, 0);
    let second = super::claim_next_for(&state.pool, Some("batch-owner"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.job.id, id);
    assert_eq!(second.job.attempts, 1);
    Box::pin(super::execute_claimed(state.clone(), second)).await;
    let row: (String, i64) = sqlx::query_as("SELECT status,attempts FROM jobs WHERE id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(row, ("succeeded".into(), 1));
    let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM jobs WHERE kind='kick'),(SELECT count(*) FROM kick_requests WHERE state='cancelled')").fetch_one(&state.pool).await.unwrap();
    assert_eq!(counts, (1, 2200));
}

#[tokio::test]
async fn successful_kick_commits_result_and_completes_durable_request() {
    let state = crate::kick_requests::tests::fixture().await;
    let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
    let job = super::claim_next(&state.pool).await.unwrap().unwrap();
    let batch = crate::kick_requests::batch(&state.pool, &job, None)
        .await
        .unwrap();
    crate::kick_requests::acknowledge(&state.pool, &job, &batch, "completed")
        .await
        .unwrap();
    let output = crate::deployment::JobOutput {
        stage: "clients_offline".into(),
        result: json!({"remaining_connections":0}),
        node_state: None,
        deployed_revision: None,
        deployed_config: None,
        deployed_sha256: None,
        published_connection: None,
        delete_node: false,
    };
    super::succeed(&state, &job, output).await.unwrap();
    let result: (String, serde_json::Value) =
        sqlx::query_as("SELECT status,result_json FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(result.0, "succeeded");
    assert_eq!(result.1["result"]["remaining_connections"], 0);
    let request: String = sqlx::query_scalar("SELECT state FROM kick_requests")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(request, "completed");
    super::maintenance_pass(&state).await.unwrap();
    assert!(super::claim_next(&state.pool).await.unwrap().is_none());
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM job_events WHERE job_id=$1 AND event_type='job.succeeded'",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(events, 1);
}

#[tokio::test]
async fn bounded_kicks_release_queue_and_keep_the_obligation() {
    let state = crate::kick_requests::tests::fixture().await;
    let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
    let mut job = super::claim_next(&state.pool).await.unwrap().unwrap();
    assert_eq!(job.id, id);
    super::fail(&state.pool, &job, "timeout", true, false, false, true)
        .await
        .unwrap();
    let seconds: i64 = sqlx::query_scalar(
        "SELECT round(extract(epoch FROM available_at-updated_at))::bigint FROM jobs WHERE id=$1",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(seconds, 30);
    sqlx::query("UPDATE jobs SET status='running',attempts=5 WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    job.attempts = 5;
    super::fail(&state.pool, &job, "timeout", false, false, false, true)
        .await
        .unwrap();
    let status: (String, String) = sqlx::query_as("SELECT status,stage FROM jobs WHERE id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, ("failed".into(), "waiting_recovery".into()));
    let request: String = sqlx::query_scalar("SELECT state FROM kick_requests")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(request, "waiting_recovery");
    let mut tx = state.pool.begin().await.unwrap();
    let ssh = crate::api::enqueue_job_in_tx(&mut tx, "ssh-test", Some("node"), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        super::claim_next(&state.pool).await.unwrap().unwrap().id,
        ssh
    );
}

#[tokio::test]
async fn fifth_interrupted_kick_is_not_executed_again_after_restart() {
    let state = crate::kick_requests::tests::fixture().await;
    let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
    sqlx::query("UPDATE jobs SET status='running',attempts=5 WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    super::recover_interrupted(&state.pool).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, "failed");
    assert!(super::claim_next(&state.pool).await.unwrap().is_none());
    let state: String = sqlx::query_scalar("SELECT state FROM kick_requests")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(state, "needs_attention");
}

#[tokio::test]
async fn failed_event_persistence_does_not_acknowledge_or_suspend_request() {
    let state = crate::kick_requests::tests::fixture().await;
    let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
    let job = super::claim_next(&state.pool).await.unwrap().unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_kick_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.failed' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_kick_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_kick_event();")
            .execute(&state.pool).await.unwrap();
    assert!(
        super::fail(&state.pool, &job, "timeout", false, false, false, true)
            .await
            .is_err()
    );
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, "running");
    let request: String = sqlx::query_scalar("SELECT state FROM kick_requests")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(request, "active");
}

#[tokio::test]
async fn fair_queue_orders_by_availability_instead_of_old_creation_time() {
    let state = crate::kick_requests::tests::fixture().await;
    let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
    let mut tx = state.pool.begin().await.unwrap();
    let ssh = crate::api::enqueue_job_in_tx(&mut tx, "ssh-test", Some("node"), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE jobs SET available_at=now()-interval '1 second' WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET available_at=now()-interval '2 seconds' WHERE id=$1")
        .bind(&ssh)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        super::claim_next(&state.pool).await.unwrap().unwrap().id,
        ssh
    );
}

#[tokio::test]
async fn interrupted_jobs_are_requeued_with_a_recovery_event() {
    let pool = test_pool().await;
    insert_running_job(&pool).await;

    // A claimed database-only execution expired before dispatch.
    sqlx::query("INSERT INTO resource_leases(resource_key,owner,work_id,generation,expires_at) VALUES('job:job-running','old','job-running',1,now()-interval '1 second')")
            .execute(&pool).await.unwrap();
    recover_interrupted(&pool).await.unwrap();

    let row = sqlx::query("SELECT status, stage, attempts, started_at, finished_at, available_at FROM jobs WHERE id = 'job-running'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.get::<String, _>("status"), "queued");
    assert_eq!(row.get::<String, _>("stage"), "recovered");
    assert_eq!(row.get::<i64, _>("attempts"), 2);
    assert_eq!(
        row.get::<chrono::DateTime<chrono::Utc>, _>("started_at"),
        chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );
    assert!(
        row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("finished_at")
            .is_none()
    );
    assert_ne!(
        row.get::<chrono::DateTime<chrono::Utc>, _>("available_at"),
        chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    );

    let event: (String, serde_json::Value) = sqlx::query_as(
        "SELECT event_type, payload_json FROM job_events WHERE job_id = 'job-running'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(event.0, "job.recovered");
    let payload = event.1;
    assert_eq!(payload["id"], "job-running");
    assert_eq!(payload["status"], "queued");
    assert_eq!(payload["stage"], "recovered");
}

#[tokio::test]
async fn failed_recovery_event_keeps_the_job_running() {
    let pool = test_pool().await;
    insert_running_job(&pool).await;
    sqlx::query("CREATE FUNCTION reject_recovery_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type = 'job.recovered' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$")
            .execute(&pool)
            .await
            .unwrap();
    sqlx::query("CREATE TRIGGER reject_recovery_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_recovery_event()")
            .execute(&pool)
            .await
            .unwrap();

    sqlx::query("INSERT INTO resource_leases(resource_key,owner,work_id,generation,expires_at) VALUES('job:job-running','old','job-running',1,now()-interval '1 second')")
            .execute(&pool).await.unwrap();
    assert!(recover_interrupted(&pool).await.is_err());

    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = 'job-running'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "running");
    let events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM job_events WHERE job_id = 'job-running'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(events, 0);
}

#[tokio::test]
async fn job_errors_redact_saved_config_and_assigned_user_secrets() {
    let pool = test_pool().await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    let values = [
        "ssh-password",
        "ssh-passphrase",
        "node-token",
        "stats-secret",
        "obfs-secret",
        "realm-token",
        "socks-user",
        "socks-password",
        "provider-token",
        "hy2-user-password",
        "client-certificate",
        "client-private-key",
    ];
    let config = json!({
        "server_config": {
            "obfs": {"gecko": {"password": values[4]}},
            "realm": {"connection": {"token": values[5]}},
            "outbounds": [{"socks5": {"username": values[6], "password": values[7]}}],
            "acme": {"dns": {"config": {"api_token": values[8]}}}
        }
    });
    let encrypted = |value: &str| secrets.encrypt(value).unwrap();
    sqlx::query("WITH fixture_ssh AS (INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('fixture-ssh','Fixture SSH','ssh_password',now(),now()) ON CONFLICT DO NOTHING), fixture_ssh_version AS (INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('fixture-ssh',1,'unused',now()) ON CONFLICT DO NOTHING) INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at,ssh_credential_id,ssh_credential_version) VALUES ('redaction-node','Node','node.example.test',22,'root','node.example.test',443,':443','node-hash',$1,$2,$3,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','fixture-ssh',1)")
            .bind(encrypted(values[2]))
            .bind(encrypted(values[3]))
            .bind(encrypted(&config.to_string()))
            .execute(&pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO users (id, name, created_at, updated_at) VALUES ('redaction-user', 'User', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
    sqlx::query("WITH assignment_fixture(user_id,node_id,credential_hash,credential_enc,created_at) AS MATERIALIZED (VALUES ('redaction-user', 'redaction-node', 'credential-hash', $1, '2026-01-01T00:00:00Z'::timestamptz)), password_fixture AS (INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at) SELECT 'connection:'||user_id,user_id,'connection',credential_hash,credential_enc,now() FROM (SELECT DISTINCT ON(user_id) user_id,credential_hash,credential_enc FROM assignment_fixture ORDER BY user_id) c ON CONFLICT(user_id,kind) WHERE revoked_at IS NULL DO UPDATE SET secret_hash=EXCLUDED.secret_hash,secret_enc=EXCLUDED.secret_enc), fixture_groups AS (INSERT INTO authorization_groups(id,name,created_at,updated_at) SELECT 'fixture-'||md5(user_id||chr(31)||node_id),'Fixture authorization',now(),now() FROM assignment_fixture ON CONFLICT(id) DO UPDATE SET name=EXCLUDED.name RETURNING id), fixture_members AS (INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT g.id,a.user_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING), fixture_nodes AS (INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT g.id,a.node_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING) INSERT INTO node_assignments(user_id,node_id,created_at) SELECT user_id,node_id,created_at FROM assignment_fixture")
            .bind(encrypted(values[9]))
            .execute(&pool)
            .await
            .unwrap();

    let mut tx = pool.begin().await.unwrap();
    crate::credentials::insert(
        &mut tx,
        &secrets,
        "SSH redaction fixture",
        "ssh_private_key",
        None,
        &json!({"secret":values[0],"passphrase":values[1]}),
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    crate::credentials::insert(
        &mut tx,
        &secrets,
        "mTLS redaction fixture",
        "tls_identity",
        Some("redaction-user"),
        &json!({"certificate":values[10],"private_key":values[11]}),
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let job = JobInput {
        id: "redaction-job".to_owned(),
        kind: "sync".to_owned(),
        node_id: Some("redaction-node".to_owned()),
        target_revision: None,
        payload: json!({}),
        attempts: 1,
    };
    let error_message = values.join(" | ");
    let error = anyhow::anyhow!(error_message);

    let safe = safe_error(&pool, &secrets, &job, &error).await;

    for value in values {
        assert!(!safe.contains(value), "job error leaked a secret: {value}");
    }
    assert!(safe.contains("[redacted]"));
}
