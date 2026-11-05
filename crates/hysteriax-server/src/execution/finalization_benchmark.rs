//! Run alone in a fresh test process: counters cover all logical instances.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "manual finalization acceptance; use tests/performance/verify-execution-finalization.py"]
async fn benchmark_execution_finalization() {
    super::supervision::counts::reset();
    let mut report =
        crate::node_limits::contention_benchmark::run_lock_contention_benchmark().await;
    let counts = super::supervision::counts::snapshot();
    report["execution_outcomes"] = counts.clone();
    report["profile"] = serde_json::json!(
        "real package/analytics/leased billing; separate instance pools/owners; 16 writers; one ready batch/s; no SSH/HTTP"
    );
    if let Ok(path) = std::env::var("HYSTERIAX_FINALIZATION_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    assert!(counts["committed_returned"].as_u64().unwrap() > 0);
    assert_eq!(
        counts["lost"], 0,
        "no execution is intentionally cancelled in this profile"
    );
    assert_eq!(counts["recovery_required"], 0);
    println!("FINALIZATION_REPORT {report}");
}
