//! Sustained history retention with full-size source cohorts and the same local
//! HTTP, SSH and QUIC fixture as the sampling work retention comparison.
use super::{distribution, sampling_probe, *};
use crate::background::history_retention::run as run_history_retention;

const TABLES: [&str; 4] = [
    "traffic_sample_items",
    "traffic_records",
    "node_network_samples",
    "traffic_sample_batches",
];
const COHORT: [u64; 4] = [100_000, 100_000, 10_000, 10_000];
const CHUNKS: u64 = 50;
const NODES_PER_CHUNK: u64 = 10_000 / CHUNKS;

async fn seed(pool: &PgPool, sequence: i64, historical: bool, first_node: i32, last_node: i32) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL statement_timeout='180s'; SET LOCAL lock_timeout='5s'")
        .execute(&mut *tx)
        .await
        .unwrap();
    // Use the same parent-first order as live billing and network collection.
    sqlx::query(
        "SELECT id FROM nodes
         WHERE id BETWEEN 'n-'||lpad($1::int::text,8,'0') AND 'n-'||lpad($2::int::text,8,'0')
         ORDER BY id FOR NO KEY UPDATE",
    )
    .bind(first_node)
    .bind(last_node)
    .execute(&mut *tx)
    .await
    .unwrap();
    let at: DateTime<Utc> = sqlx::query_scalar(
        "SELECT clock_timestamp() + CASE WHEN $1
             THEN -interval '68 days' + make_interval(mins=>$2::int)
             ELSE -interval '1 minute' END",
    )
    .bind(historical)
    .bind(sequence as i32)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO traffic_sample_batches
         (sample_id,node_id,instance_id,state,payload_digest,sampled_at,completed_at,item_count,processed_count)
         SELECT 'capacity:'||$1::text||':'||id,id,'fixture','complete','fixture',$2,$2,
                CASE WHEN $3 THEN 10 ELSE 0 END,CASE WHEN $3 THEN 10 ELSE 0 END
         FROM nodes WHERE id BETWEEN 'n-'||lpad($4::int::text,8,'0') AND 'n-'||lpad($5::int::text,8,'0')"
    ).bind(sequence.to_string()).bind(at).bind(historical).bind(first_node).bind(last_node)
        .execute(&mut *tx).await.unwrap();
    if historical {
        sqlx::query(
            "WITH billed AS MATERIALIZED (
              SELECT 'u-'||lpad((i+10000*member)::text,8,'0') AS user_id,
                     'n-'||lpad(i::text,8,'0') AS node_id,
                     'capacity:'||$1::text||':n-'||lpad(i::text,8,'0') AS sample_id
              FROM generate_series($3::int,$4::int) i CROSS JOIN generate_series(0,9) member
             ), items AS (
              INSERT INTO traffic_sample_items(sample_id,node_id,user_id,tx_total,rx_total,sampled_at,processed_at)
              SELECT sample_id,node_id,user_id,2,3,$2,$2 FROM billed
             )
             INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at,sample_id)
             SELECT gen_random_uuid()::text,node_id,user_id,'fixture',2,3,2,3,$2,sample_id FROM billed
             ORDER BY node_id,user_id"
        ).bind(sequence.to_string()).bind(at).bind(first_node).bind(last_node)
            .execute(&mut *tx).await.unwrap();
        sqlx::query(
            "INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,sampled_at)
             SELECT node_id,period_id,2,3,$1 FROM node_packages
             WHERE node_id BETWEEN 'n-'||lpad($2::int::text,8,'0') AND 'n-'||lpad($3::int::text,8,'0')
             ORDER BY node_id",
        )
        .bind(at).bind(first_node).bind(last_node)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

async fn remaining(pool: &PgPool) -> [u64; 4] {
    let mut counts = [0; 4];
    for ((out, table), column) in counts.iter_mut().zip(TABLES).zip([
        "processed_at",
        "sampled_at",
        "sampled_at",
        "completed_at",
    ]) {
        let query = sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM {table} WHERE {column}<clock_timestamp()-interval '64 days'"
        ));
        *out = sqlx::query_scalar::<_, i64>(query)
            .fetch_one(pool)
            .await
            .unwrap() as u64;
    }
    counts
}

