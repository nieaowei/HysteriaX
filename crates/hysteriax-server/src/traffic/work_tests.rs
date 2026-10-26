use super::*;
use crate::{background, execution, security::SecretBox, state::AppState, traffic::batches};
use serde_json::{Map, Value};

async fn fixture(users: usize, version: i64) -> AppState {
    let pool = crate::traffic::batch_tests::fixture_through(users, version).await;
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
async fn disable_old_sql_lock(pool: &PgPool) {
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(pool).await.unwrap();
}
#[tokio::test]
async fn maintenance_upgrade_preserves_partial_captures_and_recovers_original_ids_without_remote_reads()
 {
    let state = fixture(501, 51).await;
    capture(&state, "old-a", "a", 501, 5).await;
    capture(&state, "old-b", "b", 501, 2).await;
    assert!(matches!(
        batches::process(&state.pool, "old-a").await.unwrap(),
        batches::Progress::Committed {
            items: 500,
            complete: false
        }
    ));
    let snapshot = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'users',(SELECT jsonb_agg(to_jsonb(u) ORDER BY id) FROM users u),'batches',(SELECT jsonb_agg(to_jsonb(b) ORDER BY sample_id) FROM traffic_sample_batches b),'items',(SELECT jsonb_agg(to_jsonb(i) ORDER BY id) FROM traffic_sample_items i),'baselines',(SELECT jsonb_agg(to_jsonb(b) ORDER BY node_id,user_id) FROM traffic_baselines b),'records',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM traffic_records r))";
    let before: Value = sqlx::query_scalar(snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    db::run_migrations(&state.pool).await.unwrap();
    assert!(assert_ready(&state.pool).await.is_err());
    initialize(&state.pool).await.unwrap();
    initialize(&state.pool).await.unwrap();
    assert_ready(&state.pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Value>(snapshot)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='traffic'"
        )
        .await,
        2
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM sampling_cycles").await,
        2
    );
    disable_old_sql_lock(&state.pool).await;
    let legacy = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    for _ in 0..2 {
        let work = background::claim(&state.pool, "traffic", &state.instance_id)
            .await
            .unwrap()
            .unwrap();
        assert!(work.payload["legacy_billing"].as_bool().unwrap());
        let outcome = execution::supervise(state.pool.clone(), work.leases.clone(), async {
            // The fixture's SSH ciphertext is intentionally invalid. Recovery
            // must consume the saved batch and never load that remote material.
            crate::traffic::sample_work(&state, &work).await.unwrap();
            background::finish(&state.pool, &work, true).await.unwrap();
        })
        .await;
        let status: String = sqlx::query_scalar("SELECT state FROM background_work WHERE id=$1")
            .bind(&work.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            status, "complete",
            "work {} ended with {outcome:?}",
            work.id
        );
    }
    legacy.rollback().await.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM users WHERE usage_bytes=7").await,
        501
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM traffic_records").await,
        1002
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM background_work WHERE state='complete'"
        )
        .await,
        2
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
    assert!(
        background::claim(&state.pool, "traffic", "another")
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn adoption_is_bounded_and_skips_locked_batches_without_claiming_initialization_is_ready() {
    let state = fixture(0, i64::MAX).await;
    sqlx::query("INSERT INTO traffic_sample_batches(sample_id,node_id,state,instance_id,payload_digest) SELECT 'old-'||i,'a','ready','instance','digest' FROM generate_series(1,301)i").execute(&state.pool).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT sample_id FROM traffic_sample_batches WHERE sample_id='old-1' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    assert_eq!(adopt_batch(&state.pool).await.unwrap(), 256);
    assert_eq!(adopt_batch(&state.pool).await.unwrap(), 44);
    assert_eq!(adopt_batch(&state.pool).await.unwrap(), 0);
    assert!(
        initialize(&state.pool).await.is_err(),
        "a skipped orphan must prevent maintenance finalization"
    );
    assert!(assert_ready(&state.pool).await.is_err());
    assert_eq!(
        number(&state, "SELECT count(*) FROM background_initialization").await,
        0
    );
    held.rollback().await.unwrap();
    initialize(&state.pool).await.unwrap();
    assert_ready(&state.pool).await.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM background_work").await,
        301
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM sampling_cycles").await,
        301
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM traffic_sample_batches WHERE state='ready'"
        )
        .await,
        301
    );
}
#[tokio::test]
async fn adoption_failure_rolls_back_discarding_and_ownership_and_keeps_startup_read_only() {
    let state = fixture(1, i64::MAX).await;
    let cycle = Uuid::new_v4();
    batches::reserve(&state.pool, "uncaptured", "a", Some(cycle))
        .await
        .unwrap();
    sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,status,traffic_status,pending) VALUES($1,'a',clock_timestamp(),'ok','pending',true)").bind(cycle).execute(&state.pool).await.unwrap();
    capture(&state, "captured", "b", 1, 10).await;
    sqlx::raw_sql("CREATE FUNCTION reject_adoption() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject work insert'; END $$; CREATE TRIGGER reject_adoption BEFORE INSERT ON background_work FOR EACH ROW EXECUTE FUNCTION reject_adoption();").execute(&state.pool).await.unwrap();
    assert!(initialize(&state.pool).await.is_err());
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
            "SELECT count(*) FROM online_samples WHERE pending AND traffic_status='pending'"
        )
        .await,
        1
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM background_work").await,
        0
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM sampling_cycles").await,
        0
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM background_initialization").await,
        0
    );
    assert!(assert_ready(&state.pool).await.is_err());
    assert_eq!(
        number(&state, "SELECT count(*) FROM background_initialization").await,
        0
    );
    sqlx::query("DROP TRIGGER reject_adoption ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    initialize(&state.pool).await.unwrap();
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM traffic_sample_batches WHERE state='discarded'"
        )
        .await,
        1
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM online_samples WHERE NOT pending AND traffic_status='failed'"
        )
        .await,
        1
    );
    assert_eq!(number(&state,"SELECT count(*) FROM traffic_sample_batches WHERE sample_id='captured' AND state='ready'").await,1);
    assert_eq!(
        number(&state, "SELECT count(*) FROM users WHERE usage_bytes=0").await,
        1
    );
}
#[tokio::test]
async fn capture_expiry_is_bounded_fenced_and_skips_live_owners_and_locked_rows() {
    let state = fixture(1, i64::MAX).await;
    disable_old_sql_lock(&state.pool).await;
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
    let state = fixture(0, i64::MAX).await;
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
