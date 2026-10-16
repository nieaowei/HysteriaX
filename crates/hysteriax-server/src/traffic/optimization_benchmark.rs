//! Opt-in local measurement, driven by scripts/benchmark-optimization.py.
use super::*;
use std::time::Instant;

#[tokio::test]
#[ignore = "requires the isolated fixture created by benchmark-optimization.py"]
async fn optimization_sampling_benchmark() {
    let url =
        std::env::var("HYSTERIAX_BENCH_DATABASE_URL").expect("isolated benchmark database URL");
    let pool = db::pool_options().connect(&url).await.unwrap();
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        schema.starts_with("hysteriax_test_"),
        "benchmark requires a disposable schema"
    );
    let mut samples = Vec::new();
    for round in 1..=3 {
        let counters: Map<String, Value> = (0..1000)
            .map(|i| {
                (
                    format!("bench-user-{i}"),
                    serde_json::json!({"tx":round*100,"rx":round*200}),
                )
            })
            .collect();
        let start = Instant::now();
        apply_sample(&pool, "bench-node-0", "benchmark-instance", &counters)
            .await
            .unwrap();
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    // A separate, explicitly synthetic contention workload measures pool wait
    // independently from advisory-lock wait. It uses the production pool options.
    let mut tasks = tokio::task::JoinSet::new();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(24));
    for _ in 0..24 {
        let pool = pool.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            let start = Instant::now();
            let mut connection = pool.acquire().await.unwrap();
            let pool_ms = start.elapsed().as_secs_f64() * 1000.0;
            let start = Instant::now();
            let mut tx = sqlx::Acquire::begin(&mut *connection).await.unwrap();
            sqlx::query("SELECT pg_advisory_xact_lock(62177345902811)")
                .execute(&mut *tx)
                .await
                .unwrap();
            let lock_ms = start.elapsed().as_secs_f64() * 1000.0;
            sqlx::query("SELECT pg_sleep(0.01)")
                .execute(&mut *tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            (pool_ms, lock_ms)
        });
    }
    let mut pool_wait = Vec::new();
    let mut lock_wait = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let (pool_ms, lock_ms) = result.unwrap();
        pool_wait.push(pool_ms);
        lock_wait.push(lock_ms);
    }
    println!(
        "OPTIMIZATION_SAMPLE_METRICS={}",
        serde_json::json!({
            "sample_1000_users_ms":samples,
            "synthetic_pool_acquire_ms":pool_wait,
            "synthetic_advisory_lock_ms":lock_wait
        })
    );
    pool.close().await;
}
