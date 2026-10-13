//! Queries only the fields used by the dashboard, in one read-only snapshot.
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row, postgres::PgRow};
use std::collections::BTreeMap;

use crate::{error::ApiError, node_limits};

#[derive(Deserialize, Serialize)]
struct Alert {
    id: String,
    kind: String,
    created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
pub(super) struct NodeReading {
    id: String,
    name: String,
    state: String,
    revision: i64,
    deployed_revision: Option<i64>,
    updated_at: DateTime<Utc>,
    last_sample_at: Option<DateTime<Utc>>,
    open_gaps: i64,
    pending_revocations: i64,
    has_package: bool,
    #[sqlx(flatten)]
    package: node_limits::PackageReading,
    alerts: sqlx::types::Json<Vec<Alert>>,
    pending_disconnects: i64,
    failed_disconnects: i64,
    pub(super) online_sampled_at: Option<DateTime<Utc>>,
    pub(super) online_status: Option<String>,
    pub(super) online_connections: Option<Value>,
    pub(super) probe_sampled_at: Option<DateTime<Utc>>,
    pub(super) probe_revision: Option<i64>,
    pub(super) probe_status: Option<String>,
    pub(super) probe_external_status: Option<String>,
    pub(super) probe_connection_ms: Option<f64>,
    pub(super) probe_latency_ms: Option<f64>,
    pub(super) probe_reason: Option<String>,
    pub(super) probe_health: Option<String>,
}

impl NodeReading {
    pub(super) fn summary(&self, now: DateTime<Utc>) -> Result<Value, ApiError> {
        let alerts = self.alerts.0.iter().map(|alert| json!(alert)).collect();
        let (package, usage) = node_limits::snapshot_from_reading(
            self.has_package.then_some(&self.package),
            alerts,
            self.failed_disconnects,
            self.pending_disconnects,
            now,
        )
        .map_err(|_| ApiError::internal())?;
        let freshness = match self.last_sample_at {
            None => "not_collected",
            Some(at) if (now - at).num_seconds() <= 30 => "fresh",
            Some(_) => "stale",
        };
        Ok(
            json!({"id": self.id, "name": self.name, "state": self.state,
            "revision": self.revision, "deployed_revision": self.deployed_revision,
            "updated_at": self.updated_at, "last_sample_at": self.last_sample_at,
            "data_freshness": freshness, "open_gaps": self.open_gaps,
            "pending_revocations": self.pending_revocations,
            "package": package, "package_usage": usage}),
        )
    }
}

#[derive(sqlx::FromRow)]
pub(super) struct UserWarning {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) expires_at: Option<DateTime<Utc>>,
    pub(super) quota_bytes: Option<i64>,
    pub(super) usage_bytes: i64,
    pub(super) updated_at: DateTime<Utc>,
}

pub(super) struct SummaryData {
    pub(super) nodes: Vec<NodeReading>,
    pub(super) users: Vec<UserWarning>,
    pub(super) node_states: BTreeMap<String, i64>,
    pub(super) node_count: i64,
    pub(super) quota_ids: Vec<String>,
    pub(super) task: PgRow,
    pub(super) failures: Vec<PgRow>,
    pub(super) generated_at: DateTime<Utc>,
}

pub(super) async fn load(pool: &PgPool) -> Result<SummaryData, ApiError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let now = Utc::now();
    let rows = sqlx::query("SELECT state, count(*)::bigint AS count FROM nodes GROUP BY state")
        .fetch_all(&mut *tx)
        .await?;
    let node_states: BTreeMap<String, i64> = rows
        .iter()
        .map(|row| (row.get("state"), row.get("count")))
        .collect();
    let node_count = node_states.values().sum();
    let nodes = sqlx::query_as::<_, NodeReading>(include_str!("summary_nodes.sql"))
        .fetch_all(&mut *tx)
        .await?;
    let users = sqlx::query_as::<_, UserWarning>(
        "SELECT id, name, expires_at, quota_bytes, usage_bytes, updated_at
         FROM users WHERE enabled AND (expires_at <= $1 OR usage_bytes >= quota_bytes)
         ORDER BY lower(name) COLLATE \"C\", name COLLATE \"C\", id",
    )
    .bind(now + Duration::days(7))
    .fetch_all(&mut *tx)
    .await?;
    let quota_ids = sqlx::query_scalar(include_str!("summary_quota.sql"))
        .bind(now)
        .fetch_all(&mut *tx)
        .await?;
    let task = sqlx::query("SELECT count(*) FILTER(WHERE status='queued')::bigint queued,count(*) FILTER(WHERE status='running')::bigint running,count(*) FILTER(WHERE status='failed' AND finished_at >= $1)::bigint failed FROM jobs")
        .bind(now - Duration::hours(24)).fetch_one(&mut *tx).await?;
    let failures = sqlx::query(include_str!("summary_failures.sql"))
        .bind(now - Duration::hours(24))
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(SummaryData {
        nodes,
        users,
        node_states,
        node_count,
        quota_ids,
        task,
        failures,
        generated_at: now,
    })
}
