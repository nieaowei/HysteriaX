use chrono::{DateTime, TimeDelta, Utc};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{analytics, background, credentials, db, security::SecretBox, state::AppState};

use super::{Counts, Cursor, GROUPS, Task, rows};

async fn fixture() -> (AppState, String) {
    let pool = db::test_pool_with_max_connections(8).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    let state = AppState::new(pool, secrets);
    let request = credentials::test_node_request(
        &state,
        json!({
            "name": "Operational retention node",
            "ssh_host": "127.0.0.1",
            "ssh_port": 1,
            "ssh_username": "root",
            "ssh_auth_type": "password",
            "ssh_secret": "test",
            "public_host": "example.test",
            "public_port": 443,
            "listen_addr": ":443"
        }),
    )
    .await;
    let (_, axum::Json(receipt)) =
        crate::api::nodes::create(axum::extract::State(state.clone()), axum::Json(request))
            .await
            .unwrap();
    (state, receipt["node"]["id"].as_str().unwrap().to_owned())
}

async fn create_extra_node(state: &AppState, name: &str) -> String {
    let request = credentials::test_node_request(
        state,
        json!({
            "name": name,
            "ssh_host": "127.0.0.2",
            "ssh_port": 2,
            "ssh_username": "root",
            "ssh_auth_type": "password",
            "ssh_secret": "test",
            "public_host": "example2.test",
            "public_port": 443,
            "listen_addr": ":443"
        }),
    )
    .await;
    let (_, axum::Json(receipt)) =
        crate::api::nodes::create(axum::extract::State(state.clone()), axum::Json(request))
            .await
            .unwrap();
    receipt["node"]["id"].as_str().unwrap().to_owned()
}

