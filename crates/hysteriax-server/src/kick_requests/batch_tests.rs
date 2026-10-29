use super::*;

#[tokio::test]
async fn migration_discards_old_requests_jobs_events_and_execution_history() {
    let state = tests::fixture().await;
    let id = tests::enqueue(&state, "user_deleted").await;
    sqlx::raw_sql("ALTER TABLE kick_requests DROP COLUMN execution_generation; DROP INDEX kick_requests_active_job_idx;")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let keep = crate::api::enqueue_job_in_tx(&mut tx, "ssh-test", Some("node"), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,payload_json,status,stage,available_at,created_at,updated_at,retry_of_job_id) VALUES('old-failed','kick','node','{}','failed','failed',now(),now(),now(),$1)")
        .bind(&id).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO resource_leases(resource_key,owner,generation,work_id,expires_at,blocked) VALUES('node:node','old',1,$1,clock_timestamp()+interval '1 minute',true)").bind(&id).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO execution_operations(operation_id,resource_key,owner,generation,work_id,step,state,intent) VALUES('old-kick-op','node:node','old',1,$1,'kick','needs_attention','{}')").bind(&id).execute(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!("../../migrations/0002_node_kick_batches.sql"))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    for query in [
        "SELECT count(*) FROM kick_requests",
        "SELECT count(*) FROM jobs WHERE kind='kick'",
        "SELECT count(*) FROM job_events WHERE job_id NOT IN (SELECT id FROM jobs)",
        "SELECT count(*) FROM execution_operations",
        "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL OR blocked",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(query))
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            0
        );
    }
    let kept: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
        .bind(keep)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(kept, "queued");
}

