//! Provider package metadata and independent whole-host network accounting.
use std::time::Duration;

use anyhow::{Result, bail};
use axum::{
    Json,
    extract::{Path, State},
};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    api::enqueue_job_with_payload_in_tx,
    db,
    error::ApiError,
    ssh::{self, FingerprintResult},
    state::AppState,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Package {
    pub expires_at: Option<DateTime<Utc>>,
    pub quota_bytes: Option<i64>,
    pub cycle: String,
    pub reset_day: u32,
    pub timezone: String,
    pub interface: Option<String>,
    pub direction: String,
    pub expiry_warning_days: i64,
    pub traffic_warning_percent: i64,
}
impl Default for Package {
    fn default() -> Self {
        Self {
            expires_at: None,
            quota_bytes: None,
            cycle: "fixed".into(),
            reset_day: 1,
            timezone: "Asia/Shanghai".into(),
            interface: None,
            direction: "both".into(),
            expiry_warning_days: 7,
            traffic_warning_percent: 80,
        }
    }
}
impl Package {
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.quota_bytes.is_some_and(|x| x <= 0)
            || !matches!(self.cycle.as_str(), "fixed" | "monthly")
            || !(1..=31).contains(&self.reset_day)
            || self.timezone.parse::<Tz>().is_err()
            || !matches!(self.direction.as_str(), "tx" | "rx" | "both")
            || !(0..=365).contains(&self.expiry_warning_days)
            || !(1..=99).contains(&self.traffic_warning_percent)
            || self
                .interface
                .as_ref()
                .is_some_and(|name| !valid_interface(name))
        {
            return Err(ApiError::bad_request(
                "invalid node package: positive quota, fixed/monthly cycle, day 1–31, IANA timezone, tx/rx/both direction, warning days 0–365 and percent 1–99 are required",
            ));
        }
        Ok(())
    }
    fn charged(&self, tx: i64, rx: i64) -> i64 {
        match self.direction.as_str() {
            "tx" => tx,
            "rx" => rx,
            _ => tx.saturating_add(rx),
        }
    }
}
fn valid_interface(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 15
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|x| x.is_ascii_alphanumeric() || matches!(x, b'_' | b'-' | b'.' | b':'))
}

/// Month-end clamping and IANA timezone conversion, including DST midnight shifts.
fn monthly_boundary(package: &Package, year: i32, month: u32) -> DateTime<Utc> {
    let tz: Tz = package.timezone.parse().expect("validated timezone");
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let last = NaiveDate::from_ymd_opt(ny, nm, 1)
        .unwrap()
        .pred_opt()
        .unwrap()
        .day();
    let midnight = NaiveDate::from_ymd_opt(year, month, package.reset_day.min(last))
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    // Choose the first valid instant at or after local midnight on DST transitions.
    for minute in 0..=1440 {
        if let Some(value) = tz
            .from_local_datetime(&(midnight + chrono::Duration::minutes(minute)))
            .earliest()
        {
            return value.with_timezone(&Utc);
        }
    }
    unreachable!("timezone has no valid instant within a day")
}
fn next_reset(package: &Package, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if package.cycle != "monthly" {
        return None;
    }
    let local = now.with_timezone(&package.timezone.parse::<Tz>().expect("validated timezone"));
    let current = monthly_boundary(package, local.year(), local.month());
    if current > now {
        Some(current)
    } else {
        let (y, m) = if local.month() == 12 {
            (local.year() + 1, 1)
        } else {
            (local.year(), local.month() + 1)
        };
        Some(monthly_boundary(package, y, m))
    }
}