async fn run_page(pool: &PgPool, task: Task, now: DateTime<Utc>, shard: Option<u8>) -> Counts {
    let mut tx = pool.begin().await.unwrap();
    let mut cursor = Cursor::default();
    let counts = task
        .page(&mut tx, &mut cursor, now, shard, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    counts
}

async fn run_all_pages(pool: &PgPool, task: Task, now: DateTime<Utc>, shard: Option<u8>) -> Counts {
    let mut total = Counts::default();
    let mut cursor = Cursor::default();
    for _ in 0..32 {
        let mut tx = pool.begin().await.unwrap();
        let counts = task
            .page(&mut tx, &mut cursor, now, shard, None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        total.scanned += counts.scanned;
        total.eligible += counts.eligible;
        total.deleted += counts.deleted;
        for (reason, count) in counts.skipped {
            *total.skipped.entry(reason).or_default() += count;
        }
        if cursor.done {
            break;
        }
    }
    assert!(
        cursor.done,
        "retention sweep did not finish within page limit"
    );
    total
}

async fn insert_user(pool: &PgPool, id: &str, now: DateTime<Utc>) {
    sqlx::query("INSERT INTO users(id, name, created_at, updated_at) VALUES($1, $2, $3, $3)")
        .bind(id)
        .bind(id)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
}

#[allow(clippy::too_many_arguments)]
async fn insert_job(
    pool: &PgPool,
    id: &str,
    node_id: Option<&str>,
    kind: &str,
    status: &str,
    stage: &str,
    retry_of: Option<&str>,
    finished_at: Option<DateTime<Utc>>,
    target_revision: Option<i64>,
) {
    let ts = finished_at.unwrap_or_else(Utc::now);
    sqlx::query(
        "INSERT INTO jobs(id, kind, node_id, target_revision, status, stage, available_at, created_at, updated_at, finished_at, retry_of_job_id)
         VALUES($1, $2, $3, $4, $5, $6, $7, $7, $7, $8, $9)",
    )
    .bind(id)
    .bind(kind)
    .bind(node_id)
    .bind(target_revision)
    .bind(status)
    .bind(stage)
    .bind(ts)
    .bind(finished_at)
    .bind(retry_of)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_credential_and_ca(
    state: &AppState,
    id: &str,
    versions: &[i64],
    published_version: i64,
    now: DateTime<Utc>,
) {
    let pool = &state.pool;
    let max_v = *versions.iter().max().unwrap_or(&1);
    sqlx::query(
        "INSERT INTO credentials(id, name, kind, latest_version, created_at, updated_at)
         VALUES($1, $1, 'ca_identity', $2, $3, $3)",
    )
    .bind(id)
    .bind(max_v)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    let payload = state.secrets.encrypt("{}").unwrap();
    for version in versions {
        sqlx::query(
            "INSERT INTO credential_versions(credential_id, version, payload_enc, created_at)
             VALUES($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(*version)
        .bind(&payload)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO mtls_authorities(credential_id, source, published_version)
         VALUES($1, 'generated', $2)",
    )
    .bind(id)
    .bind(published_version)
    .execute(pool)
    .await
    .unwrap();
    for version in versions {
        let ca_state = if *version == published_version {
            "published"
        } else {
            "retired"
        };
        sqlx::query(
            "INSERT INTO mtls_ca_generations(ca_id, ca_version, state, expires_at)
             VALUES($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(*version)
        .bind(ca_state)
        .bind(now + TimeDelta::days(30))
        .execute(pool)
        .await
        .unwrap();
    }
}

#[test]
fn monitoring_retention_group_is_sharded_without_traffic_page_gate() {
    let monitoring = GROUPS
        .iter()
        .find(|group| group.key == "monitoring-history-retention")
        .unwrap();
    assert!(monitoring.sharded);
    assert!(!monitoring.traffic_gate);
    assert_eq!(GROUPS.len(), 5);
}

#[tokio::test]
async fn online_samples_sharded_cycle_retention_triggers_statement_analytics_refresh() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    let old = now - TimeDelta::days(35);

    let cycle_big = Uuid::new_v4();
    let cycle_big_shard: i32 = sqlx::query_scalar("SELECT (hashtext($1::text) & 3)")
        .bind(cycle_big)
        .fetch_one(pool)
        .await
        .unwrap();
    let mut cycle_other = Uuid::new_v4();
    loop {
        let s: i32 = sqlx::query_scalar("SELECT (hashtext($1::text) & 3)")
            .bind(cycle_other)
            .fetch_one(pool)
            .await
            .unwrap();
        if s != cycle_big_shard {
            break;
        }
        cycle_other = Uuid::new_v4();
    }

    sqlx::query(
        "INSERT INTO nodes(
            id, name, ssh_host, ssh_port, ssh_username, ssh_host_fingerprint,
            public_host, public_port, listen_addr, tls_sni, tls_skip_verify,
            node_token_hash, node_token_enc, traffic_stats_secret_enc,
            desired_config_enc, desired_revision, deployed_config_enc, deployed_revision,
            deployed_content_sha256, state, last_seen_at, last_sample_at, proxy_probe_url,
            traffic_stats_port, created_at, updated_at, ssh_credential_id, ssh_credential_version,
            published_connection, mtls_state, mtls_error, mtls_generation, mtls_activation_id,
            mtls_recovery_configured, traffic_capture_mode
         )
         SELECT
            'batch-node-' || g, 'Batch Node ' || g, ssh_host, ssh_port, ssh_username, ssh_host_fingerprint,
            public_host, public_port, listen_addr, tls_sni, tls_skip_verify,
            'tok-hash-' || g, node_token_enc, traffic_stats_secret_enc,
            desired_config_enc, desired_revision, deployed_config_enc, deployed_revision,
            deployed_content_sha256, state, last_seen_at, last_sample_at, proxy_probe_url,
            traffic_stats_port, created_at, updated_at, ssh_credential_id, ssh_credential_version,
            published_connection, mtls_state, mtls_error, mtls_generation, mtls_activation_id,
            mtls_recovery_configured, traffic_capture_mode
         FROM nodes CROSS JOIN generate_series(1, 4151) g
         WHERE id = $1",
    )
    .bind(&node)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO online_samples(cycle_id, node_id, sampled_at, status, cycle_started_at, expected_nodes, pending)
         SELECT $1, 'batch-node-' || g, $2 + make_interval(secs => g::double precision * 0.001), 'online', $2, 4152, false
         FROM generate_series(1, 4150) g",
    )
    .bind(cycle_big)
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    let locked_id: i64 = sqlx::query_scalar(
        "INSERT INTO online_samples(cycle_id, node_id, sampled_at, status, cycle_started_at, expected_nodes, pending)
         VALUES($1, 'batch-node-4151', $2 + interval '10 seconds', 'online', $2, 4152, false)
         RETURNING id",
    )
    .bind(cycle_big)
    .bind(old)
    .fetch_one(pool)
    .await
    .unwrap();

    // Pending rows and rows in a different shard must survive shard retention.
    sqlx::query(
        "INSERT INTO online_samples(cycle_id, node_id, sampled_at, status, cycle_started_at, expected_nodes, pending)
         VALUES($1, $2, $3, 'online', $3, 1, true),
               ($4, $2, $3, 'online', $3, 1, false)",
    )
    .bind(cycle_big)
    .bind(&node)
    .bind(old)
    .bind(cycle_other)
    .execute(pool)
    .await
    .unwrap();

    analytics::refresh(pool).await.unwrap();
    let initial_metrics: i64 =
        sqlx::query_scalar("SELECT count(*) FROM online_cycle_metrics WHERE cycle_id = $1")
            .bind(cycle_big)
            .fetch_one(pool)
            .await
            .unwrap();
    assert!(initial_metrics >= 1);

    // Hold FOR UPDATE lock on one row during page 1 to verify SKIP LOCKED.
    let mut locker = pool.acquire().await.unwrap();
    sqlx::query("BEGIN").execute(&mut *locker).await.unwrap();
    sqlx::query("SELECT 1 FROM online_samples WHERE id = $1 FOR UPDATE")
        .bind(locked_id)
        .execute(&mut *locker)
        .await
        .unwrap();

    let mut cursor = Cursor::default();
    let mut tx1 = pool.begin().await.unwrap();
    let page1 = Task::OnlineSamples
        .page(
            &mut tx1,
            &mut cursor,
            now,
            Some(cycle_big_shard as u8),
            None,
        )
        .await
        .unwrap();
    tx1.commit().await.unwrap();
    assert_eq!(page1.deleted, 4096);
    assert!(!cursor.done);

    let mut tx2 = pool.begin().await.unwrap();
    let page2 = Task::OnlineSamples
        .page(
            &mut tx2,
            &mut cursor,
            now,
            Some(cycle_big_shard as u8),
            None,
        )
        .await
        .unwrap();
    tx2.commit().await.unwrap();
    assert_eq!(page2.deleted, 54);
    sqlx::query("COMMIT").execute(&mut *locker).await.unwrap();
    drop(locker);

    let remaining_locked_and_pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM online_samples WHERE cycle_id = $1")
            .bind(cycle_big)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(remaining_locked_and_pending, 2);

    let other_shard_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM online_samples WHERE cycle_id = $1")
            .bind(cycle_other)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(other_shard_rows, 1);

    // Delete pending + now-unlocked row through normal sweep and verify statement trigger refreshes analytics.
    sqlx::query("UPDATE online_samples SET pending = false WHERE cycle_id = $1 AND pending")
        .bind(cycle_big)
        .execute(pool)
        .await
        .unwrap();
    let sweep = run_all_pages(pool, Task::OnlineSamples, now, Some(cycle_big_shard as u8)).await;
    assert_eq!(sweep.deleted, 2);

    analytics::refresh(pool).await.unwrap();
    let final_metrics: i64 =
        sqlx::query_scalar("SELECT count(*) FROM online_cycle_metrics WHERE cycle_id = $1")
            .bind(cycle_big)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(final_metrics, 0);
}

#[tokio::test]
async fn proxy_probe_samples_sharded_retention_updates_minute_totals_via_statement_trigger() {
    let (state, node_a) = fixture().await;
    let node_b = create_extra_node(&state, "Other shard node").await;
    let pool = &state.pool;
    let now = Utc::now();
    let old_m1 = now - TimeDelta::days(35);
    let old_m2 = old_m1 - TimeDelta::minutes(5);
    let recent = now - TimeDelta::days(1);

    let shard_a: i32 = sqlx::query_scalar("SELECT (hashtext($1) & 3)")
        .bind(&node_a)
        .fetch_one(pool)
        .await
        .unwrap();

    // Minute 1: 2 old ok/failed + 1 recent ok -> deletion shrinks attempts from 3 to 1.
    // Minute 2: 2 old ok/failed -> deletion deletes entire probe_minute_totals row.
    sqlx::query(
        "INSERT INTO proxy_probe_samples(node_id, revision, sampled_at, status)
         VALUES($1, 1, $2, 'ok'),
               ($1, 1, $2 + interval '10 seconds', 'failed'),
               ($1, 1, $4, 'ok'),
               ($1, 1, $3, 'ok'),
               ($1, 1, $3 + interval '10 seconds', 'failed')",
    )
    .bind(&node_a)
    .bind(old_m1)
    .bind(old_m2)
    .bind(recent)
    .execute(pool)
    .await
    .unwrap();

    // Align one rollup row intentionally lower to ensure attempts <= d.attempts deletes it cleanly.
    sqlx::query(
        "UPDATE probe_minute_totals SET attempts = 1, successes = 1
         WHERE node_id = $1 AND minute = hysteriax_minute($2)",
    )
    .bind(&node_a)
    .bind(old_m2)
    .execute(pool)
    .await
    .unwrap();

    // Verify 31d proxy probe retention never touches node_network_samples (60d).
    sqlx::query(
        "INSERT INTO node_network_samples(node_id, period_id, delta_tx, delta_rx, sampled_at)
         VALUES($1, 'fixed', 100, 200, $2)",
    )
    .bind(&node_a)
    .bind(old_m1)
    .execute(pool)
    .await
    .unwrap();

    // Verify active node execution lease causes PROXY_PROBES (node_lock = true) to yield.
    sqlx::query(
        "INSERT INTO resource_leases(resource_key, owner, expires_at)
         VALUES('node:' || $1, 'lease-holder', $2)
         ON CONFLICT (resource_key) DO UPDATE SET owner = EXCLUDED.owner, expires_at = EXCLUDED.expires_at",
    )
    .bind(&node_a)
    .bind(now + TimeDelta::minutes(5))
    .execute(pool)
    .await
    .unwrap();

    let locked = run_page(
        pool,
        Task::Rows(rows::PROXY_PROBES),
        now,
        Some(shard_a as u8),
    )
    .await;
    assert_eq!(locked.deleted, 0);
    assert_eq!(
        locked.skipped.get("row_or_parent_locked_or_gone").copied(),
        Some(4)
    );

    sqlx::query(
        "UPDATE resource_leases SET owner = NULL, expires_at = NULL WHERE resource_key = 'node:' || $1",
    )
    .bind(&node_a)
    .execute(pool)
    .await
    .unwrap();

    // Running on a non-matching shard deletes nothing for node_a if node_b isn't present there.
    let other_shard = ((shard_a + 1) & 3) as u8;
    let wrong_shard =
        run_all_pages(pool, Task::Rows(rows::PROXY_PROBES), now, Some(other_shard)).await;
    assert_eq!(wrong_shard.deleted, 0);

    // Put an expired probe on node_b too so we verify node_a's shard runs cleanly even with node_b rows.
    sqlx::query("INSERT INTO proxy_probe_samples(node_id, revision, sampled_at, status) VALUES($1, 1, $2, 'ok')")
        .bind(&node_b)
        .bind(old_m1)
        .execute(pool)
        .await
        .unwrap();

    let cleaned = run_all_pages(
        pool,
        Task::Rows(rows::PROXY_PROBES),
        now,
        Some(shard_a as u8),
    )
    .await;
    assert!(cleaned.deleted >= 4);

    let remaining_samples: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxy_probe_samples WHERE node_id = $1")
            .bind(&node_a)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(remaining_samples, 1);

    let remaining_m2: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM probe_minute_totals WHERE node_id = $1 AND minute = hysteriax_minute($2)",
    )
    .bind(&node_a)
    .bind(old_m2)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(remaining_m2, 0);

    let network_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM node_network_samples WHERE node_id = $1")
            .bind(&node_a)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(network_count, 1);
}

#[tokio::test]
async fn monitoring_probe_tokens_cleanup_and_rollback_safety() {
    let (state, node) = fixture().await;
    let node_active = create_extra_node(&state, "Active token node").await;
    let pool = &state.pool;
    let now = Utc::now();

    sqlx::query(
        "INSERT INTO monitoring_probe_tokens(node_id, token_hash, expires_at)
         VALUES($1, 'expired-tok', $3), ($2, 'active-tok', $4)",
    )
    .bind(&node)
    .bind(&node_active)
    .bind(now - TimeDelta::minutes(5))
    .bind(now + TimeDelta::minutes(5))
    .execute(pool)
    .await
    .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let counts = Task::Rows(rows::MONITORING_TOKENS)
        .page(&mut tx, &mut Cursor::default(), now, None, None)
        .await
        .unwrap();
    assert_eq!(counts.deleted, 1);
    tx.rollback().await.unwrap();

    let after_rollback: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM monitoring_probe_tokens WHERE node_id IN ($1, $2)",
    )
    .bind(&node)
    .bind(&node_active)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(after_rollback, 2);

    let committed = run_all_pages(pool, Task::Rows(rows::MONITORING_TOKENS), now, None).await;
    assert_eq!(committed.deleted, 1);
    let remaining_hash: String = sqlx::query_scalar(
        "SELECT token_hash FROM monitoring_probe_tokens WHERE node_id IN ($1, $2)",
    )
    .bind(&node)
    .bind(&node_active)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(remaining_hash, "active-tok");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn discarded_traffic_batches_stay_at_14d_and_unblock_0048_at_60d() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    let recent_discarded = now - TimeDelta::days(20);
    let old_discarded = now - TimeDelta::days(65);
    let cycle_recent = Uuid::new_v4();
    let cycle_old = Uuid::new_v4();

    let work_20d = format!("{cycle_recent}:{node}");
    let work_65d = format!("{cycle_old}:{node}");

    for (sample_id, cycle, ts) in [
        (work_20d.as_str(), cycle_recent, recent_discarded),
        (work_65d.as_str(), cycle_old, old_discarded),
    ] {
        sqlx::query(
            "INSERT INTO sampling_cycles(id, kind, period_start, deadline_at, expected_nodes)
             VALUES($1, 'traffic', $2 - interval '1 minute', $2, 1)",
        )
        .bind(cycle)
        .bind(ts)
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO traffic_sample_batches(
                sample_id, node_id, cycle_id, state, sampled_at, completed_at,
                capture_mode, clear_requested_at, clear_owner, clear_work_generation,
                clear_node_generation, clear_epoch
             ) VALUES($1, $2, $3, 'discarded', $4, $4, 'interval', $4, 'owner-1', 1, 1, 'epoch-1')",
        )
        .bind(sample_id)
        .bind(&node)
        .bind(cycle)
        .bind(ts)
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO background_work(id, kind, cycle_id, node_id, state, available_at, deadline_at, finished_at)
             VALUES($1, 'traffic', $2, $3, 'complete', $4, $4, $4)",
        )
        .bind(sample_id)
        .bind(cycle)
        .bind(&node)
        .bind(ts)
        .execute(pool)
        .await
        .unwrap();

        sqlx::query("INSERT INTO resource_leases(resource_key) VALUES('work:' || $1)")
            .bind(sample_id)
            .execute(pool)
            .await
            .unwrap();
    }

    // 0048 must retain BOTH discarded batches & their cycles/work/leases while batch row exists.
    background::retention::pass(&state).await.unwrap();
    let batches_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM traffic_sample_batches WHERE node_id = $1")
            .bind(&node)
            .fetch_one(pool)
            .await
            .unwrap();
    let work_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM background_work WHERE id IN ($1, $2)")
            .bind(&work_20d)
            .bind(&work_65d)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!((batches_before, work_before), (2, 2));

    // Complete-batch rule skips discarded; BATCHES_DISCARDED deletes only 65d row.
    let complete_pass = run_all_pages(pool, Task::Rows(rows::BATCHES), now, None).await;
    assert_eq!(complete_pass.deleted, 0);
    let discarded_pass = run_all_pages(pool, Task::Rows(rows::BATCHES_DISCARDED), now, None).await;
    assert_eq!(discarded_pass.deleted, 1);

    let remaining_batch: String =
        sqlx::query_scalar("SELECT sample_id FROM traffic_sample_batches WHERE node_id = $1")
            .bind(&node)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(remaining_batch, work_20d);

    // Once 65d batch is pruned, 0048 reclaims its sampling_cycles, background_work, and resource_leases.
    sqlx::query("UPDATE background_work SET available_at = clock_timestamp() WHERE id = 'maintenance:sampling-history:retention'")
        .execute(pool)
        .await
        .unwrap();
    let handle = tokio::runtime::Handle::current();
    let next_state = state.clone();
    std::thread::spawn(move || handle.block_on(background::retention::pass(&next_state)))
        .join()
        .unwrap()
        .unwrap();
    let old_work: i64 = sqlx::query_scalar("SELECT count(*) FROM background_work WHERE id = $1")
        .bind(&work_65d)
        .fetch_one(pool)
        .await
        .unwrap();
    let old_lease: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource_leases WHERE resource_key = 'work:' || $1",
    )
    .bind(&work_65d)
    .fetch_one(pool)
    .await
    .unwrap();
    let old_cycle: i64 = sqlx::query_scalar("SELECT count(*) FROM sampling_cycles WHERE id = $1")
        .bind(cycle_old)
        .fetch_one(pool)
        .await
        .unwrap();
    let kept_work: i64 = sqlx::query_scalar("SELECT count(*) FROM background_work WHERE id = $1")
        .bind(&work_20d)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!((old_work, old_lease, old_cycle, kept_work), (0, 0, 0, 1));
}

