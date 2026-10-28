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
