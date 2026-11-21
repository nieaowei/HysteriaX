//! Disposable scale comparison with the production worker and real local HTTP,
//! SSH, and QUIC paths for six nodes. Synthetic cycles supply full-scale churn.
use std::sync::atomic::{AtomicU64, Ordering};

use super::{distribution, fixture::Remote, *};
use crate::background::retention::run as run_retention;
use crate::db::test_relay::SqlRelay;

const ADMIN: &str = "local-sampling-retention-benchmark-token-not-for-deployment";
const KINDS: [&str; 3] = ["traffic", "network", "proxy"];

async fn seed_history(pool: &PgPool, cycles_per_kind: i64, age_hours: i64) -> u64 {
    // The configured production interval creates 3 x 10,000 rows per minute.
    sqlx::query_scalar::<_, i64>(
        "WITH cycles AS (
             INSERT INTO sampling_cycles(id,kind,period_start,deadline_at,expected_nodes)
             SELECT gen_random_uuid(),kind,t-interval '1 minute',t,10000
             FROM unnest(ARRAY['traffic','network','proxy']) kind
             CROSS JOIN generate_series(1,$1::bigint) slot
             CROSS JOIN LATERAL (
                 SELECT statement_timestamp()-make_interval(hours=>$2,mins=>slot::int) t
             ) stamp
             RETURNING *
         ), work AS (
             INSERT INTO background_work (id,kind,cycle_id,node_id,state,payload,generation,available_at,deadline_at,started_at,finished_at)
             SELECT c.id::text||':'||n.id,c.kind,c.id,n.id,'complete',
                    jsonb_build_object('captured_at',c.period_start,'deployed_revision',1),
                    1,c.period_start,c.deadline_at,c.period_start,c.deadline_at
             FROM cycles c CROSS JOIN nodes n
             RETURNING id,kind,cycle_id,node_id,started_at,finished_at
         ), leases AS (
             INSERT INTO resource_leases (resource_key,generation) SELECT 'work:'||id,1 FROM work
         ), batches AS (
             INSERT INTO traffic_sample_batches (sample_id,node_id,cycle_id,state,payload_digest,sampled_at,completed_at)
             SELECT id,node_id,cycle_id,'complete','fixture',started_at,finished_at
             FROM work WHERE kind='traffic'
         ) SELECT count(*) FROM work",
    )
    .bind(cycles_per_kind)
    .bind(age_hours as i32)
    .fetch_one(pool)
    .await
    .unwrap() as u64
}

async fn old_work_count(pool: &PgPool) -> u64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM background_work WHERE cycle_id IS NOT NULL
         AND state='complete' AND finished_at<clock_timestamp()-interval '14 days'",
    )
    .fetch_one(pool)
    .await
    .unwrap() as u64
}

#[derive(Default)]
struct Sampling {
    claim_ms: Vec<f64>,
    sample_ms: Vec<f64>,
    passed: usize,
    failed: usize,
    drained: usize,
}

async fn sampling_probe(
    state: AppState,
    kind: &'static str,
    admit_until: Option<Instant>,
) -> Sampling {
    let mut records = Sampling::default();
    let mut shutdown = state.shutdown.subscribe();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let admission_closed = || admit_until.is_some_and(|until| Instant::now() >= until);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.wait_for(|stop| *stop) => return records,
            _ = tick.tick() => {}
        }
        if admission_closed() {
            return records;
        }
        ensure_cycle(&state.pool, kind).await.unwrap();
        for _ in 0..6 {
            if *shutdown.borrow() || admission_closed() {
                return records;
            }
            let started = Instant::now();
            let Some(work) = claim(&state.pool, kind, &state.instance_id).await.unwrap() else {
                break;
            };
            let claim_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = Instant::now();
            let completed = execution::supervise(state.pool.clone(), work.leases.clone(), async {
                let result = match kind {
                    "traffic" => crate::traffic::sample_work(&state, &work).await,
                    "network" => crate::node_limits::sample_work(&state, &work).await,
                    "proxy" => crate::proxy_monitor::sample_work(&state, &work).await,
                    _ => unreachable!(),
                };
                if let Err(error) = &result {
                    if error.is::<Draining>() {
                        finish_or_defer(&state.pool, &work, None).await.unwrap();
                        return None;
                    }
                    tracing::warn!(kind, %error, "fixture collection failed");
                }
                finish(&state.pool, &work, result.is_ok()).await.unwrap();
                Some(result.is_ok())
            })
            .await
            .into_result()
            .unwrap();
            if completed == Some(None) {
                records.drained += 1;
                return records;
            }
            records.claim_ms.push(claim_ms);
            records
                .sample_ms
                .push(started.elapsed().as_secs_f64() * 1000.0);
            if completed == Some(Some(true)) {
                records.passed += 1;
            } else {
                records.failed += 1;
            }
        }
    }
}