#[tokio::test]
async fn traffic_baselines_retains_assigned_active_and_recent_entries() {
    let (state, node_idle) = fixture().await;
    let node_active = create_extra_node(&state, "Active batch node").await;
    let pool = &state.pool;
    let now = Utc::now();
    let old = now - TimeDelta::days(65);

    insert_user(pool, "u-del", now).await;
    insert_user(pool, "u-assigned", now).await;
    insert_user(pool, "u-recent", now).await;

    sqlx::query(
        "INSERT INTO node_assignments(user_id, node_id, created_at) VALUES('u-assigned', $1, $2)",
    )
    .bind(&node_idle)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO traffic_sample_batches(sample_id, node_id, state, sampled_at)
         VALUES('capturing-b', $1, 'capturing', $2)",
    )
    .bind(&node_active)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO traffic_baselines(node_id, user_id, instance_id, tx_total, rx_total, sampled_at)
         VALUES($1, 'u-del', 'inst-1', 10, 20, $3),
               ($1, 'u-assigned', 'inst-1', 10, 20, $3),
               ($1, 'u-recent', 'inst-1', 10, 20, $4),
               ($2, 'u-del', 'inst-1', 10, 20, $3)",
    )
    .bind(&node_idle)
    .bind(&node_active)
    .bind(old)
    .bind(now - TimeDelta::days(10))
    .execute(pool)
    .await
    .unwrap();

    // Non-zero shards skip immediately; shard 0 prunes old unassigned rows.
    let s1 = run_all_pages(pool, Task::TrafficBaselines, now, Some(1)).await;
    assert_eq!(s1.deleted, 0);
    let s0 = run_all_pages(pool, Task::TrafficBaselines, now, Some(0)).await;
    assert_eq!(s0.deleted, 1);

    let remaining: Vec<(String, String)> =
        sqlx::query_as("SELECT node_id, user_id FROM traffic_baselines ORDER BY node_id, user_id")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(remaining.len(), 3);
    assert!(!remaining.contains(&(node_idle, "u-del".to_owned())));
}

