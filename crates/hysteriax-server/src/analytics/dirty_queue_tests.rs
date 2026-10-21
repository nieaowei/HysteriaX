use super::*;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;

async fn fixture() -> PgPool {
    let pool = crate::db::test_pool_with_max_connections(4).await;
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('u','User',now(),now())")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT 'n'||i,'Node '||i,'127.0.0.1',22,'root','private_key','unused','example.test',443,':443','hash'||i,'unused','unused','{}',now(),now() FROM generate_series(1,2)i").execute(&pool).await.unwrap();
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
async fn disable_legacy_lock(pool: &PgPool) {
    // Prove the new queue protocol independently of the old application lock.
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(pool).await.unwrap();
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
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(refresh_on(&mut tx, Some(10000)).await.unwrap(), 1024);
    tx.rollback().await.unwrap();
    for table in [
        "analytics_dirty_cycles",
        "traffic_dirty_hours",
        "online_dirty_minutes",
        "job_retry_state_dirty",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 300, "rollback of {table}");
    }
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(refresh_on(&mut tx, None).await.unwrap(), 1024);
    tx.commit().await.unwrap();
    for table in [
        "analytics_dirty_cycles",
        "traffic_dirty_hours",
        "online_dirty_minutes",
        "job_retry_state_dirty",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 44, "bounded acknowledgement of {table}");
    }
    refresh(&pool).await.unwrap();
}