async fn phase(
    direct: &PgPool,
    state: AppState,
    history_enabled: bool,
    seconds: u64,
    first: i64,
) -> Value {
    let before = remaining(direct).await;
    let sampling_before = old_work_count(direct).await;
    let start_lsn: String = sqlx::query_scalar("SELECT pg_current_wal_insert_lsn()::text")
        .fetch_one(direct)
        .await
        .unwrap();
    let receiver = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = receiver.local_addr().unwrap();
    if seconds >= 300 {
        // Start at the beginning of a full sampling slot in both measured phases.
        let slot = Utc::now().timestamp_millis().rem_euclid(60_000) as u64;
        tokio::time::sleep(Duration::from_millis(60_000 - slot)).await;
    }
    let start = Instant::now();
    // The final HTTP request may run past the window. Stop admitting samples
    // just before its closing slot boundary and let claimed work finish.
    let admit_until = (seconds >= 300).then(|| start + Duration::from_secs(seconds - 1));
    let mut shutdown = state.shutdown.subscribe();
    let app = crate::api::router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(receiver, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown.wait_for(|stop| *stop).await;
            })
            .await
            .unwrap();
    });
    let worker = history_enabled.then(|| tokio::spawn(run_history_retention(state.clone())));
    let sampling_retention = tokio::spawn(run_retention(state.clone()));
    let expiry = tokio::spawn(run_expiry(state.clone()));
    let sampling_producer = {
        let pool = direct.clone();
        let mut shutdown = state.shutdown.subscribe();
        tokio::spawn(async move {
            let mut total = 0;
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            tick.tick().await;
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.wait_for(|stop| *stop) => return total,
                    _ = tick.tick() => {}
                }
                total += seed_history(&pool, 1, 337).await;
            }
        })
    };
    let heartbeat = tokio::spawn(execution::run_heartbeats(state.clone()));
    let analytics = tokio::spawn(crate::analytics::run(state.clone()));
    let sampling = KINDS.map(|kind| tokio::spawn(sampling_probe(state.clone(), kind, admit_until)));
    let mut shutdown = state.shutdown.subscribe();
    let direct_writer = direct.clone();
    let producer = tokio::spawn(async move {
        let mut count = 0_u64;
        let mut interval = tokio::time::interval(Duration::from_millis(60_000 / CHUNKS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = shutdown.wait_for(|stop| *stop) => return count,
                _ = interval.tick() => {}
            }
            // Spread each 60-second cohort across small transactions. A single
            // 100k-row writer would pin every real sampling node for the cohort.
            let minute = first + (count / CHUNKS) as i64;
            let start = (count % CHUNKS * NODES_PER_CHUNK) as i32 + 1;
            let end = start + NODES_PER_CHUNK as i32 - 1;
            seed(&direct_writer, minute, true, start, end).await;
            count += 1;
        }
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let mut requests = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while start.elapsed() < Duration::from_secs(seconds) {
        tick.tick().await;
        let requested = Instant::now();
        let response = client
            .get(format!("http://{address}/api/v2/users?page_size=25"))
            .bearer_auth(ADMIN)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        response.bytes().await.unwrap();
        requests.push(requested.elapsed().as_secs_f64() * 1000.0);
    }
    state.shutdown.send_replace(true);
    let chunks = producer.await.unwrap();
    if let Some(worker) = worker {
        worker.await.unwrap();
    }
    sampling_retention.await.unwrap();
    expiry.await.unwrap();
    let sampling_produced = sampling_producer.await.unwrap();
    let mut samples = serde_json::Map::new();
    for (kind, task) in KINDS.into_iter().zip(sampling) {
        let records = task.await.unwrap();
        samples.insert(
            kind.into(),
            json!({
                "success": records.passed, "failures": records.failed, "drained": records.drained,
                "claim": distribution(records.claim_ms),
                "collection": distribution(records.sample_ms)
            }),
        );
    }
    analytics.abort();
    heartbeat.abort();
    let _ = tokio::join!(analytics, heartbeat);
    server.await.unwrap();
    let elapsed = start.elapsed().as_secs_f64();
    let after = remaining(direct).await;
    let sampling_after = old_work_count(direct).await;
    let throughput: Vec<_> = (0..TABLES.len())
        .map(|i| (before[i] + (COHORT[i] / CHUNKS) * chunks - after[i]) as f64 / elapsed)
        .collect();
    let wal_bytes: i64 = sqlx::query_scalar(
        "SELECT pg_wal_lsn_diff(pg_current_wal_insert_lsn(),$1::text::pg_lsn)::bigint",
    )
    .bind(start_lsn)
    .fetch_one(direct)
    .await
    .unwrap();
    let table_bytes: Vec<i64> = sqlx::query_scalar(
        "SELECT pg_total_relation_size(name::regclass) FROM unnest($1::text[]) AS name",
    )
    .bind(&TABLES[..])
    .fetch_all(direct)
    .await
    .unwrap();
    maintenance::poll_summaries();
    json!({
        "seconds": elapsed, "chunks": chunks, "cohorts": chunks as f64 / CHUNKS as f64, "tables": TABLES, "before": before,
        "after": after, "deleted_per_second": throughput, "http": distribution(requests),
        "table_bytes": table_bytes, "instance_wal_bytes": wal_bytes, "samples": samples,
        "sampling_retention": {
            "mode": "delete", "before": sampling_before, "produced": sampling_produced,
            "after": sampling_after,
            "deleted_per_second": (sampling_before + sampling_produced - sampling_after) as f64 / elapsed,
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "full-size disposable PostgreSQL/SSH/QUIC capacity comparison"]
async fn history_retention_capacity() {
    use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::filter_fn(|metadata| {
                    metadata.target().ends_with("maintenance::observations")
                        || (metadata
                            .target()
                            .starts_with("hysteriax_server::background")
                            && *metadata.level() == tracing::Level::WARN)
                })),
        )
        .try_init()
        .unwrap();
    let database = url::Url::parse(&std::env::var("TEST_DATABASE_URL").unwrap()).unwrap();
    assert!(matches!(
        database.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    ));
    assert_eq!(crate::runtime_config::sampling_interval_seconds(), 60);
    let smoke = std::env::var("HX_HISTORY_RETENTION_SMOKE").as_deref() == Ok("1");
    let direct = db::test_pool_with_max_connections(8).await;
    let remote = Remote::start(direct.clone(), 6).await;
    remote.seed(&direct, 6).await;
    sqlx::query(
        "INSERT INTO admin_tokens(id,token_hash,label,created_at)
                 VALUES('retention-bench',$1,'Local benchmark',clock_timestamp())",
    )
    .bind(crate::security::token_digest(ADMIN))
    .execute(&direct)
    .await
    .unwrap();
    for cohort in 1..=if smoke { 3 } else { 12 } {
        seed(&direct, cohort, true, 1, 10_000).await;
    }
    // Both phases run 0048 with enough past-due work and leases to remain busy.
    for partition in 0..if smoke { 2 } else { 10 } {
        seed_history(&direct, if smoke { 5 } else { 10 }, 385 + partition).await;
    }
    // Preserve a recent restart checkpoint for every synthetic node.
    seed(&direct, 0, false, 1, 10_000).await;
    // Seed writes can trigger autovacuum before benchmarking starts. Let it
    // release its table locks without changing timeouts in the measured phases.
    let mut analyze = direct.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL lock_timeout='120s'; SET LOCAL statement_timeout='180s'")
        .execute(&mut *analyze)
        .await
        .unwrap();
    for table in ["background_work", "resource_leases", "sampling_cycles"]
        .into_iter()
        .chain(TABLES)
    {
        sqlx::query(sqlx::AssertSqlSafe(format!("ANALYZE {table}")))
            .execute(&mut *analyze)
            .await
            .unwrap();
    }
    analyze.commit().await.unwrap();
    let relay = SqlRelay::new(&direct, Duration::from_millis(25), 100).await;
    let off = phase(
        &direct,
        AppState::new(relay.pool.clone(), remote.secrets.clone()),
        false,
        if smoke { 65 } else { 300 },
        1000,
    )
    .await;
    println!("HISTORY_RETENTION_BASELINE {off}");
    let delete = phase(
        &direct,
        AppState::new(relay.pool.clone(), remote.secrets.clone()),
        true,
        if smoke { 65 } else { 1800 },
        2000,
    )
    .await;
    println!(
        "HISTORY_RETENTION_CAPACITY {}",
        json!({
            "smoke": smoke, "users": 100000, "nodes": 10000, "real_sampling_nodes": 6,
            "delay_ms": 25, "off": off, "delete": delete
        })
    );
    remote.stop().await;
    relay.pool.close().await;
    if smoke {
        assert!(delete["deleted_per_second"][0].as_f64().unwrap() > 0.0);
        return;
    }
    assert!(
        delete["chunks"].as_u64().unwrap() >= 29 * CHUNKS,
        "producer lagged"
    );
    let old = &delete["sampling_retention"];
    assert!(
        old["produced"].as_u64().unwrap() >= 29 * 30_000,
        "0048 producer lagged"
    );
    assert!(
        old["deleted_per_second"].as_f64().unwrap() >= 500.0,
        "0048 throughput: {old}"
    );
    assert!(
        old["after"].as_u64().unwrap() <= old["before"].as_u64().unwrap(),
        "0048 backlog grew: {old}"
    );
    for (index, table) in TABLES.iter().enumerate() {
        let actual = delete["deleted_per_second"][index].as_f64().unwrap();
        assert!(
            actual >= COHORT[index] as f64 * 1.25 / 60.0,
            "{table}: {actual} rows/s"
        );
    }
    let p95 = |metrics: &Value| metrics["p95_ms"].as_f64().unwrap();
    assert!(p95(&delete["http"]) <= 1.10 * p95(&off["http"]));
    for kind in KINDS {
        let (baseline, actual) = (&off["samples"][kind], &delete["samples"][kind]);
        assert!(
            baseline["success"].as_u64().unwrap() >= 24,
            "{kind} baseline sampled too few slots"
        );
        assert!(
            actual["success"].as_u64().unwrap() >= 174,
            "{kind} missed sampling slots"
        );
        assert_eq!(actual["failures"], 0, "{kind} sampling failures");
        assert_eq!(actual["drained"], 0, "{kind} interrupted sampling");
        assert!(
            p95(&actual["claim"]) <= 1.10 * p95(&baseline["claim"]),
            "{kind} claim p95"
        );
        assert!(
            p95(&actual["collection"]) <= 1.10 * p95(&baseline["collection"]),
            "{kind} collection p95"
        );
    }
}