async fn measure(direct: &PgPool, state: AppState, retention_enabled: bool, seconds: u64) -> Value {
    let initial = old_work_count(direct).await;
    let produced = Arc::new(AtomicU64::new(0));
    let mut shutdown = state.shutdown.subscribe();
    let producer = {
        let pool = direct.clone();
        let count = produced.clone();
        let mut shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            tick.tick().await;
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.wait_for(|stop| *stop) => return,
                    _ = tick.tick() => {}
                }
                count.fetch_add(seed_history(&pool, 1, 337).await, Ordering::Relaxed);
            }
        })
    };
    let receiver = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = receiver.local_addr().unwrap();
    let app = crate::api::router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(receiver, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown.wait_for(|stop| *stop).await;
            })
            .await
            .unwrap();
    });
    let heartbeat = tokio::spawn(execution::run_heartbeats(state.clone()));
    let analytics = tokio::spawn(crate::analytics::run(state.clone()));
    let expiry = tokio::spawn(run_expiry(state.clone()));
    let worker = retention_enabled.then(|| tokio::spawn(run_retention(state.clone())));
    let phase = if retention_enabled { "Delete" } else { "Off" };
    let sampling = KINDS.map(|kind| tokio::spawn(sampling_probe(state.clone(), kind, None)));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let start = Instant::now();
    let mut next_report = Duration::from_secs(60);
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
        if start.elapsed() >= next_report {
            let remaining = old_work_count(direct).await;
            println!(
                "RETENTION_PROGRESS {phase} {} {remaining} {}",
                start.elapsed().as_secs(),
                produced.load(Ordering::Relaxed)
            );
            next_report += Duration::from_secs(60);
        }
    }
    state.shutdown.send_replace(true);
    producer.await.unwrap();
    if let Some(worker) = worker {
        worker.await.unwrap();
    }
    let mut samples = serde_json::Map::new();
    for (kind, task) in KINDS.into_iter().zip(sampling) {
        let records = task.await.unwrap();
        samples.insert(
            kind.into(),
            json!({
                "success":records.passed,"failures":records.failed,
                "claim":distribution(records.claim_ms)
            }),
        );
    }
    // main also aborts the analytics task once HTTP has shut down.
    analytics.abort();
    heartbeat.abort();
    let _ = tokio::join!(analytics, heartbeat);
    for background in [server, expiry] {
        tokio::time::timeout(Duration::from_secs(30), background)
            .await
            .unwrap()
            .unwrap();
    }
    let remaining = old_work_count(direct).await;
    let produced = produced.load(Ordering::Relaxed);
    let elapsed_seconds = start.elapsed().as_secs_f64();
    json!({
        "seconds":elapsed_seconds, "work_before":initial, "work_produced":produced,
        "work_remaining":remaining, "deleted_per_second":
            (initial + produced - remaining) as f64 / elapsed_seconds,
        "http":distribution(requests),"samples":samples
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "30-minute retention scale comparison; requires local test DB and Hysteria fixture"]
async fn sampling_retention_capacity() {
    let database = url::Url::parse(&std::env::var("TEST_DATABASE_URL").unwrap()).unwrap();
    assert!(matches!(
        database.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    ));
    assert_eq!(crate::runtime_config::sampling_interval_seconds(), 60);
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
    let mut seeded = 0;
    for partition in 0..12 {
        seeded += seed_history(&direct, 10, 385 + partition).await;
    }
    assert_eq!(seeded, 3_600_000);
    println!("RETENTION_SEEDED {seeded}");
    let mut prepare = direct.begin().await.unwrap();
    sqlx::raw_sql(
        "SET LOCAL lock_timeout = '60s';
         SET LOCAL statement_timeout = '5min';
         ANALYZE background_work;
         ANALYZE resource_leases;
         ANALYZE traffic_sample_batches;
         ANALYZE sampling_cycles;",
    )
    .execute(&mut *prepare)
    .await
    .unwrap();
    prepare.commit().await.unwrap();
    let relay = SqlRelay::new(&direct, Duration::from_millis(25), 100).await;
    let off = measure(
        &direct,
        AppState::new(relay.pool.clone(), remote.secrets.clone()),
        false,
        300,
    )
    .await;
    println!("RETENTION_BASELINE {off}");
    let delete = measure(
        &direct,
        AppState::new(relay.pool.clone(), remote.secrets.clone()),
        true,
        1800,
    )
    .await;
    println!(
        "RETENTION_CAPACITY {}",
        json!({"users":100000,"nodes":10000,
        "real_sampling_nodes":6, "db_response_delay_ms":25, "off":off,"delete":delete})
    );
    remote.stop().await;
    relay.pool.close().await;
    direct.close().await;
    assert!(delete["deleted_per_second"].as_f64().unwrap() > 500.0);
    assert!(delete["work_remaining"].as_u64().unwrap() < delete["work_before"].as_u64().unwrap());
    let rate = |metrics: &Value| {
        let success = metrics["success"].as_f64().unwrap();
        success / (success + metrics["failures"].as_f64().unwrap())
    };
    let p95 = |metrics: &Value| metrics["p95_ms"].as_f64().unwrap();
    assert!(p95(&delete["http"]) <= 1.10 * p95(&off["http"]));
    for kind in KINDS {
        let (baseline, actual) = (&off["samples"][kind], &delete["samples"][kind]);
        assert!(
            rate(actual) >= rate(baseline),
            "{kind} sampling success rate"
        );
        assert!(
            p95(&actual["claim"]) <= 1.10 * p95(&baseline["claim"]),
            "{kind} claim P95"
        );
    }
}

#[path = "history_retention_benchmark.rs"]
mod history;
