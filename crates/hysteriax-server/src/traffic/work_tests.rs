use crate::{execution, security::SecretBox, state::AppState, traffic::batches};
use serde_json::json;
use serde_json::{Map, Value};

async fn fixture(users: usize) -> AppState {
    let pool = crate::traffic::batch_tests::fixture(users).await;
    AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    )
}
fn values(users: usize, tx: i64) -> Map<String, Value> {
    (1..=users)
        .map(|n| (format!("user-{n:04}"), json!({"tx":tx,"rx":0})))
        .collect()
}
async fn capture(state: &AppState, id: &str, node: &str, users: usize, tx: i64) {
    batches::reserve(&state.pool, id, node, None).await.unwrap();
    batches::seal(&state.pool, id, "original-instance", &values(users, tx))
        .await
        .unwrap();
}
async fn number(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
#[tokio::test]
async fn capture_expiry_is_bounded_fenced_and_skips_live_owners_and_locked_rows() {
    let state = fixture(1).await;
    sqlx::query("INSERT INTO traffic_sample_batches(sample_id,node_id,capture_deadline) SELECT 'expired-'||i,'a',clock_timestamp()-interval '1 minute' FROM generate_series(1,301)i").execute(&state.pool).await.unwrap();
    batches::reserve(&state.pool, "live", "a", None)
        .await
        .unwrap();
    sqlx::query("UPDATE traffic_sample_batches SET capture_deadline=clock_timestamp()-interval '1 minute' WHERE sample_id='live'").execute(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    execution::acquire(&mut tx, "work:live", "healthy", "live")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    capture(&state, "ready", "b", 1, 10).await;
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query(
        "SELECT sample_id FROM traffic_sample_batches WHERE sample_id='expired-1' FOR UPDATE",
    )
    .fetch_one(&mut *held)
    .await
    .unwrap();
    crate::traffic::maintenance_pass(&state).await.unwrap();
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM traffic_sample_batches WHERE state='discarded'"
        )
        .await,
        256
    );
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE kind='traffic-maintenance'").execute(&state.pool).await.unwrap();
    crate::traffic::maintenance_pass(&state).await.unwrap();
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM traffic_sample_batches WHERE state='discarded'"
        )
        .await,
        300
    );
    held.rollback().await.unwrap();
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE kind='traffic-maintenance'").execute(&state.pool).await.unwrap();
    crate::traffic::maintenance_pass(&state).await.unwrap();
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM traffic_sample_batches WHERE state='discarded'"
        )
        .await,
        301
    );
    assert_eq!(number(&state,"SELECT count(*) FROM traffic_sample_batches WHERE sample_id='live' AND state='capturing'").await,1);
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM traffic_sample_batches WHERE sample_id='ready' AND state='ready'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn capture_expiry_ack_failure_rolls_back_and_a_replaced_owner_cannot_discard() {
    use crate::background::maintenance;
    let state = fixture(0).await;
    batches::reserve(&state.pool, "old-read", "a", None)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE traffic_sample_batches SET capture_deadline=clock_timestamp()-interval '1 second'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    maintenance::ensure(
        &state.pool,
        "traffic-capture-expiry",
        "traffic-maintenance",
        json!({}),
    )
    .await
    .unwrap();
    let old = maintenance::claim(&state.pool, "traffic-maintenance", "old")
        .await
        .unwrap()
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_capture_expiry_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state='complete' THEN RAISE EXCEPTION 'reject expiry acknowledgement'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_capture_expiry_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_capture_expiry_ack();").execute(&state.pool).await.unwrap();
    assert!(crate::traffic::expire_work(&state, &old).await.is_err());
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM traffic_sample_batches WHERE state='capturing'"
        )
        .await,
        1
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM background_work WHERE state='running'"
        )
        .await,
        1
    );
    sqlx::query("DROP TRIGGER reject_capture_expiry_ack ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    let new = maintenance::claim(&state.pool, "traffic-maintenance", "new")
        .await
        .unwrap()
        .unwrap();
    assert!(crate::traffic::expire_work(&state, &old).await.is_err());
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM traffic_sample_batches WHERE state='capturing'"
        )
        .await,
        1
    );
    crate::traffic::expire_work(&state, &new).await.unwrap();
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM traffic_sample_batches WHERE state='discarded'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn readiness_rejects_orphaned_capture_without_modifying_it() {
    let state = fixture(0).await;
    super::assert_ready(&state.pool).await.unwrap();
    batches::reserve(&state.pool, "orphan", "a", None)
        .await
        .unwrap();
    let before: Value = sqlx::query_scalar(
        "SELECT to_jsonb(b) FROM traffic_sample_batches b WHERE sample_id='orphan'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(super::assert_ready(&state.pool).await.is_err());
    let after: Value = sqlx::query_scalar(
        "SELECT to_jsonb(b) FROM traffic_sample_batches b WHERE sample_id='orphan'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        number(&state, "SELECT count(*) FROM background_work").await,
        0
    );
}