#[tokio::test]
async fn execution_operations_cascades_rechecks_and_unblocks_job_chains() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    let old = now - TimeDelta::days(95);

    insert_job(
        pool,
        "job-done",
        Some(&node),
        "deploy",
        "succeeded",
        "completed",
        None,
        Some(old),
        None,
    )
    .await;
    insert_job(
        pool,
        "job-blocked",
        Some(&node),
        "deploy",
        "succeeded",
        "completed",
        None,
        Some(old),
        None,
    )
    .await;
    insert_job(
        pool,
        "job-running",
        Some(&node),
        "deploy",
        "running",
        "executing",
        None,
        None,
        None,
    )
    .await;

    sqlx::query(
        "INSERT INTO resource_leases(resource_key, blocked)
         VALUES('job:job-done', false), ('job:job-blocked', true), ('job:job-running', false)",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO execution_operations(operation_id, resource_key, owner, generation, work_id, step, state, intent, created_at, updated_at)
         VALUES('op-del', 'job:job-done', 'inst-1', 1, 'job-done', 'apply', 'verified', '{}', $1, $1),
               ('op-blocked', 'job:job-blocked', 'inst-1', 1, 'job-blocked', 'apply', 'verified', '{}', $1, $1),
               ('op-active-job', 'job:job-running', 'inst-1', 1, 'job-running', 'apply', 'verified', '{}', $1, $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO execution_recheck_requests(namespace, request_digest, created_at)
         VALUES('ns-orphan', 'dig-1', $1), ('ns-blocked', 'dig-2', $1), ('ns-stale', 'dig-3', $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO execution_rechecks(id, request_namespace, request_digest, operation_id, prepared, state, created_at, updated_at)
         VALUES('rc-1', 'ns-orphan', 'dig-1', 'op-del', '{}', 'verified', $1, $1),
               ('rc-keep', 'ns-blocked', 'dig-2', 'op-blocked', '{}', 'verified', $1, $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    // JobChains cannot delete job-done while execution_operations still references work_id='job-done'.
    let jobs_before = run_all_pages(pool, Task::JobChains, now, None).await;
    assert_eq!(jobs_before.deleted, 0);

    let ops = run_all_pages(pool, Task::ExecutionOperations, now, None).await;
    assert_eq!(ops.deleted, 1);

    // ns-orphan should be gone in the exact same transaction as op-del; ns-blocked must stay because rc-keep remains.
    let namespaces: Vec<String> =
        sqlx::query_scalar("SELECT namespace FROM execution_recheck_requests ORDER BY namespace")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(
        namespaces,
        vec!["ns-blocked".to_owned(), "ns-stale".to_owned()]
    );

    // Orphaned ns-stale without any recheck row is pruned by ExecutionRecheckRequests.
    let reqs = run_all_pages(pool, Task::ExecutionRecheckRequests, now, None).await;
    assert_eq!(reqs.deleted, 1);
    let remaining_ns: Vec<String> =
        sqlx::query_scalar("SELECT namespace FROM execution_recheck_requests ORDER BY namespace")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(remaining_ns, vec!["ns-blocked".to_owned()]);

    // Now JobChains cleans up job-done and its job:job-done released lease!
    let jobs_after = run_all_pages(pool, Task::JobChains, now, None).await;
    assert_eq!(jobs_after.deleted, 1);
    let done_count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE id = 'job-done'")
        .fetch_one(pool)
        .await
        .unwrap();
    let lease_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource_leases WHERE resource_key = 'job:job-done'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!((done_count, lease_count), (0, 0));
}

#[tokio::test]
async fn job_chains_batches_multiple_roots_deletes_job_leases_and_cleans_auth_changes() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    let old = now - TimeDelta::days(95);

    insert_job(
        pool,
        "r1",
        Some(&node),
        "deploy",
        "succeeded",
        "completed",
        None,
        Some(old),
        None,
    )
    .await;
    insert_job(
        pool,
        "r2",
        Some(&node),
        "deploy",
        "failed",
        "completed",
        None,
        Some(old + TimeDelta::seconds(1)),
        None,
    )
    .await;
    insert_job(
        pool,
        "r2-retry",
        Some(&node),
        "deploy",
        "succeeded",
        "completed",
        Some("r2"),
        Some(old + TimeDelta::seconds(2)),
        None,
    )
    .await;
    insert_job(
        pool,
        "r3-incomplete-auth",
        Some(&node),
        "auth",
        "succeeded",
        "completed",
        None,
        Some(old + TimeDelta::seconds(3)),
        None,
    )
    .await;
    insert_job(
        pool,
        "r4-excl-auth",
        Some(&node),
        "auth",
        "succeeded",
        "completed",
        None,
        Some(old + TimeDelta::seconds(4)),
        None,
    )
    .await;

    sqlx::query(
        "INSERT INTO resource_leases(resource_key) VALUES('job:r1'), ('job:r2'), ('job:r2-retry'), ('job:r3-incomplete-auth')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO authorization_groups(id, name, created_at, updated_at) VALUES('grp-1', 'G1', $1, $1)",
    )
    .bind(now)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO authorization_changes(job_id, target_type, target_id, action, actor_id, input, complete)
         VALUES('r1', 'group', 'grp-1', 'update', 'admin', '{}', true),
               ('r3-incomplete-auth', 'group', 'grp-1', 'update', 'admin', '{}', false),
               ('r4-excl-auth', 'group', 'grp-1', 'update', 'admin', '{}', true)",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO authorization_exclusions(job_id, group_id) VALUES('r4-excl-auth', 'grp-1')",
    )
    .execute(pool)
    .await
    .unwrap();

    // One page batches r1 + (r2, r2-retry) = 3 deleted jobs, while leaving r3 & r4 protected.
    let counts = run_page(pool, Task::JobChains, now, None).await;
    assert_eq!(counts.deleted, 3);

    let remaining_jobs: Vec<String> = sqlx::query_scalar("SELECT id FROM jobs ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap();
    assert_eq!(
        remaining_jobs,
        vec!["r3-incomplete-auth".to_owned(), "r4-excl-auth".to_owned()]
    );

    let remaining_leases: Vec<String> = sqlx::query_scalar(
        "SELECT resource_key FROM resource_leases WHERE resource_key LIKE 'job:%' ORDER BY resource_key",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(remaining_leases, vec!["job:r3-incomplete-auth".to_owned()]);
}

#[tokio::test]
async fn non_sampling_work_and_kick_requests_retention() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    let old = now - TimeDelta::days(35);

    insert_credential_and_ca(&state, "ca-rot", &[1, 2], 2, now).await;
    sqlx::query(
        "INSERT INTO credentials(id, name, kind, created_at, updated_at)
         VALUES('batch-cred', 'batch', 'ca_identity', $1, $1)",
    )
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO credential_versions(credential_id, version, payload_enc, created_at)
         VALUES('batch-cred', 1, $1, $2)",
    )
    .bind(state.secrets.encrypt("{}").unwrap())
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO credential_batches(id, credential_id, version, created_at)
         VALUES('rot-batch', 'batch-cred', 1, $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mtls_rotations(id, ca_id, ca_version, phase, batch_id, next_action_at, created_at, updated_at)
         VALUES('rot-done', 'ca-rot', 1, 'complete', 'rot-batch', $1, $1, $1),
               ('rot-active', 'ca-rot', 2, 'trust', 'rot-batch', $1, $1, $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO background_work(id, kind, state, available_at, finished_at, payload)
         VALUES('w-access', 'access-revocation', 'complete', $1, $1, '{}'),
               ('w-limit', 'node-limit-dispatch', 'failed', $1, $1, '{}'),
               ('w-rot-done', 'mtls-rotation-trust', 'complete', $1, $1, '{\"rotation_id\":\"rot-done\"}'),
               ('w-rot-active', 'mtls-rotation-trust', 'complete', $1, $1, '{\"rotation_id\":\"rot-active\"}'),
               ('maintenance:history-retention:operations-history-retention', 'maintenance:history-retention:operations-history-retention', 'complete', $1, $1, '{}')",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO resource_leases(resource_key) VALUES('work:w-access'), ('work:w-rot-active')",
    )
    .execute(pool)
    .await
    .unwrap();

    let work_counts = run_all_pages(pool, Task::NonSamplingWork, now, None).await;
    assert_eq!(work_counts.deleted, 3);

    let remaining_work: Vec<String> =
        sqlx::query_scalar("SELECT id FROM background_work ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(
        remaining_work,
        vec![
            "maintenance:history-retention:operations-history-retention".to_owned(),
            "w-rot-active".to_owned(),
        ]
    );
    let access_lease: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource_leases WHERE resource_key = 'work:w-access'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(access_lease, 0);

    // Kick requests retention.
    insert_job(
        pool,
        "kick-running",
        Some(&node),
        "kick",
        "running",
        "executing",
        None,
        None,
        None,
    )
    .await;
    sqlx::query(
        "INSERT INTO kick_requests(node_id, user_id, reasons, state, latest_job_id, updated_at)
         VALUES($1, 'u-del-1', '[]', 'completed', NULL, $2),
               ($1, 'u-del-2', '[]', 'cancelled', NULL, $2),
               ($1, 'u-active', '[]', 'active', NULL, $2),
               ($1, 'u-job-live', '[]', 'completed', 'kick-running', $2)",
    )
    .bind(&node)
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    let kick_counts = run_all_pages(pool, Task::KickRequests, now, None).await;
    assert_eq!(kick_counts.deleted, 2);
    let remaining_kicks: Vec<String> =
        sqlx::query_scalar("SELECT user_id FROM kick_requests WHERE node_id = $1 ORDER BY user_id")
            .bind(&node)
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(
        remaining_kicks,
        vec!["u-active".to_owned(), "u-job-live".to_owned()]
    );
}

#[tokio::test]
async fn metadata_operational_tables_cache_tombstones_and_orphan_cleanups() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    let old = now - TimeDelta::days(35);
    let recent = now - TimeDelta::days(2);
    let digest = "a".repeat(64);
    let enc = state.secrets.encrypt("{}").unwrap();

    sqlx::query(
        "INSERT INTO management_receipts(actor_id, operation, idempotency_key, request_digest, response_enc, created_at)
         VALUES('adm', 'op1', 'k-old', $1, $2, $3), ('adm', 'op1', 'k-new', $1, $2, $4)",
    )
    .bind(&digest)
    .bind(&enc)
    .bind(old)
    .bind(recent)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        run_all_pages(pool, Task::ManagementReceipts, now, None)
            .await
            .deleted,
        1
    );

    sqlx::query(
        "INSERT INTO mtls_operations(actor_id, idempotency_key, operation, request_digest, receipt, created_at)
         VALUES('adm', 'm-old', 'rot', $1, '{}', $2), ('adm', 'm-new', 'rot', $1, '{}', $3)",
    )
    .bind(&digest)
    .bind(old)
    .bind(recent)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        run_all_pages(pool, Task::MtlsOperations, now, None)
            .await
            .deleted,
        1
    );

    // CacheTombstones: deletes deleted=true non-quota_rank >=30d, preserves quota_rank, live, and recent.
    sqlx::query(
        "INSERT INTO cache_entity_versions(kind, entity_id, generation, deleted, bucket, bucket_generation, updated_at)
         VALUES('nodes', 'tomb-old', 1, true, 0, 1, $1),
               ('quota_rank', 'tomb-qr-old', 1, true, 0, 1, $1),
               ('users', 'live-old', 1, false, 0, 1, $1),
               ('nodes', 'tomb-recent', 1, true, 0, 1, $2)
         ON CONFLICT (kind, entity_id) DO UPDATE SET updated_at = EXCLUDED.updated_at, deleted = EXCLUDED.deleted",
    )
    .bind(old)
    .bind(recent)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        run_all_pages(pool, Task::CacheTombstones, now, None)
            .await
            .deleted,
        1
    );

    // Trigger check: updating node/cache flush populates updated_at >= previous.
    let updated_at_present: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM cache_entity_versions WHERE kind = 'quota_rank' AND entity_id = 'tomb-qr-old'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(updated_at_present, 1);

    // OrphanResourceLeases & OrphanJobNodeKickTotals.
    sqlx::query(
        "INSERT INTO resource_leases(resource_key, blocked)
         VALUES('node:' || $1, false),
               ('node:missing-n', false),
               ('job:missing-j', false),
               ('work:missing-w', false),
               ('node:blocked-n', true)
         ON CONFLICT (resource_key) DO NOTHING",
    )
    .bind(&node)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        run_all_pages(pool, Task::OrphanResourceLeases, now, None)
            .await
            .deleted,
        3
    );
    let kept_live_and_blocked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource_leases WHERE resource_key IN ('node:' || $1, 'node:blocked-n')",
    )
    .bind(&node)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(kept_live_and_blocked, 2);

    sqlx::query(
        "INSERT INTO job_node_kick_totals(node_id, pending, pending_limit, failed_limit)
         VALUES('orphan-zero', 0, 0, 0),
               ('orphan-nonzero', 1, 0, 0),
               ($1, 0, 0, 0)",
    )
    .bind(&node)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        run_all_pages(pool, Task::OrphanJobNodeKickTotals, now, None)
            .await
            .deleted,
        1
    );
}