#[tokio::test]
async fn checkpoint_rejects_a_different_attempt_without_modifying_requests() {
    let state = tests::fixture().await;
    let id = tests::enqueue(&state, "user_deleted").await;
    sqlx::query("UPDATE jobs SET status='running',attempts=2 WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    let current = tests::job(&state, &id).await;
    let batch = batch(&state.pool, &current, None).await.unwrap();
    let mut stale = current.clone();
    stale.attempts = 1;
    assert!(
        acknowledge(&state.pool, &stale, &batch, "completed")
            .await
            .is_err()
    );
    let state: String = sqlx::query_scalar("SELECT state FROM kick_requests")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(state, "active");
}

#[tokio::test]
async fn clearing_one_user_keeps_the_other_users_node_job() {
    let state = tests::fixture().await;
    let mut tx = state.pool.begin().await.unwrap();
    let id = enqueue_many(
        &mut tx,
        "node",
        &["alice".into(), "bob".into()],
        "authorization_group_removed",
    )
    .await
    .unwrap();
    clear_reason(
        &mut tx,
        Some("node"),
        Some("alice"),
        "authorization_group_removed",
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "queued"
    );
    assert!(
        still_required(&state.pool, &tests::job(&state, &id).await)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn concurrent_different_users_share_one_node_job() {
    let state = tests::fixture().await;
    async fn add(state: &crate::state::AppState, user: &str) -> String {
        let mut tx = state.pool.begin().await.unwrap();
        let id = enqueue_many(
            &mut tx,
            "node",
            &[user.to_owned()],
            "authorization_group_removed",
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }
    let (a, b) = tokio::join!(add(&state, "alice"), add(&state, "bob"));
    assert_eq!(a, b);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM kick_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn trusted_recovery_merges_waiting_users_into_an_already_queued_node_job() {
    let state = tests::fixture().await;
    let old = tests::enqueue(&state, "user_deleted").await;
    sqlx::query("UPDATE jobs SET status='failed',stage='waiting_recovery' WHERE id=$1")
        .bind(&old)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE kick_requests SET state='waiting_recovery'")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let current = enqueue_many(
        &mut tx,
        "node",
        &["another".into()],
        "authorization_group_removed",
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    resume_node(&state.pool, "node").await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM kick_requests WHERE state='active' AND latest_job_id=$1",
    )
    .bind(current)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn migration_keeps_another_operations_node_barrier() {
    let state = tests::fixture().await;
    let old = tests::enqueue(&state, "user_deleted").await;
    sqlx::raw_sql("ALTER TABLE kick_requests DROP COLUMN execution_generation; DROP INDEX kick_requests_active_job_idx;")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let keep = crate::api::enqueue_job_in_tx(&mut tx, "ssh-test", Some("node"), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("INSERT INTO resource_leases(resource_key,owner,generation,work_id,expires_at,blocked) VALUES('node:node','old',1,$1,clock_timestamp()+interval '1 minute',true)").bind(old).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO execution_operations(operation_id,resource_key,owner,generation,work_id,step,state,intent) VALUES('other-operation','node:node','owner',1,$1,'ssh','reconciling','{}')").bind(keep).execute(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::raw_sql(include_str!("../../migrations/0002_node_kick_batches.sql"))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let row:(bool,bool,i64)=sqlx::query_as("SELECT owner IS NULL,blocked,generation FROM resource_leases WHERE resource_key='node:node'").fetch_one(&state.pool).await.unwrap();
    assert_eq!(row, (true, true, 2));
    let state: String = sqlx::query_scalar(
        "SELECT state FROM execution_operations WHERE operation_id='other-operation'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(state, "reconciling");
}

#[tokio::test]
async fn clearing_pairs_preserves_crossed_pairs_other_reasons_and_shared_jobs() {
    let state = tests::fixture().await;
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES('node-2','Second','127.0.0.1',22,'root','private_key','unused','example.test',443,':443','hash-2','unused','unused','{}',now(),now())")
        .execute(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let first = enqueue_many(
        &mut tx,
        "node",
        &["alice".into(), "bob".into()],
        "authorization_group_removed",
    )
    .await
    .unwrap();
    let second = enqueue_many(
        &mut tx,
        "node-2",
        &["alice".into(), "bob".into()],
        "authorization_group_removed",
    )
    .await
    .unwrap();
    enqueue_many(&mut tx, "node", &["alice".into()], "access_restricted")
        .await
        .unwrap();
    clear_reason_for_pairs(
        &mut tx,
        &[
            ("alice".into(), "node".into()),
            ("bob".into(), "node-2".into()),
        ],
        "authorization_group_removed",
    )
    .await
    .unwrap();
    let active: Vec<(String,String)> = sqlx::query_as("SELECT user_id,node_id FROM kick_requests WHERE state='active' AND reasons ? 'authorization_group_removed' ORDER BY user_id,node_id").fetch_all(&mut *tx).await.unwrap();
    assert_eq!(
        active,
        vec![
            ("alice".into(), "node-2".into()),
            ("bob".into(), "node".into())
        ]
    );
    let restriction: (String,i64,Value) = sqlx::query_as("SELECT state,generation,reasons FROM kick_requests WHERE user_id='alice' AND node_id='node'").fetch_one(&mut *tx).await.unwrap();
    assert_eq!(restriction.0, "active");
    assert_eq!(restriction.1, 3);
    assert_eq!(restriction.2, json!({"access_restricted":true}));
    clear_reason_for_pairs(&mut tx, &active, "authorization_group_removed")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    schedule(&mut tx, None, false).await.unwrap();
    let statuses: Vec<(String, String)> =
        sqlx::query_as("SELECT id,status FROM jobs WHERE id=ANY($1) ORDER BY id")
            .bind(vec![first.clone(), second.clone()])
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert!(statuses.contains(&(first.clone(), "queued".into())));
    assert!(statuses.contains(&(second.clone(), "cancelled".into())));
    let events: Vec<String> =
        sqlx::query_scalar("SELECT job_id FROM job_events WHERE event_type='job.cancelled'")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
    assert_eq!(events, vec![second]);
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn enqueue_pairs_keeps_shared_running_generation_and_exact_pairs() {
    let state = tests::fixture().await;
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES('node-2','Second','127.0.0.1',22,'root','private_key','unused','example.test',443,':443','hash-2','unused','unused','{}',now(),now())")
        .execute(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let running = enqueue_many(
        &mut tx,
        "node",
        &["alice".into(), "other".into()],
        "access_restricted",
    )
    .await
    .unwrap();
    sqlx::query("UPDATE jobs SET status='running' WHERE id=$1")
        .bind(&running)
        .execute(&mut *tx)
        .await
        .unwrap();
    let ids = enqueue_pairs(
        &mut tx,
        &[
            ("alice".into(), "node".into()),
            ("alice".into(), "node".into()),
            ("bob".into(), "node-2".into()),
        ],
        "authorization_group_removed",
    )
    .await
    .unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&running));
    let rows:Vec<(String,String,i64,Option<i64>,Value)>=sqlx::query_as("SELECT user_id,node_id,generation,execution_generation,reasons FROM kick_requests ORDER BY user_id,node_id").fetch_all(&mut *tx).await.unwrap();
    assert_eq!(
        rows,
        vec![
            (
                "alice".into(),
                "node".into(),
                2,
                Some(1),
                json!({"access_restricted":true,"authorization_group_removed":true})
            ),
            (
                "bob".into(),
                "node-2".into(),
                1,
                Some(1),
                json!({"authorization_group_removed":true})
            ),
            (
                "other".into(),
                "node".into(),
                1,
                Some(1),
                json!({"access_restricted":true})
            ),
        ]
    );
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM job_events WHERE event_type='job.queued'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(events, 2);
    let again = enqueue_pairs(
        &mut tx,
        &[("bob".into(), "node-2".into())],
        "authorization_group_removed",
    )
    .await
    .unwrap();
    assert_eq!(again.len(), 1);
    assert!(ids.contains(&again[0]));
    let queued: (i64, Option<i64>) = sqlx::query_as(
        "SELECT generation,execution_generation FROM kick_requests WHERE user_id='bob'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(queued, (2, Some(2)));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM job_events WHERE event_type='job.queued'"
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap(),
        2
    );
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn running_completion_after_snapshot_does_not_force_regrant_to_restart() {
    let state = tests::fixture().await;
    let mut seed = state.pool.begin().await.unwrap();
    let job = enqueue_many(
        &mut seed,
        "node",
        &["alice".into()],
        "authorization_group_removed",
    )
    .await
    .unwrap();
    sqlx::query("UPDATE jobs SET status='running' WHERE id=$1")
        .bind(&job)
        .execute(&mut *seed)
        .await
        .unwrap();
    seed.commit().await.unwrap();
    let mut authorization = state
        .pool
        .begin_with("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .await
        .unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id=$1")
        .bind(&job)
        .fetch_one(&mut *authorization)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET status='failed',stage='waiting_recovery',updated_at=clock_timestamp() WHERE id=$1")
        .bind(&job)
        .execute(&state.pool)
        .await
        .unwrap();
    clear_reason_for_pairs(
        &mut authorization,
        &[("alice".into(), "node".into())],
        "authorization_group_removed",
    )
    .await
    .unwrap();
    authorization.commit().await.unwrap();
    let job_state: (String, String) = sqlx::query_as("SELECT status,stage FROM jobs WHERE id=$1")
        .bind(&job)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(job_state, ("failed".into(), "waiting_recovery".into()));
    let request: (String, i64, Value) =
        sqlx::query_as("SELECT state,generation,reasons FROM kick_requests WHERE user_id='alice'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(request, ("cancelled".into(), 2, json!({})));
}

#[tokio::test]
async fn regrant_commits_while_job_is_busy_and_scheduler_cleans_after_restart() {
    let state = tests::fixture().await;
    let mut seed = state.pool.begin().await.unwrap();
    let job = enqueue_many(
        &mut seed,
        "node",
        &["alice".into()],
        "authorization_group_removed",
    )
    .await
    .unwrap();
    seed.commit().await.unwrap();
    let mut worker = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id=$1 FOR UPDATE")
        .bind(&job)
        .fetch_one(&mut *worker)
        .await
        .unwrap();
    let mut authorization = state
        .pool
        .begin_with("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        clear_reason_for_pairs(
            &mut authorization,
            &[("alice".into(), "node".into())],
            "authorization_group_removed",
        ),
    )
    .await
    .unwrap()
    .unwrap();
    authorization.commit().await.unwrap();
    let mut cleanup = state.pool.begin().await.unwrap();
    schedule(&mut cleanup, None, false).await.unwrap();
    cleanup.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id=$1")
            .bind(&job)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "queued"
    );
    worker.rollback().await.unwrap();
    let mut restarted = state.pool.begin().await.unwrap();
    schedule(&mut restarted, None, false).await.unwrap();
    restarted.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id=$1")
            .bind(&job)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "cancelled"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM job_events WHERE job_id=$1 AND event_type='job.cancelled'"
        )
        .bind(&job)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        1
    );
}