async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    actor: &str,
    action: &str,
    entity_type: &str,
    id: &str,
    detail: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO audit_records (id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7)")
        .bind(Uuid::new_v4().to_string()).bind(actor).bind(action).bind(entity_type).bind(id).bind(detail).bind(Utc::now()).execute(&mut **tx).await?;
    Ok(())
}
pub async fn ensure(tx: &mut Transaction<'_, Postgres>, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO node_packages (node_id, period_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(Uuid::new_v4().to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}
pub async fn save(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    package: &Package,
) -> Result<(), ApiError> {
    package.validate()?;
    ensure(tx, id).await?;
    let old: Value = sqlx::query_scalar("SELECT config FROM node_packages WHERE node_id = $1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    let previous: Package = serde_json::from_value(old).map_err(|_| ApiError::internal())?;
    rollover(tx, id, Utc::now())
        .await
        .map_err(|_| ApiError::internal())?;
    let changed_meter = previous.interface != package.interface
        || previous.direction != package.direction
        || previous.quota_bytes.is_some() != package.quota_bytes.is_some();
    let changed_cycle = previous.cycle != package.cycle
        || previous.reset_day != package.reset_day
        || previous.timezone != package.timezone;
    sqlx::query("UPDATE node_packages SET config = $1, generation = generation + 1, next_reset_at = CASE WHEN $2 THEN $3 ELSE next_reset_at END, boot_id = CASE WHEN $4 THEN NULL ELSE boot_id END, sampled_at = CASE WHEN $4 THEN NULL ELSE sampled_at END, gap_reason = CASE WHEN $4 THEN 'meter_configuration_changed' ELSE gap_reason END WHERE node_id = $5")
        .bind(json!(package)).bind(changed_cycle).bind(next_reset(package, Utc::now())).bind(changed_meter).bind(id).execute(&mut **tx).await?;
    evaluate(tx, id, Utc::now())
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(())
}
async fn rollover(tx: &mut Transaction<'_, Postgres>, id: &str, now: DateTime<Utc>) -> Result<()> {
    let row = sqlx::query("SELECT config, next_reset_at FROM node_packages WHERE node_id = $1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    let package: Package = serde_json::from_value(row.get("config"))?;
    let due: Option<DateTime<Utc>> = row.get("next_reset_at");
    if package.cycle == "monthly" && due.is_none_or(|x| x <= now) {
        sqlx::query("UPDATE node_packages SET usage_bytes = CASE WHEN next_reset_at IS NOT NULL THEN 0 ELSE usage_bytes END, period_id = $1, next_reset_at = $2 WHERE node_id = $3")
            .bind(Uuid::new_v4().to_string()).bind(next_reset(&package, now)).bind(id).execute(&mut **tx).await?;
    }
    Ok(())
}
fn reasons(package: &Package, usage: i64, now: DateTime<Utc>) -> Vec<&'static str> {
    let mut result = vec![];
    if package.expires_at.is_some_and(|x| x <= now) {
        result.push("expired");
    }
    if package.quota_bytes.is_some_and(|x| usage >= x) {
        result.push("quota_exhausted");
    }
    result
}
fn effective_usage(row: &sqlx::postgres::PgRow, now: DateTime<Utc>) -> i64 {
    if row
        .get::<Option<DateTime<Utc>>, _>("next_reset_at")
        .is_some_and(|x| x <= now)
    {
        0
    } else {
        row.get("usage_bytes")
    }
}
pub async fn restricted(pool: &PgPool, id: &str) -> Result<bool> {
    let Some(row) = sqlx::query(
        "SELECT config, usage_bytes, next_reset_at FROM node_packages WHERE node_id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(false);
    };
    let package: Package = serde_json::from_value(row.get("config"))?;
    let now = Utc::now();
    Ok(!reasons(&package, effective_usage(&row, now), now).is_empty())
}
pub async fn snapshot(pool: &PgPool, id: &str) -> Result<(Value, Value)> {
    let Some(row) = sqlx::query("SELECT * FROM node_packages WHERE node_id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
    else {
        return Ok((
            json!(Package::default()),
            json!({"usage_bytes": 0, "restricted": false, "reasons": [], "alerts": [], "freshness": "not_collected"}),
        ));
    };
    let package: Package = serde_json::from_value(row.get("config"))?;
    let now = Utc::now();
    let usage = effective_usage(&row, now);
    let restrictions = reasons(&package, usage, now);
    let sampled: Option<DateTime<Utc>> = row.get("sampled_at");
    let gap: Option<String> = row.get("gap_reason");
    let alerts = sqlx::query("SELECT id, kind, created_at FROM node_alerts WHERE node_id = $1 AND active ORDER BY created_at, id").bind(id).fetch_all(pool).await?;
    let alerts: Vec<Value> = alerts.iter().map(|r| json!({"id": r.get::<String,_>("id"), "kind": r.get::<String,_>("kind"), "created_at": r.get::<DateTime<Utc>,_>("created_at")})).collect();
    let failed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE node_id = $1 AND kind = 'kick' AND payload_json->>'node_limit' = 'true' AND status = 'failed'").bind(id).fetch_one(pool).await?;
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE node_id = $1 AND kind = 'kick' AND payload_json->>'node_limit' = 'true' AND status IN ('queued', 'running')").bind(id).fetch_one(pool).await?;
    Ok((
        json!(package),
        json!({"usage_bytes": usage, "restricted": !restrictions.is_empty(), "reasons": restrictions,
        "next_reset_at": next_reset(&package, now), "interface": row.get::<Option<String>,_>("interface"),
        "sampled_at": sampled, "gap_reason": gap,
        "freshness": if sampled.is_none() { "not_collected" } else if gap.is_some() || sampled.is_some_and(|x| (now-x).num_seconds() > 30) { "stale" } else { "fresh" },
        "pending_disconnects": pending, "failed_disconnects": if restrictions.is_empty() { 0 } else { failed }, "alerts": alerts}),
    ))
}

async fn evaluate(tx: &mut Transaction<'_, Postgres>, id: &str, now: DateTime<Utc>) -> Result<()> {
    rollover(tx, id, now).await?;
    let row = sqlx::query("SELECT * FROM node_packages WHERE node_id = $1")
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
    let package: Package = serde_json::from_value(row.get("config"))?;
    let usage: i64 = row.get("usage_bytes");
    let period: String = row.get("period_id");
    let restrictions = reasons(&package, usage, now);
    let mut alerts: Vec<(&str, String)> = vec![];
    if let Some(expiry) = package.expires_at {
        let kind = if expiry <= now {
            Some("expired")
        } else if expiry - now <= chrono::Duration::days(package.expiry_warning_days) {
            Some("expiring")
        } else {
            None
        };
        if let Some(kind) = kind {
            alerts.push((kind, expiry.to_rfc3339()));
        }
    }
    if let Some(quota) = package.quota_bytes {
        if usage >= quota {
            alerts.push(("quota_exhausted", period.clone()));
        } else if i128::from(usage) * 100
            >= i128::from(quota) * i128::from(package.traffic_warning_percent)
        {
            alerts.push(("traffic_warning", period.clone()));
        }
    }
    let desired: Vec<Value> = alerts
        .iter()
        .map(|(kind, scope)| json!({"kind":kind,"scope":scope}))
        .collect();
    sqlx::query("UPDATE node_alerts SET active = FALSE WHERE node_id = $1 AND active AND NOT EXISTS (SELECT 1 FROM jsonb_array_elements($2) desired WHERE desired->>'kind'=node_alerts.kind AND desired->>'scope'=node_alerts.scope)")
        .bind(id).bind(json!(desired))
        .execute(&mut **tx)
        .await?;
    for (kind, scope) in alerts {
        sqlx::query("INSERT INTO node_alerts (id, node_id, kind, scope, created_at) VALUES ($1,$2,$3,$4,$5) ON CONFLICT(node_id,kind,scope) DO UPDATE SET active = TRUE WHERE NOT node_alerts.active")
            .bind(Uuid::new_v4().to_string()).bind(id).bind(kind).bind(scope).bind(now).execute(&mut **tx).await?;
    }
    let blocked = !restrictions.is_empty();
    if blocked && !row.get::<bool, _>("restricted") {
        let users: Vec<String> =
            sqlx::query_scalar("SELECT user_id FROM node_assignments WHERE node_id = $1")
                .bind(id)
                .fetch_all(&mut **tx)
                .await?;
        for user in users {
            enqueue_job_with_payload_in_tx(
                tx,
                "kick",
                Some(id),
                None,
                json!({"user_id": user, "node_limit": true}),
            )
            .await
            .map_err(|e| anyhow::anyhow!(e.message))?;
        }
    }
    if blocked != row.get::<bool, _>("restricted") {
        sqlx::query("UPDATE node_packages SET restricted = $1 WHERE node_id = $2")
            .bind(blocked)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        audit(
            tx,
            "system",
            if blocked {
                "node.restricted"
            } else {
                "node.restored"
            },
            "node",
            id,
            json!({"reasons": restrictions}),
        )
        .await?;
        if !blocked {
            sqlx::query("UPDATE jobs SET status = 'cancelled', stage = 'cancelled', finished_at = $1, updated_at = $1 WHERE node_id = $2 AND kind = 'kick' AND payload_json->>'node_limit' = 'true' AND status = 'queued'")
                .bind(now).bind(id).execute(&mut **tx).await?;
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct UsageUpdate {
    expected_revision: i64,
    usage_bytes: i64,
    #[serde(default)]
    reset: bool,
}
pub async fn update_usage(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UsageUpdate>,
) -> Result<Json<Value>, ApiError> {
    if input.usage_bytes < 0 || input.expected_revision < 1 {
        return Err(ApiError::bad_request("usage and revision must be valid"));
    }
    let mut tx = db::begin_write(&state.pool).await?;
    let revision: Option<(i64, String)> =
        sqlx::query_as("SELECT desired_revision, state FROM nodes WHERE id = $1")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
    let (revision, status) = revision.ok_or_else(|| ApiError::not_found("node"))?;
    if revision != input.expected_revision
        || matches!(status.as_str(), "deleting" | "delete_failed")
    {
        return Err(ApiError::conflict(
            "node changed or is being deleted; reload",
        ));
    }
    ensure(&mut tx, &id).await?;
    rollover(&mut tx, &id, Utc::now())
        .await
        .map_err(|_| ApiError::internal())?;
    sqlx::query("UPDATE node_packages SET usage_bytes = $1, generation = generation + 1, boot_id = NULL, sampled_at = NULL, gap_reason = 'usage_corrected', period_id = CASE WHEN $2 THEN $3 ELSE period_id END WHERE node_id = $4")
        .bind(input.usage_bytes).bind(input.reset).bind(Uuid::new_v4().to_string()).bind(&id).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE nodes SET desired_revision = desired_revision + 1, updated_at = $1 WHERE id = $2",
    )
    .bind(Utc::now())
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) SELECT $1,node_id,$2,config_enc,content_sha256,$3 FROM config_versions WHERE node_id=$4 AND revision=$5")
        .bind(Uuid::new_v4().to_string()).bind(revision+1).bind(Utc::now()).bind(&id).bind(revision).execute(&mut *tx).await?;
    audit(
        &mut tx,
        "admin",
        if input.reset {
            "node.usage_reset"
        } else {
            "node.usage_corrected"
        },
        "node",
        &id,
        json!({"usage_bytes": input.usage_bytes}),
    )
    .await?;
    evaluate(&mut tx, &id, Utc::now())
        .await
        .map_err(|_| ApiError::internal())?;
    tx.commit().await?;
    Ok(Json(json!({"id": id, "revision": revision + 1})))
}

pub async fn run(state: AppState) {
    loop {
        if let Err(error) = check_all(&state.pool).await {
            tracing::error!(%error, "node limit evaluation failed");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
async fn check_all(pool: &PgPool) -> Result<()> {
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM nodes WHERE state NOT IN ('deleting','delete_failed')")
            .fetch_all(pool)
            .await?;
    for id in ids {
        let mut tx = db::begin_write(pool).await?;
        ensure(&mut tx, &id).await?;
        evaluate(&mut tx, &id, Utc::now()).await?;
        tx.commit().await?;
    }
    Ok(())
}

pub async fn collect(state: AppState) {
    let mut tasks = tokio::task::JoinSet::new();
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
    let mut inflight = std::collections::HashSet::new();
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let ids = sqlx::query_scalar::<_, String>("SELECT n.id FROM nodes n JOIN node_packages p ON n.id = p.node_id WHERE p.config->>'quota_bytes' IS NOT NULL AND n.state NOT IN ('deleting','delete_failed')").fetch_all(&state.pool).await;
                match ids {
                    Ok(ids) => for id in ids {
                        if !inflight.insert(id.clone()) { continue; }
                        let state = state.clone();
                        let slots = slots.clone();
                        tasks.spawn(async move {
                            let _permit = slots.acquire_owned().await.expect("collector semaphore remains open");
                            let result = tokio::time::timeout(Duration::from_secs(30), sample(&state, &id)).await;
                            if !matches!(result, Ok(Ok(()))) {
                                match &result {
                                    Ok(Err(error)) => tracing::warn!(node_id = %id, %error, "whole-host network sampling failed"),
                                    Err(_) => tracing::warn!(node_id = %id, "whole-host network sampling timed out"),
                                    _ => {}
                                }
                                let _ = record_failure(&state.pool, &id).await;
                            }
                            id
                        });
                    },
                    Err(error) => tracing::error!(%error, "network collector query failed"),
                }
            }
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Ok(id) = result { inflight.remove(&id); }
            }
        }
    }
}
async fn record_failure(pool: &PgPool, id: &str) -> Result<()> {
    let mut tx = db::begin_write(pool).await?;
    sqlx::query("UPDATE node_packages SET gap_reason = 'network_sample_failed' WHERE node_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,gap_reason,sampled_at) SELECT node_id,period_id,0,0,'network_sample_failed',$1 FROM node_packages WHERE node_id=$2")
        .bind(Utc::now()).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
#[derive(Debug)]
struct NetworkSample {
    boot: String,
    interface: String,
    tx: i64,
    rx: i64,
}
fn parse_sample(value: &str) -> Result<NetworkSample> {
    let parts: Vec<_> = value.split_whitespace().collect();
    if parts.len() != 4 || !valid_interface(parts[1]) || Uuid::parse_str(parts[0]).is_err() {
        bail!("invalid network sample");
    }
    let tx: i64 = parts[2].parse()?;
    let rx: i64 = parts[3].parse()?;
    if tx < 0 || rx < 0 {
        bail!("negative network counters");
    }
    Ok(NetworkSample {
        boot: parts[0].into(),
        interface: parts[1].into(),
        tx,
        rx,
    })
}
async fn sample(state: &AppState, id: &str) -> Result<()> {
    let row = sqlx::query("SELECT config, generation FROM node_packages WHERE node_id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    let package: Package = serde_json::from_value(row.get("config"))?;
    let generation: i64 = row.get("generation");
    let node = crate::deployment::load_ssh_node(&state.pool, &state.secrets, id).await?;
    let FingerprintResult::Trusted(session) = ssh::connect(&node).await? else {
        bail!("untrusted SSH fingerprint");
    };
    let select = match &package.interface {
        Some(name) => { if !valid_interface(name) { bail!("invalid interface"); } format!("iface='{name}'") },
        None => r#"iface=$(awk 'NR>1 && $2=="00000000" && $1!="lo" {if(!found || $7<metric) {found=1; metric=$7; iface=$1}} END {print iface}' /proc/net/route); [ -n "$iface" ] || iface=$(awk '$1=="00000000000000000000000000000000" && $2=="00" && $NF!="lo" {if(!found || ("x" $6)<("x" metric)) {found=1; metric=$6; iface=$NF}} END {print iface}' /proc/net/ipv6_route)"#.into(),
    };
    let command = format!(
        "set -eu; {select}; [ -n \"$iface\" ]; before=$(cat /proc/sys/kernel/random/boot_id); tx=$(cat \"/sys/class/net/$iface/statistics/tx_bytes\"); rx=$(cat \"/sys/class/net/$iface/statistics/rx_bytes\"); after=$(cat /proc/sys/kernel/random/boot_id); [ \"$before\" = \"$after\" ]; printf '%s %s %s %s\\n' \"$before\" \"$iface\" \"$tx\" \"$rx\""
    );
    let value = parse_sample(&session.execute_checked(&command).await?)?;
    apply_sample(&state.pool, id, generation, value, Utc::now()).await
}
fn delta(row: &sqlx::postgres::PgRow, sample: &NetworkSample) -> (i64, i64, Option<&'static str>) {
    let boot: Option<String> = row.get("boot_id");
    let iface: Option<String> = row.get("interface");
    let old_tx: Option<i64> = row.get("tx_total");
    let old_rx: Option<i64> = row.get("rx_total");
    match (boot, iface, old_tx, old_rx) {
        (None, _, _, _) => (0, 0, None),
        (Some(boot), Some(iface), Some(tx), Some(rx))
            if boot == sample.boot
                && iface == sample.interface
                && sample.tx >= tx
                && sample.rx >= rx =>
        {
            (sample.tx - tx, sample.rx - rx, None)
        }
        _ => (0, 0, Some("network_counter_reset_or_interface_changed")),
    }
}
async fn apply_sample(
    pool: &PgPool,
    id: &str,
    generation: i64,
    sample: NetworkSample,
    now: DateTime<Utc>,
) -> Result<()> {
    let mut tx = db::begin_write(pool).await?;
    let row = sqlx::query("SELECT p.* FROM node_packages p JOIN nodes n ON n.id=p.node_id WHERE p.node_id = $1 AND n.state NOT IN ('deleting','delete_failed')")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(row) = row else {
        return Ok(());
    };
    if row.get::<i64, _>("generation") != generation {
        return Ok(());
    }
    // Corrections invalidate in-flight samples and establish a fresh baseline.
    rollover(&mut tx, id, now).await?;
    let row = sqlx::query("SELECT * FROM node_packages WHERE node_id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let package: Package = serde_json::from_value(row.get("config"))?;
    let (tx_delta, rx_delta, gap) = delta(&row, &sample);
    let usage = row
        .get::<i64, _>("usage_bytes")
        .saturating_add(package.charged(tx_delta, rx_delta));
    sqlx::query("UPDATE node_packages SET usage_bytes=$1, boot_id=$2, interface=$3, tx_total=$4, rx_total=$5, sampled_at=$6, gap_reason=$7 WHERE node_id=$8")
        .bind(usage).bind(sample.boot).bind(sample.interface).bind(sample.tx).bind(sample.rx).bind(now).bind(gap).bind(id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,gap_reason,sampled_at) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(id).bind(row.get::<String,_>("period_id")).bind(tx_delta).bind(rx_delta).bind(gap).bind(now).execute(&mut *tx).await?;
    evaluate(&mut tx, id, now).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn instant(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }
    fn sample_value(tx: i64, rx: i64) -> NetworkSample {
        NetworkSample {
            boot: "11111111-1111-4111-8111-111111111111".into(),
            interface: "eth0".into(),
            tx,
            rx,
        }
    }
    async fn fixture() -> (AppState, String, String) {
        let pool = db::test_pool_with_max_connections(3).await;
        let key = base64::engine::general_purpose::STANDARD_NO_PAD.encode([3_u8; 32]);
        let state = AppState::new(pool, crate::security::SecretBox::from_base64(&key).unwrap());
        let (_, Json(created)) = crate::api::nodes::create(State(state.clone()), Json(serde_json::from_value(json!({
            "name":"Package node", "ssh_host":"127.0.0.1", "ssh_port":22, "ssh_username":"root",
            "ssh_auth_type":"password", "ssh_secret":"unused", "public_host":"node.example.test", "public_port":443, "listen_addr":":443"
        })).unwrap())).await.unwrap();
        (
            state,
            created["node"]["id"].as_str().unwrap().into(),
            created["node_auth_token"].as_str().unwrap().into(),
        )
    }
    async fn set_package(state: &AppState, id: &str, package: &Package) {
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        save(&mut tx, id, package).await.unwrap();
        tx.commit().await.unwrap();
    }
    async fn usage(pool: &PgPool, id: &str) -> i64 {
        sqlx::query_scalar("SELECT usage_bytes FROM node_packages WHERE node_id=$1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }
    async fn generation(pool: &PgPool, id: &str) -> i64 {
        sqlx::query_scalar("SELECT generation FROM node_packages WHERE node_id=$1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[test]
    fn month_end_leap_year_timezone_dst_and_exact_boundary() {
        let package = Package {
            cycle: "monthly".into(),
            reset_day: 31,
            ..Package::default()
        };
        assert_eq!(
            next_reset(&package, instant("2024-02-01T00:00:00Z")),
            Some(instant("2024-02-28T16:00:00Z"))
        );
        assert_eq!(
            next_reset(&package, instant("2025-02-01T00:00:00Z")),
            Some(instant("2025-02-27T16:00:00Z"))
        );
        assert_eq!(
            next_reset(&package, instant("2025-02-27T16:00:00Z")),
            Some(instant("2025-03-30T16:00:00Z"))
        );
        assert_eq!(
            next_reset(&package, instant("2025-12-31T00:00:00Z")),
            Some(instant("2026-01-30T16:00:00Z"))
        );
        let dst = Package {
            timezone: "America/New_York".into(),
            reset_day: 9,
            ..package
        };
        assert_eq!(
            next_reset(&dst, instant("2025-03-08T12:00:00Z")),
            Some(instant("2025-03-09T05:00:00Z"))
        );
        assert_eq!(
            next_reset(&dst, instant("2025-03-09T05:00:00Z")),
            Some(instant("2025-04-09T04:00:00Z"))
        );
    }
    #[test]
    fn validates_config_counters_and_threshold_boundaries() {
        for bad in [
            json!({"quota_bytes":0}),
            json!({"quota_bytes":-1}),
            json!({"timezone":"invalid"}),
            json!({"interface":"eth0'; exit"}),
            json!({"reset_day":32}),
            json!({"traffic_warning_percent":100}),
            json!({"expiry_warning_days":366}),
            json!({"direction":"bad"}),
        ] {
            assert!(
                serde_json::from_value::<Package>(bad)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        assert!(parse_sample("11111111-1111-4111-8111-111111111111 eth0 5 8").is_ok());
        assert!(parse_sample("bad eth0 5 8").is_err());
        assert!(parse_sample("11111111-1111-4111-8111-111111111111 eth0 -1 8").is_err());
        let package = Package {
            quota_bytes: Some(i64::MAX),
            ..Package::default()
        };
        assert!(reasons(&package, i64::MAX - 1, Utc::now()).is_empty());
        assert_eq!(
            reasons(&package, i64::MAX, Utc::now()),
            vec!["quota_exhausted"]
        );
    }
    #[tokio::test]
    async fn first_sample_deltas_duplicates_directions_and_counter_resets() {
        let (state, id, _) = fixture().await;
        for (direction, expected) in [("both", 30), ("tx", 10), ("rx", 20)] {
            set_package(
                &state,
                &id,
                &Package {
                    quota_bytes: Some(1000),
                    direction: direction.into(),
                    ..Package::default()
                },
            )
            .await;
            sqlx::query("UPDATE node_packages SET usage_bytes=0,boot_id=NULL WHERE node_id=$1")
                .bind(&id)
                .execute(&state.pool)
                .await
                .unwrap();
            let generation = generation(&state.pool, &id).await;
            apply_sample(
                &state.pool,
                &id,
                generation,
                sample_value(1000, 2000),
                Utc::now(),
            )
            .await
            .unwrap();
            assert_eq!(usage(&state.pool, &id).await, 0);
            apply_sample(
                &state.pool,
                &id,
                generation,
                sample_value(1010, 2020),
                Utc::now(),
            )
            .await
            .unwrap();
            apply_sample(
                &state.pool,
                &id,
                generation,
                sample_value(1010, 2020),
                Utc::now(),
            )
            .await
            .unwrap();
            assert_eq!(usage(&state.pool, &id).await, expected);
            let mut changed = sample_value(3000, 4000);
            changed.boot = Uuid::new_v4().to_string();
            apply_sample(&state.pool, &id, generation, changed, Utc::now())
                .await
                .unwrap();
            assert_eq!(usage(&state.pool, &id).await, expected);
            apply_sample(
                &state.pool,
                &id,
                generation,
                sample_value(10, 20),
                Utc::now(),
            )
            .await
            .unwrap();
            assert_eq!(usage(&state.pool, &id).await, expected);
            let mut changed = sample_value(100, 200);
            changed.interface = "ens3".into();
            apply_sample(&state.pool, &id, generation, changed, Utc::now())
                .await
                .unwrap();
            assert_eq!(usage(&state.pool, &id).await, expected);
        }
    }
    #[tokio::test]
    async fn correction_invalidates_inflight_samples_and_revisions_without_sync() {
        let (state, id, _) = fixture().await;
        set_package(
            &state,
            &id,
            &Package {
                quota_bytes: Some(1000),
                ..Package::default()
            },
        )
        .await;
        let old_generation = generation(&state.pool, &id).await;
        apply_sample(
            &state.pool,
            &id,
            old_generation,
            sample_value(100, 100),
            Utc::now(),
        )
        .await
        .unwrap();
        let _ = update_usage(
            State(state.clone()),
            Path(id.clone()),
            Json(UsageUpdate {
                expected_revision: 1,
                usage_bytes: 500,
                reset: false,
            }),
        )
        .await
        .unwrap();
        apply_sample(
            &state.pool,
            &id,
            old_generation,
            sample_value(300, 300),
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(usage(&state.pool, &id).await, 500);
        let new_generation = generation(&state.pool, &id).await;
        apply_sample(
            &state.pool,
            &id,
            new_generation,
            sample_value(350, 350),
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(usage(&state.pool, &id).await, 500);
        apply_sample(
            &state.pool,
            &id,
            new_generation,
            sample_value(360, 360),
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(usage(&state.pool, &id).await, 520);
        assert!(
            update_usage(
                State(state.clone()),
                Path(id.clone()),
                Json(UsageUpdate {
                    expected_revision: 1,
                    usage_bytes: 0,
                    reset: true
                })
            )
            .await
            .is_err()
        );
        let versions: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM config_versions WHERE node_id=$1")
                .bind(&id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(versions, 2);
        let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(jobs, 0);
    }
    #[tokio::test]
    async fn warnings_deduplicate_and_monthly_rollover_catches_up_after_downtime() {
        let (state, id, _) = fixture().await;
        let package = Package {
            quota_bytes: Some(1000),
            cycle: "monthly".into(),
            ..Package::default()
        };
        set_package(&state, &id, &package).await;
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        sqlx::query("UPDATE node_packages SET usage_bytes=799 WHERE node_id=$1")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .unwrap();
        evaluate(&mut tx, &id, Utc::now()).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM node_alerts WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(count, 0);
        sqlx::query("UPDATE node_packages SET usage_bytes=800 WHERE node_id=$1")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .unwrap();
        evaluate(&mut tx, &id, Utc::now()).await.unwrap();
        evaluate(&mut tx, &id, Utc::now()).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM node_alerts WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(count, 1);
        sqlx::query("UPDATE node_packages SET next_reset_at='2020-01-01T00:00:00Z',usage_bytes=1000 WHERE node_id=$1").bind(&id).execute(&mut *tx).await.unwrap();
        evaluate(&mut tx, &id, Utc::now()).await.unwrap();
        let row =
            sqlx::query("SELECT usage_bytes,next_reset_at FROM node_packages WHERE node_id=$1")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(row.get::<i64, _>("usage_bytes"), 0);
        assert!(row.get::<DateTime<Utc>, _>("next_reset_at") > Utc::now());
        tx.commit().await.unwrap();
        assert!(!restricted(&state.pool, &id).await.unwrap());
        assert!(
            snapshot(&state.pool, &id).await.unwrap().1["alerts"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn expiry_auth_is_node_local_probe_survives_and_renewal_cancels_old_kicks() {
        let (state, id, token) = fixture().await;
        sqlx::query(
            "INSERT INTO users(id,name,created_at,updated_at) VALUES ('user','User',now(),now())",
        )
        .execute(&state.pool)
        .await
        .unwrap();
        let credential = "credential";
        sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES ('user',$1,$2,'unused',now())").bind(&id).bind(crate::security::token_digest(credential)).execute(&state.pool).await.unwrap();
        let request = || {
            Json(
                serde_json::from_value(json!({"addr":"127.0.0.1:1","auth":credential,"tx":0}))
                    .unwrap(),
            )
        };
        let auth = crate::api::subscriptions::hy2_auth(
            State(state.clone()),
            Path((id.clone(), token.clone())),
            request(),
        )
        .await
        .unwrap();
        assert_eq!(serde_json::to_value(auth.0).unwrap()["ok"], true);
        set_package(
            &state,
            &id,
            &Package {
                expires_at: Some(Utc::now() - chrono::Duration::seconds(1)),
                ..Package::default()
            },
        )
        .await;
        let auth = crate::api::subscriptions::hy2_auth(
            State(state.clone()),
            Path((id.clone(), token.clone())),
            request(),
        )
        .await
        .unwrap();
        assert_eq!(serde_json::to_value(auth.0).unwrap()["ok"], false);
        let (_,Json(other))=crate::api::nodes::create(State(state.clone()),Json(serde_json::from_value(json!({
            "name":"Unlimited node","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"unused","public_host":"other.example.test","public_port":443,"listen_addr":":443"
        })).unwrap())).await.unwrap();
        let other_id = other["node"]["id"].as_str().unwrap();
        sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) SELECT user_id,$1,credential_hash,credential_enc,created_at FROM node_assignments WHERE node_id=$2")
            .bind(other_id).bind(&id).execute(&state.pool).await.unwrap();
        let other_auth = crate::api::subscriptions::hy2_auth(
            State(state.clone()),
            Path((
                other_id.into(),
                other["node_auth_token"].as_str().unwrap().into(),
            )),
            request(),
        )
        .await
        .unwrap();
        assert_eq!(serde_json::to_value(other_auth.0).unwrap()["ok"], true);
        sqlx::query("UPDATE node_assignments SET credential_enc=$1 WHERE user_id='user'")
            .bind(state.secrets.encrypt(credential).unwrap())
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE nodes SET deployed_revision=1,deployed_config_enc=$1 WHERE id IN ($2,$3)",
        )
        .bind(state.secrets.encrypt("{}").unwrap())
        .bind(&id)
        .bind(other_id)
        .execute(&state.pool)
        .await
        .unwrap();
        let subscriptions = crate::api::subscriptions::subscription_node_count(&state, "user")
            .await
            .unwrap();
        assert_eq!(subscriptions, 1);
        check_all(&state.pool).await.unwrap();
        let jobs: Vec<(String, String)> =
            sqlx::query_as("SELECT node_id,status FROM jobs WHERE kind='kick'")
                .fetch_all(&state.pool)
                .await
                .unwrap();
        assert_eq!(jobs, vec![(id.clone(), "queued".into())]);
        sqlx::query(
            "INSERT INTO deployment_probe_tokens(node_id,token_hash,expires_at) VALUES ($1,$2,$3)",
        )
        .bind(&id)
        .bind(crate::security::token_digest("probe"))
        .bind(Utc::now() + chrono::Duration::minutes(1))
        .execute(&state.pool)
        .await
        .unwrap();
        let probe = crate::api::subscriptions::hy2_auth(
            State(state.clone()),
            Path((id.clone(), token.clone())),
            Json(serde_json::from_value(json!({"addr":"","auth":"probe","tx":0})).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(serde_json::to_value(probe.0).unwrap()["ok"], true);
        set_package(&state, &id, &Package::default()).await;
        assert!(!restricted(&state.pool, &id).await.unwrap());
        let stale = crate::deployment::JobInput {
            id: "stale".into(),
            kind: "kick".into(),
            node_id: Some(id.clone()),
            target_revision: None,
            payload: json!({"user_id":"user","node_limit":true}),
            attempts: 1,
        };
        let skipped = crate::deployment::run_job(&state.pool, &state.secrets, &stale)
            .await
            .unwrap();
        assert_eq!(skipped.stage, "restriction_cleared");
        assert_eq!(skipped.result["skipped"], true);
        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "cancelled");
        let auth =
            crate::api::subscriptions::hy2_auth(State(state.clone()), Path((id, token)), request())
                .await
                .unwrap();
        assert_eq!(serde_json::to_value(auth.0).unwrap()["ok"], true);
    }
    #[tokio::test]
    async fn package_patch_preserves_usage_clears_limits_and_never_queues_sync() {
        let (state, id, _) = fixture().await;
        let package = Package {
            quota_bytes: Some(1000),
            ..Package::default()
        };
        let response = crate::api::nodes::patch(
            State(state.clone()),
            Path(id.clone()),
            Json(serde_json::from_value(json!({"expected_revision":1,"package":package})).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(response.0["sync_job_queued"], false);
        assert_eq!(response.0["revision"], 2);
        let _ = update_usage(
            State(state.clone()),
            Path(id.clone()),
            Json(UsageUpdate {
                expected_revision: 2,
                usage_bytes: 900,
                reset: false,
            }),
        )
        .await
        .unwrap();
        let response = crate::api::nodes::patch(
            State(state.clone()),
            Path(id.clone()),
            Json(serde_json::from_value(json!({"expected_revision":3,"name":"Renamed"})).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(response.0["sync_job_queued"], false);
        assert_eq!(
            snapshot(&state.pool, &id).await.unwrap().0["quota_bytes"],
            1000
        );
        let _ = crate::api::nodes::patch(
            State(state.clone()),
            Path(id.clone()),
            Json(
                serde_json::from_value(json!({"expected_revision":4,"package":Package::default()}))
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
        let (package, status) = snapshot(&state.pool, &id).await.unwrap();
        assert!(package["quota_bytes"].is_null());
        assert_eq!(status["usage_bytes"], 900);
        assert_eq!(status["restricted"], false);
        assert!(status["alerts"].as_array().unwrap().is_empty());
        let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(jobs, 0);
        let invalid = crate::api::nodes::patch(
            State(state.clone()),
            Path(id),
            Json(
                serde_json::from_value(json!({"expected_revision":5,"package":{"quota_bytes":-1}}))
                    .unwrap(),
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(invalid.status, axum::http::StatusCode::BAD_REQUEST);
    }
    #[tokio::test]
    async fn concurrent_corrections_allow_only_one_revision_and_failure_retains_usage() {
        let (state, id, _) = fixture().await;
        set_package(
            &state,
            &id,
            &Package {
                quota_bytes: Some(1000),
                ..Package::default()
            },
        )
        .await;
        let (a, b) = tokio::join!(
            update_usage(
                State(state.clone()),
                Path(id.clone()),
                Json(UsageUpdate {
                    expected_revision: 1,
                    usage_bytes: 500,
                    reset: false
                })
            ),
            update_usage(
                State(state.clone()),
                Path(id.clone()),
                Json(UsageUpdate {
                    expected_revision: 1,
                    usage_bytes: 600,
                    reset: false
                })
            )
        );
        assert_ne!(a.is_ok(), b.is_ok());
        let retained = usage(&state.pool, &id).await;
        record_failure(&state.pool, &id).await.unwrap();
        assert_eq!(usage(&state.pool, &id).await, retained);
        assert_eq!(
            snapshot(&state.pool, &id).await.unwrap().1["gap_reason"],
            "network_sample_failed"
        );
    }
    #[tokio::test]
    async fn monthly_reset_does_not_override_expiry_and_crossing_sample_charges_new_period() {
        let (state, id, _) = fixture().await;
        let package = Package {
            expires_at: Some(Utc::now() - chrono::Duration::days(1)),
            quota_bytes: Some(1000),
            cycle: "monthly".into(),
            ..Package::default()
        };
        set_package(&state, &id, &package).await;
        let generation = generation(&state.pool, &id).await;
        apply_sample(
            &state.pool,
            &id,
            generation,
            sample_value(100, 100),
            Utc::now(),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE node_packages SET usage_bytes=1000,next_reset_at='2020-01-01T00:00:00Z' WHERE node_id=$1").bind(&id).execute(&state.pool).await.unwrap();
        apply_sample(
            &state.pool,
            &id,
            generation,
            sample_value(110, 120),
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(usage(&state.pool, &id).await, 30);
        assert!(restricted(&state.pool, &id).await.unwrap());
        let (_, status) = snapshot(&state.pool, &id).await.unwrap();
        assert_eq!(status["reasons"], json!(["expired"]));
        apply_sample(
            &state.pool,
            &id,
            generation,
            sample_value(110, 120),
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(usage(&state.pool, &id).await, 30);
    }
    #[tokio::test]
    async fn migration_backfills_unlimited_nodes_and_deletion_cascades_package_history() {
        let (state, id, _) = fixture().await;
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        sqlx::raw_sql(
            "DROP TABLE node_alerts; DROP TABLE node_network_samples; DROP TABLE node_packages;",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!("../migrations/0003_node_packages.sql"))
            .execute(&mut *tx)
            .await
            .unwrap();
        let row =
            sqlx::query("SELECT config,usage_bytes,restricted FROM node_packages WHERE node_id=$1")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        let package: Package = serde_json::from_value(row.get("config")).unwrap();
        assert_eq!(package, Package::default());
        assert_eq!(row.get::<i64, _>("usage_bytes"), 0);
        assert!(!row.get::<bool, _>("restricted"));
        sqlx::query("DELETE FROM nodes WHERE id=$1")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM node_packages")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(count, 0);
        tx.commit().await.unwrap();
    }
}