#[tokio::test]
async fn credential_dns_mtls_and_config_version_retention_chains() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    let old = now - TimeDelta::days(95);
    let enc = state.secrets.encrypt("{}").unwrap();

    insert_user(pool, "u-cred", now).await;
    sqlx::query(
        "INSERT INTO user_credentials(id, user_id, kind, secret_hash, secret_enc, created_at, revoked_at)
         VALUES('uc-rev-old', 'u-cred', 'connection', 'h1', $1, $2, $2),
               ('uc-live', 'u-cred', 'connection', 'h2', $1, $2, NULL)",
    )
    .bind(&enc)
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        run_all_pages(pool, Task::Rows(rows::USER_CREDENTIALS_REVOKED), now, None)
            .await
            .deleted,
        1
    );

    insert_job(
        pool,
        "job-term",
        Some(&node),
        "deploy",
        "succeeded",
        "completed",
        None,
        Some(old),
        None,
    )
    .await;
    sqlx::query(
        "INSERT INTO credential_application_receipts(job_id, source, execution, stage, result, created_at)
         VALUES('job-term', '{}', '{}', 'done', '{}', $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        run_all_pages(pool, Task::Rows(rows::CREDENTIAL_APP_RECEIPTS), now, None)
            .await
            .deleted,
        1
    );

    // CredentialBatches + DNS entities + mTLS entities.
    insert_credential_and_ca(&state, "ca-main", &[1, 2], 2, now).await;
    sqlx::query(
        "INSERT INTO credentials(id, name, kind, latest_version, created_at, updated_at)
         VALUES('dns-cred', 'dns', 'dns', 1, $1, $1),
               ('probe-cred', 'probe', 'tls_identity', 2, $1, $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO credential_versions(credential_id, version, payload_enc, created_at)
         VALUES('dns-cred', 1, $1, $2), ('probe-cred', 1, $1, $2), ('probe-cred', 2, $1, $2)",
    )
    .bind(&enc)
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO dns_connections(id, name, provider, credential_id, credential_version)
         VALUES('dns-conn', 'CF', 'cloudflare', 'dns-cred', 1)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO dns_zones(id, connection_id, provider_zone_id, name)
         VALUES('dns-zone', 'dns-conn', 'z1', 'example.test')",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO dns_records(id, zone_id, name, record_type, content, deleted_at, created_at, updated_at)
         VALUES('dns-rec-del', 'dns-zone', 'a', 'A', '1.1.1.1', $1, $1, $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO dns_operations(id, idempotency_key, request_sha256, job_id, connection_id, credential_version, resource_type, resource_id, action, payload, applied_at, created_at)
         VALUES('dns-op-old', 'k1', 's1', 'job-term', 'dns-conn', 1, 'record', 'dns-rec-del', 'delete', '{}', $1, $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO credential_batches(id, credential_id, version, created_at)
         VALUES('cb-rot', 'ca-main', 1, $1), ('cb-del', 'dns-cred', 1, $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO dns_credential_batch_items(batch_id, connection_id, job_id, expected_revision)
         VALUES('cb-del', 'dns-conn', 'job-term', 1)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mtls_rotations(id, ca_id, ca_version, phase, batch_id, next_action_at, created_at, updated_at)
         VALUES('rot-1', 'ca-main', 1, 'complete', 'cb-rot', $1, $1, $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mtls_node_operations(job_id, node_id, ca_id, ca_version, rotation_id, stage, desired_revision, created_at)
         VALUES('job-term', $1, 'ca-main', 1, 'rot-1', 'trust', 1, $2)",
    )
    .bind(&node)
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mtls_trust_cleanup(ca_id, ca_version, next_action_at, completed_at)
         VALUES('ca-main', 1, $1, $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mtls_cleanup_nodes(ca_id, ca_version, node_id, job_id, completed)
         VALUES('ca-main', 1, $1, 'job-term', true)",
    )
    .bind(&node)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO mtls_probe_versions(node_id, credential_id, credential_version, ca_id, ca_version, expires_at, next_action_at)
         VALUES($1, 'probe-cred', 1, 'ca-main', 1, $2, $2)",
    )
    .bind(&node)
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    // Activation chain: act-1 (probe-cred v1) <- act-2 <- act-3 (current node activation, probe-cred v2).
    for (id, job_id, prev, generation, ver) in [
        ("act-1", "job-act-1", None, 1_i64, 1_i64),
        ("act-2", "job-act-2", Some("act-1"), 2, 1),
        ("act-3", "job-act-3", Some("act-2"), 3, 2),
    ] {
        insert_job(
            pool,
            job_id,
            Some(&node),
            "deploy",
            "succeeded",
            "completed",
            None,
            Some(old),
            None,
        )
        .await;
        sqlx::query(
            "INSERT INTO mtls_activations(id, node_id, job_id, generation, target_revision, phase, previous_state, target_digest, snapshot_enc, trust, probe_credential_id, probe_credential_version, created_at, updated_at, previous_activation_id)
             VALUES($1, $2, $3, $4, 1, 'committed', 'ready', 'dig', $5, '[]', 'probe-cred', $6, $7, $7, $8)",
        )
        .bind(id)
        .bind(&node)
        .bind(job_id)
        .bind(generation)
        .bind(&enc)
        .bind(ver)
        .bind(old + TimeDelta::seconds(generation))
        .bind(prev)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query("UPDATE nodes SET mtls_activation_id = 'act-3' WHERE id = $1")
        .bind(&node)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO mtls_deployment_snapshots(node_id, revision, activation_id, snapshot_enc, created_at)
         VALUES($1, 2, 'act-2', $2, $3)",
    )
    .bind(&node)
    .bind(&enc)
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    // ConfigVersions: create 25 old revisions; keep deployed/desired/latest deployed_success/newest 20/snapshot/dirty.
    for rev in 2_i64..=26 {
        let id = format!("cv-{rev:02}");
        sqlx::query(
            "INSERT INTO config_versions(id, node_id, revision, config_enc, content_sha256, deployed_success, created_at)
             VALUES($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&id)
        .bind(&node)
        .bind(rev)
        .bind(&enc)
        .bind(format!("sha-{rev}"))
        .bind(rev == 3)
        .bind(old + TimeDelta::seconds(rev))
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query("UPDATE nodes SET desired_revision = 26, deployed_revision = 26 WHERE id = $1")
        .bind(&node)
        .execute(pool)
        .await
        .unwrap();

    // Execute metadata/credential retention tasks in dependency order:
    assert_eq!(
        run_all_pages(pool, Task::Rows(rows::DNS_OPERATIONS), now, None)
            .await
            .deleted,
        1
    );
    assert_eq!(
        run_all_pages(pool, Task::Rows(rows::DNS_RECORDS_DELETED), now, None)
            .await
            .deleted,
        1
    );
    assert_eq!(
        run_all_pages(pool, Task::Rows(rows::MTLS_NODE_OPERATIONS), now, None)
            .await
            .deleted,
        1
    );
    assert_eq!(
        run_all_pages(pool, Task::Rows(rows::MTLS_ROTATIONS), now, None)
            .await
            .deleted,
        1
    );
    assert_eq!(
        run_all_pages(pool, Task::CredentialBatches, now, None)
            .await
            .deleted,
        2
    );
    assert_eq!(
        run_all_pages(pool, Task::MtlsTrustCleanup, now, None)
            .await
            .deleted,
        1
    );

    // While config_version rev 2 still exists, MtlsDeploymentSnapshots protects its snapshot.
    assert_eq!(
        run_all_pages(pool, Task::MtlsDeploymentSnapshots, now, None)
            .await
            .deleted,
        0
    );

    // Reset deployed_success only on rev 3 and age rev 1 so latest_deployed_success + newest 20 stay protected.
    sqlx::query(
        "UPDATE config_versions
         SET deployed_success = (revision = 3),
             created_at = CASE WHEN revision = 1 THEN $2 ELSE created_at END
         WHERE node_id = $1",
    )
    .bind(&node)
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    let cv_dirty = run_all_pages(pool, Task::ConfigVersions, now, None).await;
    // Before reference_index drains credential_reference_dirty, only already-indexed rev 1 is pruned.
    assert_eq!(cv_dirty.deleted, 1);

    credentials::reference_index::initialize(&state.pool, &state.secrets)
        .await
        .unwrap();
    let cv = run_all_pages(pool, Task::ConfigVersions, now, None).await;
    // Remaining revisions for node: 2..=26; protected: rev 3 (latest_deployed_success) + 7..=26 (top 20) => 4 deleted (2,4,5,6).
    assert_eq!(cv.deleted, 4);

    // ConfigVersions deleted rev 2; now MtlsDeploymentSnapshots -> MtlsActivations -> credentials::retention::prune cascades cleanly!
    assert_eq!(
        run_all_pages(pool, Task::MtlsDeploymentSnapshots, now, None)
            .await
            .deleted,
        1
    );
    let act_deleted = run_all_pages(pool, Task::MtlsActivations, now, None).await;
    assert_eq!(act_deleted.deleted, 2);
    let remaining_acts: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, previous_activation_id FROM mtls_activations ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    assert_eq!(remaining_acts, vec![("act-3".to_owned(), None)]);

    // Removing old activations now unblocks credentials::retention::prune for probe-cred v1!
    sqlx::query(
        "UPDATE config_versions SET deployed_success = true WHERE node_id = $1 AND revision = 26",
    )
    .bind(&node)
    .execute(pool)
    .await
    .unwrap();
    assert!(credentials::retention::prune(&state).await.unwrap() >= 1);
    let probe_v1: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM credential_versions WHERE credential_id = 'probe-cred' AND version = 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(probe_v1, 0);
}

#[tokio::test]
async fn config_versions_retains_direct_rollback_target_and_prunes_older_revisions() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    let old = now - TimeDelta::days(95);
    let enc = state.secrets.encrypt("{}").unwrap();

    for rev in 2_i64..=26 {
        let id = format!("rb-cv-{rev:02}");
        sqlx::query(
            "INSERT INTO config_versions(id, node_id, revision, config_enc, content_sha256, deployed_success, created_at)
             VALUES($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&id)
        .bind(&node)
        .bind(rev)
        .bind(&enc)
        .bind(format!("rb-sha-{rev}"))
        .bind(matches!(rev, 2 | 3 | 26))
        .bind(old + TimeDelta::seconds(rev))
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query("UPDATE nodes SET desired_revision = 26, deployed_revision = 26 WHERE id = $1")
        .bind(&node)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE config_versions SET created_at = $2 WHERE node_id = $1 AND revision = 1")
        .bind(&node)
        .bind(old)
        .execute(pool)
        .await
        .unwrap();

    credentials::reference_index::initialize(&state.pool, &state.secrets)
        .await
        .unwrap();

    // Direct rollback target is rev 3 (< deployed 26). Revs 1, 2, 4, 5, 6 are pruned while 3 and 7..=26 stay.
    let cv = run_all_pages(pool, Task::ConfigVersions, now, None).await;
    assert_eq!(cv.deleted, 5);

    let rollback_target: Option<i64> = sqlx::query_scalar(
        "SELECT revision FROM config_versions
         WHERE node_id = $1 AND deployed_success = TRUE AND revision < $2
         ORDER BY revision DESC LIMIT 1 FOR KEY SHARE",
    )
    .bind(&node)
    .bind(26_i64)
    .fetch_optional(pool)
    .await
    .unwrap();
    assert_eq!(rollback_target, Some(3));
}

#[tokio::test]
async fn dns_records_deleted_retention_stays_bounded_with_large_bindings_and_operations() {
    let (state, node) = fixture().await;
    let pool = &state.pool;
    let now = Utc::now();
    let old = now - TimeDelta::days(95);
    let enc = state.secrets.encrypt("{}").unwrap();

    sqlx::query(
        "INSERT INTO credentials(id, name, kind, latest_version, created_at, updated_at)
         VALUES('dns-perf-cred', 'dns-perf', 'dns', 1, $1, $1)",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO credential_versions(credential_id, version, payload_enc, created_at)
         VALUES('dns-perf-cred', 1, $1, $2)",
    )
    .bind(&enc)
    .bind(old)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO dns_connections(id, name, provider, credential_id, credential_version)
         VALUES('dns-perf-conn', 'CF', 'cloudflare', 'dns-perf-cred', 1)",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO dns_zones(id, connection_id, provider_zone_id, name)
         VALUES('dns-perf-zone', 'dns-perf-conn', 'z-perf', 'perf.example.test')",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO nodes(
            id, name, ssh_host, ssh_port, ssh_username, ssh_host_fingerprint,
            public_host, public_port, listen_addr, tls_sni, tls_skip_verify,
            node_token_hash, node_token_enc, traffic_stats_secret_enc,
            desired_config_enc, desired_revision, deployed_config_enc, deployed_revision,
            deployed_content_sha256, state, last_seen_at, last_sample_at, proxy_probe_url,
            traffic_stats_port, created_at, updated_at, ssh_credential_id, ssh_credential_version,
            published_connection, mtls_state, mtls_error, mtls_generation, mtls_activation_id,
            mtls_recovery_configured, traffic_capture_mode
         )
         SELECT
            'dns-node-' || g, 'DNS Node ' || g, ssh_host, ssh_port, ssh_username, ssh_host_fingerprint,
            public_host, public_port, listen_addr, tls_sni, tls_skip_verify,
            'dns-tok-hash-' || g, node_token_enc, traffic_stats_secret_enc,
            desired_config_enc, desired_revision, deployed_config_enc, deployed_revision,
            deployed_content_sha256, state, last_seen_at, last_sample_at, proxy_probe_url,
            traffic_stats_port, created_at, updated_at, ssh_credential_id, ssh_credential_version,
            published_connection, mtls_state, mtls_error, mtls_generation, mtls_activation_id,
            mtls_recovery_configured, traffic_capture_mode
         FROM nodes CROSS JOIN generate_series(1, 10000) g
         WHERE id = $1",
    )
    .bind(&node)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO dns_bindings(node_id, zone_id, hostname, record_ids)
         SELECT 'dns-node-' || g, 'dns-perf-zone', 'host-' || g || '.perf.example.test',
                CASE WHEN g = 1 THEN jsonb_build_array('rec-bound')
                     ELSE jsonb_build_array('rec-unused-bind-' || g) END
         FROM generate_series(1, 10000) g",
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO dns_operations(
            id, idempotency_key, request_sha256, connection_id, credential_version,
            resource_type, resource_id, action, payload, applied_at, created_at
         )
         SELECT 'op-' || g, 'idem-' || g, repeat('a', 64), 'dns-perf-conn', 1,
                'record', CASE WHEN g = 1 THEN 'rec-op' ELSE 'rec-unused-op-' || g END,
                'delete', '{}'::jsonb, $1, $1
         FROM generate_series(1, 300000) g",
    )
    .bind(now - TimeDelta::days(10))
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO dns_records(id, zone_id, name, record_type, content, deleted_at, created_at, updated_at)
         VALUES('rec-bound', 'dns-perf-zone', 'b', 'A', '1.1.1.1', $1, $1, $1),
               ('rec-op', 'dns-perf-zone', 'o', 'A', '1.1.1.2', $2, $2, $2)",
    )
    .bind(old - TimeDelta::minutes(2))
    .bind(old - TimeDelta::minutes(1))
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO dns_records(id, zone_id, name, record_type, content, deleted_at, created_at, updated_at)
         SELECT 'rec-del-' || lpad(g::text, 3, '0'), 'dns-perf-zone', 'd' || g, 'A', '1.1.1.3',
                $1 + make_interval(secs => g::double precision * 0.001), $1, $1
         FROM generate_series(1, 512) g",
    )
    .bind(old)
    .execute(pool)
    .await
    .unwrap();

    for table in ["dns_bindings", "dns_operations", "dns_records"] {
        sqlx::query(sqlx::AssertSqlSafe(format!("ANALYZE {table}")))
            .execute(pool)
            .await
            .unwrap();
    }

    let mut cursor = Cursor::default();
    let mut deleted = 0_u64;
    let mut pages = 0_usize;
    loop {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL statement_timeout = '4000'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let counts = Task::Rows(rows::DNS_RECORDS_DELETED)
            .page(&mut tx, &mut cursor, now, None, None)
            .await
            .unwrap();
        let elapsed = started.elapsed();
        tx.commit().await.unwrap();
        eprintln!(
            "dns_records page {}: scanned={} deleted={} elapsed_ms={}",
            pages + 1,
            counts.scanned,
            counts.deleted,
            elapsed.as_millis()
        );
        assert!(elapsed.as_secs_f64() < 4.0);
        deleted += counts.deleted;
        pages += 1;
        if cursor.done {
            break;
        }
        assert!(pages < 8);
    }

    assert_eq!(deleted, 512);
    let remaining: Vec<String> = sqlx::query_scalar("SELECT id FROM dns_records ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap();
    assert_eq!(remaining, vec!["rec-bound".to_owned(), "rec-op".to_owned()]);
}
