//! Explicit local database benchmark; shared with the frozen baseline runner.
use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tracing::{
    Subscriber,
    field::{Field, Visit},
    instrument::WithSubscriber,
};
use tracing_subscriber::{Layer, layer::SubscriberExt};

#[derive(Default)]
struct Counts {
    queries: AtomicU64,
    retries: AtomicU64,
}

struct QueryCounts(Arc<Counts>);
struct Outcome<'a>(&'a Counts);
impl Visit for Outcome<'_> {
    fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "outcome" && value == "retry" {
            self.0.retries.fetch_add(1, Ordering::Relaxed);
        }
    }
}
impl<S: Subscriber> Layer<S> for QueryCounts {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        if event.metadata().target() == "sqlx::query" {
            self.0.queries.fetch_add(1, Ordering::Relaxed);
        }
        event.record(&mut Outcome(&self.0));
    }
}

#[tokio::test]
#[ignore = "explicit baseline/candidate database performance comparison"]
async fn maintenance_transaction_comparison() {
    let iterations: usize = std::env::var("HX_MAINTENANCE_BENCH_ITERATIONS")
        .unwrap_or_else(|_| "40".into())
        .parse()
        .unwrap();
    assert!((10..=200).contains(&iterations));
    for busy in [false, true] {
        let state = fixture(if busy { PAGE * iterations as i64 } else { 0 }).await;
        if !busy {
            assert_eq!(page(&state).await, 0);
        }
        let mut warmed = Vec::new();
        for _ in 0..8 {
            warmed.push(state.pool.acquire().await.unwrap());
        }
        for mut connection in warmed {
            connection.return_to_pool().await;
        }
        let counts = Arc::new(Counts::default());
        let mut durations = Vec::new();
        let mut removed = 0;
        for _ in 0..iterations {
            let work = claim(&state, "benchmark").await;
            let subscriber = tracing_subscriber::registry().with(QueryCounts(counts.clone()));
            let started = std::time::Instant::now();
            // Count the transaction's SQL independently of the supervisor's
            // periodic renewal; wall time still includes the real supervisor.
            let operation = prune_work(&state.pool, &work).with_subscriber(subscriber);
            let result =
                crate::execution::supervise(state.pool.clone(), work.leases.clone(), operation)
                    .await
                    .into_result()
                    .unwrap()
                    .unwrap()
                    .unwrap();
            durations.push(started.elapsed().as_micros() as u64);
            assert_eq!(result, if busy { PAGE as u64 } else { 0 });
            removed += result;
        }
        durations.sort_unstable();
        println!(
            "MAINTENANCE_BENCHMARK {}",
            json!({
                "scenario":if busy {"nonempty"}else{"empty"},
                "iterations":iterations,"removed":removed,
                "queries":counts.queries.load(Ordering::Relaxed),
                "retries":counts.retries.load(Ordering::Relaxed),
                "mean_us":durations.iter().sum::<u64>() as f64/iterations as f64,
                "p95_us":durations[(iterations*95).div_ceil(100)-1],
                "max_us":durations[iterations-1],
            })
        );
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM credential_versions WHERE credential_id='leaf'"
            )
            .await,
            1
        );
        state.pool.close().await;
    }
    // A newly eligible version after an idle sweep pays the probe-to-delete
    // handoff cost. Keep this separate from continuous nonempty paging.
    let state = fixture(0).await;
    assert_eq!(page(&state).await, 0);
    let mut warmed = Vec::new();
    for _ in 0..8 {
        warmed.push(state.pool.acquire().await.unwrap());
    }
    for mut connection in warmed {
        connection.return_to_pool().await;
    }
    let counts = Arc::new(Counts::default());
    let mut durations = Vec::new();
    for index in 0..iterations {
        add_leaf(&state.pool, &format!("wake:{index}"), 1).await;
        let work = claim(&state, "benchmark").await;
        let subscriber = tracing_subscriber::registry().with(QueryCounts(counts.clone()));
        let started = std::time::Instant::now();
        let operation = prune_work(&state.pool, &work).with_subscriber(subscriber);
        let removed =
            crate::execution::supervise(state.pool.clone(), work.leases.clone(), operation)
                .await
                .into_result()
                .unwrap()
                .unwrap()
                .unwrap();
        durations.push(started.elapsed().as_micros() as u64);
        assert_eq!(removed, 1);
        // Reset both versions with a real empty sweep outside the measurement.
        assert_eq!(page(&state).await, 0);
    }
    durations.sort_unstable();
    println!(
        "MAINTENANCE_BENCHMARK {}",
        json!({
            "scenario":"wake_from_idle","iterations":iterations,"removed":iterations,
            "queries":counts.queries.load(Ordering::Relaxed),
            "retries":counts.retries.load(Ordering::Relaxed),
            "mean_us":durations.iter().sum::<u64>() as f64/iterations as f64,
            "p95_us":durations[(iterations*95).div_ceil(100)-1],
            "max_us":durations[iterations-1],
        })
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE credential_id LIKE 'wake:%'"
        )
        .await,
        iterations as i64
    );
    state.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "explicit background-loop contention comparison on an isolated database"]
