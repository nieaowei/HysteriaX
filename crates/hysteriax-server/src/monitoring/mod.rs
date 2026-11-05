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
mod tests;
