use super::*;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;

#[tokio::test]
async fn composite_queue_pages_use_primary_key_order_and_index_ranges() {
    let pool = fixture().await;
    sqlx::raw_sql("INSERT INTO traffic_dirty_hours(source,node_id,hour) SELECT 'users','n1','2020-01-01'::timestamptz+i*interval '1 hour' FROM generate_series(1,2000)i; INSERT INTO online_dirty_minutes(scope_node_id,minute) SELECT 'n1','2020-01-01'::timestamptz+i*interval '1 minute' FROM generate_series(1,2000)i; ANALYZE traffic_dirty_hours; ANALYZE online_dirty_minutes")
        .execute(&pool).await.unwrap();
    let start: DateTime<Utc> = "2020-01-01T00:00:00Z".parse().unwrap();
    let mut connection = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL plan_cache_mode=force_generic_plan")
        .execute(&mut *connection)
        .await
        .unwrap();
    for (queue, index, cursor) in [
        (
            Queue::Hours,
            "traffic_dirty_hours_pkey",
            json!({"source":"users","node_id":"n1","hour":start+Duration::hours(1000)}),
        ),
        (
            Queue::Minutes,
            "online_dirty_minutes_pkey",
            json!({"scope_node_id":"n1","minute":start+Duration::minutes(1000)}),
        ),
    ] {
        for after in [None, Some(cursor)] {
            let query = format!(
                "EXPLAIN (ANALYZE, FORMAT JSON) {}",
                super::refresh::candidates_sql(queue, after.is_some())
            );
            let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
                .bind(&after)
                .bind(64_i64)
                .fetch_one(&mut *connection)
                .await
                .unwrap();
            fn contains(plan: &serde_json::Value, key: &str, value: &str) -> bool {
                plan[key] == value
                    || plan["Plans"].as_array().is_some_and(|children| {
                        children.iter().any(|child| contains(child, key, value))
                    })
            }
            let root = &plan[0]["Plan"];
            assert!(contains(root, "Index Name", index));
            assert!(!contains(root, "Node Type", "Sort"));
            assert_eq!(root["Actual Rows"].as_f64(), Some(64.0));
            if after.is_some() {
                assert!(
                    plan.to_string().contains("Index Cond"),
                    "cursor must bound the index scan"
                );
            }
        }
    }
}