async fn background_contention_comparison() {
    use axum::{Json, extract::State};
    use std::time::Duration;
    use tracing_subscriber::util::SubscriberInitExt;
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::INFO),
        )
        .with(crate::db::diagnostics::QueryLayer.with_filter(
            tracing_subscriber::filter::filter_fn(|metadata| metadata.target() == "sqlx::query"),
        ))
        .try_init()
        .unwrap();
    let seconds: u64 = std::env::var("HX_MAINTENANCE_BENCH_SECONDS")
        .unwrap_or_else(|_| "90".into())
        .parse()
        .unwrap();
    assert!((90..=600).contains(&seconds));
    let warmup: u64 = std::env::var("HX_MAINTENANCE_BENCH_WARMUP")
        .unwrap_or_else(|_| "0".into())
        .parse()
        .unwrap();
    assert!(warmup < seconds);
    let pool_max: u32 = std::env::var("HX_MAINTENANCE_BENCH_POOL")
        .unwrap_or_else(|_| "32".into())
        .parse()
        .unwrap();
    assert!((4..=100).contains(&pool_max));
    let reuse = std::env::var("HX_MAINTENANCE_BENCH_SCHEMA").ok();
    let direct = if let Some(schema) = &reuse {
        assert!(schema.starts_with("hysteriax_test_"));
        assert!(
            schema
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        );
        let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
        let url = url::Url::parse(&database_url).unwrap();
        assert!(matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]")
        ));
        let schema = schema.clone();
        db::pool_options()
            .max_connections(pool_max)
            .after_connect(move |connection, _| {
                let schema = schema.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}")))
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("SET lock_timeout='1s'")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("SET hysteriax.reference_maintenance='on'")
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&database_url)
            .await
            .unwrap()
    } else {
        db::test_pool_with_max_connections(pool_max).await
    };
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&direct)
        .await
        .unwrap();
    if reuse.is_none() {
        let secrets =
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        crate::credentials::reference_index::initialize(&direct, &secrets)
            .await
            .unwrap();
        let original = AppState::new(direct.clone(), secrets);
        for index in 0..6 {
            let input = crate::credentials::test_node_request(
                &original,
                json!({
                    "name":format!("Benchmark {index}"),"ssh_host":"127.0.0.1","ssh_port":1,
                    "ssh_username":"fixture","ssh_auth_type":"password","ssh_secret":"fixture",
                    "public_host":"example.test","public_port":443,"listen_addr":":443",
                }),
            )
            .await;
            let (_, Json(receipt)) =
                crate::api::nodes::create(State(original.clone()), Json(input))
                    .await
                    .unwrap();
            sqlx::query(
            "UPDATE nodes SET deployed_revision=1,state='deployed',mtls_state='ready' WHERE id=$1",
        )
        .bind(receipt["node"]["id"].as_str().unwrap())
        .execute(&direct)
        .await
        .unwrap();
        }
        sqlx::query(
            "UPDATE jobs SET status='cancelled',stage='benchmark_fixture' WHERE status='queued'",
        )
        .execute(&direct)
        .await
        .unwrap();
        sqlx::raw_sql("INSERT INTO background_work(id,kind,state,finished_at) SELECT 'historical:'||lpad(i::text,9,'0'),'benchmark-history','complete',clock_timestamp()-interval '1 day' FROM generate_series(1,260000)i; INSERT INTO resource_leases(resource_key) SELECT 'work:historical:'||lpad(i::text,9,'0') FROM generate_series(1,240000)i; ANALYZE background_work; ANALYZE resource_leases; ANALYZE nodes; ANALYZE jobs;")
        .execute(&direct).await.unwrap();
    }
    if std::env::var("HX_MAINTENANCE_BENCH_PREPARE_ONLY").as_deref() == Ok("1") {
        println!("BACKGROUND_FIXTURE {}", json!({"schema":schema}));
        direct.close().await;
        return;
    }
    let relay = db::test_relay::SqlRelay::new(&direct, Duration::from_millis(25), pool_max).await;
    let state = AppState::new(
        relay.pool.clone(),
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(db::diagnostics::run_summaries(state.shutdown.subscribe()));
    tasks.spawn(crate::events::run_publisher(state.clone()));
    tasks.spawn(crate::quota_rank::run(state.clone()));
    tasks.spawn(crate::execution::run_heartbeats(state.clone()));
    tasks.spawn(crate::background::run_expiry(state.clone()));
    tasks.spawn(crate::credentials::maintenance::run(state.clone()));
    tasks.spawn(crate::analytics::run(state.clone()));
    tasks.spawn(crate::jobs::run(state.clone()));
    tasks.spawn(crate::traffic::run(state.clone()));
    tasks.spawn(crate::node_limits::collect(state.clone()));
    tasks.spawn(crate::proxy_monitor::run(state.clone()));
    tasks.spawn(crate::node_limits::run(state.clone()));
    if warmup > 0 {
        tokio::time::sleep(Duration::from_secs(warmup)).await;
        println!("BACKGROUND_WINDOW_START");
        db::diagnostics::summaries();
        crate::background::maintenance::poll_summaries();
        println!("BACKGROUND_WINDOW_OPEN");
    }
    tokio::time::sleep(Duration::from_secs(seconds - warmup)).await;
    // Take the terminal measurement before aborting fixture tasks, so test
    // shutdown cancellations do not masquerade as business failures.
    println!("BACKGROUND_WINDOW_END");
    db::diagnostics::summaries();
    crate::background::maintenance::poll_summaries();
    println!("BACKGROUND_WINDOW_CLOSED");
    state.shutdown.send_replace(true);
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    db::diagnostics::summaries();
    crate::background::maintenance::poll_summaries();
    println!(
        "BACKGROUND_BENCHMARK {}",
        json!({
            "schema":schema,"seconds":seconds,"warmup_seconds":warmup,"response_delay_ms":25,
            "historical_work":260000,"historical_leases":240000,
            "nodes":6,"pool_max_connections":pool_max,"wire_commands":relay.commands.load(Ordering::SeqCst),
            "scope":"local fixture; SSH only to loopback port 1; not capacity acceptance",
        })
    );
}