#[tokio::test]
async fn another_refresher_skips_claimed_keys_and_history_overlays_the_held_source() {
    let pool = fixture().await;
    disable_legacy_lock(&pool).await;
    let (a, b) = (uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2));
    let start: DateTime<Utc> = "2026-01-01T12:00:00Z".parse().unwrap();
    cycle(&pool, a, start + Duration::seconds(10), 1).await;
    cycle(&pool, b, start + Duration::seconds(20), 3).await;
    let mut owner = pool.begin().await.unwrap();
    sqlx::query("SELECT cycle_id FROM analytics_dirty_cycles WHERE cycle_id=$1 FOR UPDATE")
        .bind(a)
        .execute(&mut *owner)
        .await
        .unwrap();
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
async fn a_source_change_waiting_on_a_claim_survives_the_old_ack() {
    let pool = fixture().await;
    disable_legacy_lock(&pool).await;
    let id = uuid::Uuid::new_v4();
    let start: DateTime<Utc> = "2026-01-01T12:00:00Z".parse().unwrap();
    cycle(&pool, id, start + Duration::seconds(10), 1).await;
    let original = generation(&pool, id).await;
    let mut owner = pool.begin().await.unwrap();
    sqlx::query("SELECT cycle_id FROM analytics_dirty_cycles WHERE cycle_id=$1 FOR UPDATE")
        .bind(id)
        .execute(&mut *owner)
        .await
        .unwrap();
    let mut writer = pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    let update = tokio::spawn(async move {
        sqlx::query("UPDATE online_samples SET connections='{\"u\":9}' WHERE cycle_id=$1")
            .bind(id)
            .execute(&mut *writer)
            .await
            .unwrap();
        writer.commit().await.unwrap();
    });
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT coalesce((SELECT wait_event_type='Lock' FROM pg_stat_activity WHERE pid=$1),false)").bind(pid).fetch_one(&pool).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    refresh_on(&mut owner, Some(1)).await.unwrap();
    owner.commit().await.unwrap();
    update.await.unwrap();
    assert!(generation(&pool, id).await > original);
    let cached:i64=sqlx::query_scalar("SELECT connections::bigint FROM online_cycle_metrics WHERE cycle_id=$1 AND scope_node_id=''").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(cached, 1);
    let before = history(&pool, start, start + Duration::hours(1)).await;
    assert_eq!(
        before[0].3,
        Some(9.0),
        "read-only fallback must replace a stale cached cycle"
    );
    refresh(&pool).await.unwrap();
    assert_eq!(
        history(&pool, start, start + Duration::hours(1)).await,
        before
    );
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
async fn migration_preserves_pending_keys_and_source_content() {
    let pool = crate::db::test_pool_through_version(1, 45).await;
    sqlx::raw_sql("INSERT INTO users(id,name,usage_bytes,created_at,updated_at) VALUES('retained-user','Retained',1234,now(),now());
        INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES('live-node','Retained node','127.0.0.1',22,'root','private_key','opaque ciphertext','example.test',443,':443','retained-hash','opaque token','opaque stats','opaque config',now(),now());
        INSERT INTO online_samples(cycle_id,node_id,sampled_at,cycle_started_at,status,traffic_status,connections,expected_nodes) VALUES('00000000-0000-0000-0000-000000000002','live-node','2026-01-01','2026-01-01','ok','ok','{\"retained-user\":3}',1);").execute(&pool).await.unwrap();
    const SOURCE: &str = "SELECT jsonb_build_object('users',(SELECT jsonb_agg(to_jsonb(u) ORDER BY id) FROM users u),'nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'samples',(SELECT jsonb_agg(to_jsonb(s) ORDER BY cycle_id,node_id) FROM online_samples s))";
    let source_before: Value = sqlx::query_scalar(SOURCE).fetch_one(&pool).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO analytics_dirty_cycles VALUES('00000000-0000-0000-0000-000000000001');
        INSERT INTO traffic_dirty_hours VALUES('users','old-node','2026-01-01');
        INSERT INTO online_dirty_minutes VALUES('old-node','2026-01-01');
        INSERT INTO job_retry_state_dirty VALUES('old-job');
        INSERT INTO credential_reference_dirty VALUES('node','old-node');",
    )
    .execute(&pool)
    .await
    .unwrap();
    let tables = [
        "analytics_dirty_cycles",
        "traffic_dirty_hours",
        "online_dirty_minutes",
        "job_retry_state_dirty",
        "credential_reference_dirty",
    ];
    let mut saved = Vec::new();
    for table in tables {
        let value: Value = sqlx::query_scalar(&format!(
            "SELECT jsonb_agg(to_jsonb(d) ORDER BY to_jsonb(d)::text) FROM {table} d"
        ))
        .fetch_one(&pool)
        .await
        .unwrap();
        saved.push(value);
    }
    let expected: usize = saved
        .iter()
        .map(|rows| rows.as_array().unwrap().len())
        .sum();
    crate::db::run_migrations(&pool).await.unwrap();
    let source_after: Value = sqlx::query_scalar(SOURCE).fetch_one(&pool).await.unwrap();
    assert_eq!(
        source_before, source_after,
        "migration must preserve usage, ciphertext and historical samples"
    );
    for (table, before) in tables.into_iter().zip(saved) {
        let value:Value=sqlx::query_scalar(&format!("SELECT jsonb_agg(to_jsonb(d)-'generation' ORDER BY (to_jsonb(d)-'generation')::text) FROM {table} d")).fetch_one(&pool).await.unwrap();
        assert_eq!(value, before, "{table}");
    }
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dirty_key_versions WHERE generation=1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count as usize, expected);
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
    sqlx::query("SELECT job_id FROM job_retry_state_dirty WHERE job_id='root' FOR UPDATE")
        .execute(&mut *owner)
        .await
        .unwrap();
    sqlx::query("SELECT root_id FROM analytics_retry_guards WHERE root_id='root' FOR UPDATE")
        .execute(&mut *owner)
        .await
        .unwrap();
    let mut other = pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *other)
        .await
        .unwrap();
    let worker = tokio::spawn(async move {
        refresh_on(&mut other, Some(1)).await.unwrap();
        other.commit().await.unwrap();
    });
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT coalesce((SELECT wait_event_type='Lock' AND query LIKE '%analytics_retry_guards%' FROM pg_stat_activity WHERE pid=$1),false)").bind(pid).fetch_one(&pool).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    refresh_on(&mut owner, Some(1)).await.unwrap();
    owner.commit().await.unwrap();
    worker.await.unwrap();
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
