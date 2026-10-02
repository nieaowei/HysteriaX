use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::{Json, extract::State};
use serde::Serialize;
use sysinfo::{Disks, System};

use crate::{error::ApiError, state::AppState};

pub struct Monitor {
    started: Instant,
    sampler: Mutex<Option<Sampler>>,
}

struct Sampler {
    system: System,
    sampled: Instant,
    snapshot: Resources,
}

#[derive(Clone, Serialize)]
pub struct Resources {
    sampled_at: String,
    hostname: Option<String>,
    os: Option<String>,
    host_uptime_seconds: u64,
    cpu_count: usize,
    cpu_usage_percent: f32,
    memory_used_bytes: u64,
    memory_total_bytes: u64,
    root_disk_used_bytes: Option<u64>,
    root_disk_total_bytes: Option<u64>,
}

#[derive(Serialize)]
pub struct ServerMonitoring {
    service_version: &'static str,
    service_uptime_seconds: u64,
    database: &'static str,
    #[serde(flatten)]
    resources: Resources,
}

impl Monitor {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            sampler: Mutex::new(None),
        }
    }

    fn sample(&self) -> Resources {
        let mut sampler = self
            .sampler
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(previous) = sampler.as_ref()
            && previous.sampled.elapsed() < Duration::from_secs(2)
        {
            return previous.snapshot.clone();
        }
        let mut system = sampler
            .take()
            .map(|previous| previous.system)
            .unwrap_or_else(|| {
                let mut system = System::new();
                system.refresh_cpu_usage();
                // CPU utilization needs two observations; this runs on a blocking worker.
                std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
                system
            });
        system.refresh_cpu_usage();
        system.refresh_memory();
        let disks = Disks::new_with_refreshed_list();
        let root = disks
            .list()
            .iter()
            .find(|disk| disk.mount_point() == std::path::Path::new("/"));
        let snapshot = Resources {
            sampled_at: chrono::Utc::now().to_rfc3339(),
            hostname: System::host_name(),
            os: System::long_os_version(),
            host_uptime_seconds: System::uptime(),
            cpu_count: system.cpus().len(),
            cpu_usage_percent: system.global_cpu_usage().clamp(0.0, 100.0),
            memory_used_bytes: system.used_memory(),
            memory_total_bytes: system.total_memory(),
            root_disk_used_bytes: root
                .map(|disk| disk.total_space().saturating_sub(disk.available_space())),
            root_disk_total_bytes: root.map(|disk| disk.total_space()),
        };
        *sampler = Some(Sampler {
            system,
            sampled: Instant::now(),
            snapshot: snapshot.clone(),
        });
        snapshot
    }
}

pub async fn get(State(state): State<AppState>) -> Result<Json<ServerMonitoring>, ApiError> {
    let database = match tokio::time::timeout(
        Duration::from_secs(2),
        sqlx::query_scalar::<_, i64>("SELECT 1::BIGINT").fetch_one(&state.pool),
    )
    .await
    {
        Ok(Ok(1)) => "ok",
        _ => "unavailable",
    };
    let monitor = state.monitor.clone();
    let resources = tokio::task::spawn_blocking(move || monitor.sample())
        .await
        .map_err(|_| {
            ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "monitoring_unavailable",
                "Server monitoring is unavailable",
            )
        })?;
    Ok(Json(ServerMonitoring {
        service_version: env!("CARGO_PKG_VERSION"),
        service_uptime_seconds: state.monitor.started.elapsed().as_secs(),
        database,
        resources,
    }))
}

#[cfg(test)]
mod tests {
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
        let result = reqwest::get(format!("http://{address}/api/v1/server/monitoring")).await;
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
}
