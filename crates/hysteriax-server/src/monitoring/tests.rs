use super::*;

fn disconnected_state() -> AppState {
    use base64::Engine;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .unwrap();
    let key = base64::engine::general_purpose::STANDARD_NO_PAD.encode([0_u8; 32]);
    AppState::new(pool, crate::security::SecretBox::from_base64(&key).unwrap())
}

#[tokio::test]
async fn unavailable_database_still_returns_monitoring_and_nullable_disk_fields() {
    let Json(snapshot) = get(State(disconnected_state())).await.unwrap();
    let json = serde_json::to_value(snapshot).unwrap();
    assert_eq!(json["database"], "unavailable");
    assert_eq!(json["service_version"], env!("CARGO_PKG_VERSION"));
    assert!(json["cpu_usage_percent"].is_number());
    assert!(json.get("root_disk_used_bytes").is_some());
    assert!(json.get("root_disk_total_bytes").is_some());
}

#[tokio::test]
async fn monitoring_route_requires_admin_authentication() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = crate::api::router(disconnected_state());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let result = reqwest::get(format!("http://{address}/api/v2/server/monitoring")).await;
    server.abort();
    assert_eq!(result.unwrap().status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[test]
fn sampling_returns_bounded_resources_and_reuses_recent_observation() {
    let monitor = Monitor::new();
    let first = monitor.sample();
    let second = monitor.sample();
    assert_eq!(first.sampled_at, second.sampled_at);
    assert!((0.0..=100.0).contains(&first.cpu_usage_percent));
    assert!(first.memory_used_bytes <= first.memory_total_bytes);
    assert!(first.root_disk_used_bytes <= first.root_disk_total_bytes);
    assert!(chrono::DateTime::parse_from_rfc3339(&first.sampled_at).is_ok());
}