async fn fixture() -> PgPool {
    let pool = crate::db::test_pool_with_max_connections(4).await;
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('u','User',now(),now())")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("WITH fixture_ssh AS (INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('fixture-ssh','Fixture SSH','ssh_password',now(),now()) ON CONFLICT DO NOTHING), fixture_ssh_version AS (INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('fixture-ssh',1,'unused',now()) ON CONFLICT DO NOTHING) INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at,ssh_credential_id,ssh_credential_version) SELECT 'n'||i,'Node '||i,'127.0.0.1',22,'root','example.test',443,':443','hash'||i,'unused','unused','{}',now(),now(),'fixture-ssh',1 FROM generate_series(1,2)i").execute(&pool).await.unwrap();
    pool
}
async fn cycle(pool: &PgPool, id: uuid::Uuid, at: DateTime<Utc>, connections: i64) {
    sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,cycle_started_at,status,traffic_status,connections,expected_nodes) VALUES($1,'n1',$2,$2,'ok','ok',$3,1)")
        .bind(id).bind(at).bind(json!({"u":connections})).execute(pool).await.unwrap();
}
async fn generation(pool: &PgPool, id: uuid::Uuid) -> i64 {
    sqlx::query_scalar("SELECT generation FROM analytics_dirty_cycles WHERE cycle_id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}
type Reading = (
    DateTime<Utc>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    i64,
    bool,
    i64,
    i64,
    bool,
);
async fn history(pool: &PgPool, start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<Reading> {
    sqlx::query_as(include_str!("online_history.sql"))
        .bind("hour")
        .bind("UTC")
        .bind(start)
        .bind(end)
        .bind(None::<String>)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn generations_are_monotonic_across_ack_and_source_rollbacks() {
    let pool = fixture().await;
    let id = uuid::Uuid::new_v4();
    cycle(&pool, id, Utc::now(), 1).await;
    let initial = generation(&pool, id).await;
    sqlx::query("UPDATE online_samples SET connections='{\"u\":2}' WHERE cycle_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let changed = generation(&pool, id).await;
    assert!(changed > initial);
    refresh(&pool).await.unwrap();
    sqlx::query("UPDATE online_samples SET connections='{\"u\":3}' WHERE cycle_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let next = generation(&pool, id).await;
    assert!(
        next > changed,
        "acknowledgement must not reset the per-key generation"
    );
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE online_samples SET connections='{\"u\":99}' WHERE cycle_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(generation(&pool, id).await, next);
}

#[tokio::test]
async fn statistics_queues_are_bounded_and_acknowledgements_roll_back_with_results() {
    let pool = fixture().await;
    sqlx::raw_sql("INSERT INTO analytics_dirty_cycles(cycle_id) SELECT gen_random_uuid() FROM generate_series(1,300);
        INSERT INTO traffic_dirty_hours(source,node_id,hour) SELECT 'users','n1','2026-01-01'::timestamptz+i*interval '1 hour' FROM generate_series(1,300)i;
        INSERT INTO online_dirty_minutes(scope_node_id,minute) SELECT 'n1','2026-01-01'::timestamptz+i*interval '1 minute' FROM generate_series(1,300)i;
        INSERT INTO job_retry_state_dirty(job_id) SELECT 'missing-'||i FROM generate_series(1,300)i;").execute(&pool).await.unwrap();
    for (queue, table, limit) in [
        (Queue::Cycles, "analytics_dirty_cycles", 1),
        (Queue::Hours, "traffic_dirty_hours", 64),
        (Queue::Minutes, "online_dirty_minutes", 64),
        (Queue::Jobs, "job_retry_state_dirty", 32),
    ] {
        let mut tx = pool.begin().await.unwrap();
        let claim = claim_on(&mut tx, queue, None).await.unwrap();
        assert_eq!(claim.rows.len(), limit);
        assert_eq!(
            refresh_queue_on(&mut tx, queue, claim).await.unwrap(),
            limit as u64
        );
        tx.rollback().await.unwrap();
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 300, "rollback of {table}");
        let mut tx = pool.begin().await.unwrap();
        let claim = claim_on(&mut tx, queue, None).await.unwrap();
        assert_eq!(
            refresh_queue_on(&mut tx, queue, claim).await.unwrap(),
            limit as u64
        );
        tx.commit().await.unwrap();
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            count,
            300 - limit as i64,
            "bounded acknowledgement of {table}"
        );
    }
    refresh(&pool).await.unwrap();
}

#[tokio::test]
async fn another_refresher_skips_claimed_keys_and_history_overlays_the_held_source() {
    let pool = fixture().await;
    let (a, b) = (uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2));
    let start: DateTime<Utc> = "2026-01-01T12:00:00Z".parse().unwrap();
    cycle(&pool, a, start + Duration::seconds(10), 1).await;
    cycle(&pool, b, start + Duration::seconds(20), 3).await;
    let mut owner = pool.begin().await.unwrap();
    let held = claim_on(&mut owner, Queue::Cycles, None).await.unwrap();
    assert_eq!(held.rows[0]["cycle_id"], json!(a));
    tokio::time::timeout(std::time::Duration::from_secs(2), refresh(&pool))
        .await
        .unwrap()
        .unwrap();
    let pending: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT cycle_id FROM analytics_dirty_cycles")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(pending, vec![a]);
    let before = history(&pool, start, start + Duration::hours(1)).await;
    assert_eq!(before.len(), 1);
    assert_eq!(
        before[0].3,
        Some(2.0),
        "one raw and one refreshed cycle must be counted once each"
    );
    owner.commit().await.unwrap();
    refresh(&pool).await.unwrap();
    assert_eq!(
        history(&pool, start, start + Duration::hours(1)).await,
        before
    );
}

#[tokio::test]
async fn source_writes_commit_during_a_claim_and_old_generation_is_not_acknowledged() {
    let pool = fixture().await;
    let id = uuid::Uuid::new_v4();
    let start: DateTime<Utc> = "2026-01-01T12:00:00Z".parse().unwrap();
    cycle(&pool, id, start + Duration::seconds(10), 1).await;
    let original = generation(&pool, id).await;
    let mut owner = pool.begin().await.unwrap();
    let claim = claim_on(&mut owner, Queue::Cycles, None).await.unwrap();
    let rows = refresh::compute_on(&mut owner, Queue::Cycles, Value::Array(claim.rows))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        sqlx::query("UPDATE online_samples SET connections='{\"u\":9}' WHERE cycle_id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    })
    .await
    .expect("a computing refresher must not lock source dirty rows");
    assert!(generation(&pool, id).await > original);
    assert_eq!(
        refresh::acknowledge_on(&mut owner, Queue::Cycles, &rows)
            .await
            .unwrap(),
        0
    );
    owner.commit().await.unwrap();
    assert!(generation(&pool, id).await > original);
    let stale:i64=sqlx::query_scalar("SELECT connections::bigint FROM online_cycle_metrics WHERE cycle_id=$1 AND scope_node_id=''")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(
        stale, 1,
        "old result can commit only while its dirty key remains visible"
    );
    let before = history(&pool, start, start + Duration::hours(1)).await;
    assert_eq!(before[0].3, Some(9.0));
    refresh(&pool).await.unwrap();
    assert_eq!(
        history(&pool, start, start + Duration::hours(1)).await,
        before
    );
}

#[tokio::test]
async fn result_and_generation_ack_rollback_together_and_worker_lock_is_recoverable() {
    let pool = fixture().await;
    let id = uuid::Uuid::new_v4();
    cycle(&pool, id, Utc::now(), 7).await;
    let original = generation(&pool, id).await;
    let mut owner = pool.begin().await.unwrap();
    let claim = claim_on(&mut owner, Queue::Cycles, None).await.unwrap();
    assert_eq!(
        refresh_queue_on(&mut owner, Queue::Cycles, claim)
            .await
            .unwrap(),
        1
    );
    let visible:i64=sqlx::query_scalar("SELECT connections::bigint FROM online_cycle_metrics WHERE cycle_id=$1 AND scope_node_id=''")
        .bind(id).fetch_one(&mut *owner).await.unwrap();
    assert_eq!(visible, 7);
    owner.rollback().await.unwrap();
    assert_eq!(generation(&pool, id).await, original);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM online_cycle_metrics WHERE cycle_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
    // Rollback/process loss releases the transaction advisory lock as well.
    refresh(&pool).await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM analytics_dirty_cycles WHERE cycle_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn moved_and_deleted_dirty_cycles_do_not_leave_old_rollups_visible() {
    let pool = fixture().await;
    let id = uuid::Uuid::new_v4();
    let start: DateTime<Utc> = "2026-01-01T12:00:00Z".parse().unwrap();
    cycle(&pool, id, start + Duration::seconds(10), 4).await;
    refresh(&pool).await.unwrap();
    let end = start + Duration::minutes(2);
    assert_eq!(history(&pool, start, end).await.len(), 1);
    sqlx::query("UPDATE online_samples SET cycle_started_at=$2,sampled_at=$2 WHERE cycle_id=$1")
        .bind(id)
        .bind(start + Duration::minutes(3))
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        history(&pool, start, end).await.is_empty(),
        "old materialized minute must be excluded before refresh"
    );
    assert_eq!(
        history(&pool, start, start + Duration::hours(1)).await[0].3,
        Some(4.0)
    );
    sqlx::query("DELETE FROM online_samples WHERE cycle_id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        history(&pool, start, start + Duration::hours(1))
            .await
            .is_empty()
    );
    refresh(&pool).await.unwrap();
    assert!(
        history(&pool, start, start + Duration::hours(1))
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn deletion_of_one_parent_does_not_block_another_nodes_hour_refresh() {
    let pool = fixture().await;
    sqlx::query("INSERT INTO traffic_minute_totals(source,node_id,minute,tx,rx,samples,incomplete_samples) VALUES('users','n1','2026-01-01',1,2,1,0),('users','n2','2026-01-01',3,4,1,0)").execute(&pool).await.unwrap();
    let mut deletion = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM nodes WHERE id='n1'")
        .execute(&mut *deletion)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), refresh(&pool))
        .await
        .unwrap()
        .unwrap();
    let total: i64 =
        sqlx::query_scalar("SELECT tx::bigint FROM traffic_hour_totals WHERE node_id='n2'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(total, 3);
    deletion.commit().await.unwrap();
    refresh(&pool).await.unwrap();
    let stale: i64 =
        sqlx::query_scalar("SELECT count(*) FROM traffic_hour_totals WHERE node_id='n1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stale, 0);
}

#[tokio::test]
async fn different_dirty_jobs_in_one_chain_share_only_the_root_guard() {
    let pool = fixture().await;
    sqlx::raw_sql("INSERT INTO jobs(id,node_id,kind,status,stage,available_at,created_at,updated_at,finished_at,error_message) VALUES('root','n1','kick','failed','failed',now(),now(),now(),now(),'root error');
        INSERT INTO jobs(id,node_id,kind,status,stage,retry_of_job_id,available_at,created_at,updated_at) VALUES('leaf','n1','kick','queued','queued','root',now(),now(),now());").execute(&pool).await.unwrap();
    refresh(&pool).await.unwrap();
    sqlx::query("UPDATE jobs SET status='failed',error_message='leaf error',finished_at=now() WHERE id='leaf'").execute(&pool).await.unwrap();
    let mut owner = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended(current_database()||':'||current_schema()||':analytics:retry-root:root',0))")
        .execute(&mut *owner).await.unwrap();
    let mut other = pool.begin().await.unwrap();
    let claim = claim_on(&mut other, Queue::Jobs, None).await.unwrap();
    assert!(!claim.rows.is_empty());
    assert_eq!(
        refresh_queue_on(&mut other, Queue::Jobs, claim)
            .await
            .unwrap(),
        0
    );
    other.commit().await.unwrap();
    owner.rollback().await.unwrap();
    refresh(&pool).await.unwrap();
    let rows: Vec<(String, String, String, String)> =
        sqlx::query_as("SELECT root_id,leaf_id,status,last_error FROM job_retry_chain_state")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![(
            "root".into(),
            "leaf".into(),
            "failed".into(),
            "leaf error".into()
        )]
    );
    let members: i64 =
        sqlx::query_scalar("SELECT count(*) FROM job_retry_chain_members WHERE root_id='root'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(members, 2);
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM job_retry_state_dirty")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pending, 0);
}
