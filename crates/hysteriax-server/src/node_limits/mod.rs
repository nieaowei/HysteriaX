//! Provider package metadata and independent whole-host network accounting.
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    db,
    error::ApiError,
    ssh::{self, FingerprintResult},
    state::AppState,
};

mod maintenance;

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
    sqlx::query("INSERT INTO audit_records (id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES ($1,$2,$3,$4,$5,$6,clock_timestamp())")
        .bind(Uuid::new_v4().to_string()).bind(actor).bind(action).bind(entity_type).bind(id).bind(detail).execute(&mut **tx).await?;
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
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    ensure(tx, id).await?;
    let old: Value =
        sqlx::query_scalar("SELECT config FROM node_packages WHERE node_id = $1 FOR NO KEY UPDATE")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
    let previous: Package = serde_json::from_value(old).map_err(|_| ApiError::internal())?;
    let now = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    rollover(tx, id, now).await.map_err(ApiError::from)?;
    let changed_meter = previous.interface != package.interface
        || previous.direction != package.direction
        || previous.quota_bytes.is_some() != package.quota_bytes.is_some();
    let changed_cycle = previous.cycle != package.cycle
        || previous.reset_day != package.reset_day
        || previous.timezone != package.timezone;
    sqlx::query("UPDATE node_packages SET config = $1, generation = generation + 1, next_reset_at = CASE WHEN $2 THEN $3 ELSE next_reset_at END, boot_id = CASE WHEN $4 THEN NULL ELSE boot_id END, sampled_at = CASE WHEN $4 THEN NULL ELSE sampled_at END, gap_reason = CASE WHEN $4 THEN 'meter_configuration_changed' ELSE gap_reason END WHERE node_id = $5")
        .bind(json!(package)).bind(changed_cycle).bind(next_reset(package, now)).bind(changed_meter).bind(id).execute(&mut **tx).await?;
    evaluate(tx, id, now).await.map_err(ApiError::from)?;
    Ok(())
}
async fn rollover(tx: &mut Transaction<'_, Postgres>, id: &str, now: DateTime<Utc>) -> Result<()> {
    let row = sqlx::query(
        "SELECT config, next_reset_at FROM node_packages WHERE node_id = $1 FOR NO KEY UPDATE",
    )
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
fn effective_usage(
    usage_bytes: i64,
    next_reset_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> i64 {
    if next_reset_at.is_some_and(|at| at <= now) {
        0
    } else {
        usage_bytes
    }
}

#[derive(sqlx::FromRow)]
pub(crate) struct PackageReading {
    pub(crate) config: Value,
    pub(crate) usage_bytes: i64,
    pub(crate) next_reset_at: Option<DateTime<Utc>>,
    pub(crate) interface: Option<String>,
    pub(crate) sampled_at: Option<DateTime<Utc>>,
    pub(crate) gap_reason: Option<String>,
}

// Both node details and the overview apply the same time-sensitive package rules.
pub(crate) fn snapshot_from_reading(
    reading: Option<&PackageReading>,
    alerts: Vec<Value>,
    failed: i64,
    pending: i64,
    now: DateTime<Utc>,
) -> Result<(Value, Value)> {
    let Some(row) = reading else {
        return Ok((
            json!(Package::default()),
            json!({"usage_bytes": 0, "restricted": false, "reasons": [], "alerts": [], "freshness": "not_collected"}),
        ));
    };
    let package: Package = serde_json::from_value(row.config.clone())?;
    let usage = effective_usage(row.usage_bytes, row.next_reset_at, now);
    let restrictions = reasons(&package, usage, now);
    Ok((
        json!(package),
        json!({"usage_bytes": usage, "restricted": !restrictions.is_empty(), "reasons": restrictions,
        "next_reset_at": next_reset(&package, now), "interface": row.interface,
        "sampled_at": row.sampled_at, "gap_reason": row.gap_reason,
        "freshness": if row.sampled_at.is_none() { "not_collected" } else if row.gap_reason.is_some() || row.sampled_at.is_some_and(|at| (now-at).num_seconds() > crate::runtime_config::sampling_freshness_seconds()) { "stale" } else { "fresh" },
        "pending_disconnects": pending, "failed_disconnects": if restrictions.is_empty() { 0 } else { failed }, "alerts": alerts}),
    ))
}

pub async fn restricted(pool: &PgPool, id: &str) -> Result<bool> {
    let mut tx = crate::read_snapshot::begin(pool).await?;
    let row = sqlx::query("SELECT config,usage_bytes,next_reset_at,clock_timestamp() AS database_now FROM node_packages WHERE node_id=$1")
        .bind(id).fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let package: Package = serde_json::from_value(row.get("config"))?;
    let now = row.get("database_now");
    Ok(!reasons(
        &package,
        effective_usage(row.get("usage_bytes"), row.get("next_reset_at"), now),
        now,
    )
    .is_empty())
}
pub async fn snapshot(pool: &PgPool, id: &str) -> Result<(Value, Value)> {
    let row = sqlx::query_as::<_, PackageReading>("SELECT config, usage_bytes, next_reset_at, interface, sampled_at, gap_reason FROM node_packages WHERE node_id = $1")
        .bind(id).fetch_optional(&mut *crate::read_snapshot::begin(pool).await?).await?;
    if row.is_none() {
        return snapshot_from_reading(None, vec![], 0, 0, Utc::now());
    }
    let now = Utc::now();
    let alerts = sqlx::query("SELECT id, kind, created_at FROM node_alerts WHERE node_id = $1 AND active ORDER BY created_at, id").bind(id).fetch_all(&mut *crate::read_snapshot::begin(pool).await?).await?;
    let alerts: Vec<Value> = alerts.iter().map(|r| json!({"id": r.get::<String,_>("id"), "kind": r.get::<String,_>("kind"), "created_at": r.get::<DateTime<Utc>,_>("created_at")})).collect();
    let failed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM kick_requests WHERE node_id=$1 AND reasons ? 'node_limit' AND state IN ('waiting_recovery','needs_attention')",
    )
    .bind(id)
    .fetch_one(&mut *crate::read_snapshot::begin(pool).await?)
    .await?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM kick_requests WHERE node_id=$1 AND reasons ? 'node_limit' AND state='active'",
    )
    .bind(id)
    .fetch_one(&mut *crate::read_snapshot::begin(pool).await?)
    .await?;
    snapshot_from_reading(row.as_ref(), alerts, failed, pending, now)
}

mod evaluation;

async fn evaluate(tx: &mut Transaction<'_, Postgres>, id: &str, now: DateTime<Utc>) -> Result<()> {
    evaluation::evaluate_many(tx, &[id.to_owned()], now).await
}

#[derive(Clone, Deserialize)]
pub struct UsageUpdate {
    expected_revision: i64,
    usage_bytes: i64,
    #[serde(default)]
    reset: bool,
}

pub(crate) async fn update_usage_on(
    tx: &mut Transaction<'static, Postgres>,
    id: String,
    input: UsageUpdate,
    secrets: std::sync::Arc<crate::security::SecretBox>,
    actor: String,
) -> Result<Value, ApiError> {
    if input.usage_bytes < 0 || input.expected_revision < 1 {
        return Err(ApiError::bad_request("usage and revision must be valid"));
    }
    let revision: Option<(i64, String)> =
        sqlx::query_as("SELECT desired_revision, state FROM nodes WHERE id = $1 FOR NO KEY UPDATE")
            .bind(&id)
            .fetch_optional(&mut **tx)
            .await?;
    let (revision, status) = revision.ok_or_else(|| ApiError::not_found("node"))?;
    if revision != input.expected_revision
        || matches!(status.as_str(), "deleting" | "delete_failed")
    {
        return Err(ApiError::conflict(
            "node changed or is being deleted; reload",
        ));
    }
    ensure(tx, &id).await?;
    let now = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    rollover(tx, &id, now).await.map_err(ApiError::from)?;
    sqlx::query("UPDATE node_packages SET usage_bytes = $1, generation = generation + 1, boot_id = NULL, sampled_at = NULL, gap_reason = 'usage_corrected', period_id = CASE WHEN $2 THEN $3 ELSE period_id END WHERE node_id = $4")
        .bind(input.usage_bytes).bind(input.reset).bind(Uuid::new_v4().to_string()).bind(&id).execute(&mut **tx).await?;
    sqlx::query(
        "UPDATE nodes SET desired_revision = desired_revision + 1, updated_at = $1 WHERE id = $2",
    )
    .bind(Utc::now())
    .bind(&id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) SELECT $1,node_id,$2,config_enc,content_sha256,$3 FROM config_versions WHERE node_id=$4 AND revision=$5")
        .bind(Uuid::new_v4().to_string()).bind(revision+1).bind(Utc::now()).bind(&id).bind(revision).execute(&mut **tx).await?;
    crate::credentials::reference_index::flush_node_on(tx, &secrets, &id).await?;
    audit(
        tx,
        &actor,
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
    evaluate(tx, &id, now).await.map_err(ApiError::from)?;
    Ok(json!({"id": id, "revision": revision + 1}))
}

pub async fn run(state: AppState) {
    loop {
        match maintenance::pass(&state.pool, &state.instance_id).await {
            Ok(processed) if processed > 0 => tokio::task::yield_now().await,
            result => {
                if let Err(error) = result {
                    tracing::error!(%error,"node limit maintenance deferred");
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

pub async fn collect(state: AppState) {
    crate::background::run_sampling(state, "network").await;
}
pub(crate) async fn sample_work(state: &AppState, work: &crate::background::Work) -> Result<()> {
    let generation = work.payload["capture_generation"]
        .as_i64()
        .context("missing network capture generation")?;
    let captured_at: DateTime<Utc> = serde_json::from_value(work.payload["captured_at"].clone())?;
    // A process may die after committing the sample but before acknowledging
    // its work item. Recovery must not perform a second read at the same time.
    if let Some(gap) = sqlx::query_scalar::<_, Option<String>>(
        "SELECT gap_reason FROM node_network_samples WHERE node_id=$1 AND sampled_at=$2 LIMIT 1",
    )
    .bind(&work.node)
    .bind(captured_at)
    .fetch_optional(&state.pool)
    .await?
    {
        if gap.as_deref() == Some("network_sample_failed") {
            bail!("network sample was already recorded as failed");
        }
        return Ok(());
    }
    crate::background::admit_read(&state.pool, work).await?;
    let result = tokio::time::timeout(
        Duration::from_secs(crate::runtime_config::operation_timeout_seconds(30)),
        sample(state, &work.node, generation, captured_at),
    )
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("whole-host network sampling timed out")));
    if result.is_err() {
        record_failure(&state.pool, &work.node, generation, captured_at).await?;
    }
    result
}

async fn record_failure(
    pool: &PgPool,
    id: &str,
    generation: i64,
    captured_at: DateTime<Utc>,
) -> Result<()> {
    let id = id.to_owned();
    db::write(pool,move |tx| {
        let id=id.clone();
        Box::pin(async move {
            let active:Option<bool>=sqlx::query_scalar("SELECT true FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
                .bind(&id).fetch_optional(&mut **tx).await?;
            if active.is_none() {return Ok(());}
            let row=sqlx::query("SELECT generation,sampled_at,period_id FROM node_packages WHERE node_id=$1 FOR NO KEY UPDATE")
                .bind(&id).fetch_optional(&mut **tx).await?;
            let Some(row)=row else {return Ok(());};
            if row.get::<i64,_>("generation")!=generation || row.get::<Option<DateTime<Utc>>,_>("sampled_at").is_some_and(|latest|latest>=captured_at) {return Ok(());}
            let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM node_network_samples WHERE node_id=$1 AND sampled_at=$2)")
                .bind(&id).bind(captured_at).fetch_one(&mut **tx).await?;
            if exists {return Ok(());}
            sqlx::query("UPDATE node_packages SET gap_reason='network_sample_failed' WHERE node_id=$1")
                .bind(&id).execute(&mut **tx).await?;
            sqlx::query("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,gap_reason,sampled_at) VALUES($1,$2,0,0,'network_sample_failed',$3)")
                .bind(&id).bind(row.get::<String,_>("period_id")).bind(captured_at).execute(&mut **tx).await?;
            Ok(())
        })
    }).await
}
#[derive(Clone, Debug)]
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
async fn sample(
    state: &AppState,
    id: &str,
    generation: i64,
    captured_at: DateTime<Utc>,
) -> Result<()> {
    let row = sqlx::query("SELECT config FROM node_packages WHERE node_id=$1 AND generation=$2")
        .bind(id)
        .bind(generation)
        .fetch_one(&state.pool)
        .await?;
    let package: Package = serde_json::from_value(row.get("config"))?;
    let node = crate::deployment::load_ssh_node(&state.pool, &state.secrets, id).await?;
    let FingerprintResult::Trusted(session) = ssh::connect(&node).await? else {
        bail!("untrusted SSH fingerprint");
    };
    crate::kick_requests::resume_node(&state.pool, id).await?;
    let select = match &package.interface {
        Some(name) => { if !valid_interface(name) { bail!("invalid interface"); } format!("iface='{name}'") },
        None => r#"iface=$(awk 'NR>1 && $2=="00000000" && $1!="lo" {if(!found || $7<metric) {found=1; metric=$7; iface=$1}} END {print iface}' /proc/net/route); [ -n "$iface" ] || iface=$(awk '$1=="00000000000000000000000000000000" && $2=="00" && $NF!="lo" {if(!found || ("x" $6)<("x" metric)) {found=1; metric=$6; iface=$NF}} END {print iface}' /proc/net/ipv6_route)"#.into(),
    };
    let command = format!(
        "set -eu; {select}; [ -n \"$iface\" ]; before=$(cat /proc/sys/kernel/random/boot_id); tx=$(cat \"/sys/class/net/$iface/statistics/tx_bytes\"); rx=$(cat \"/sys/class/net/$iface/statistics/rx_bytes\"); after=$(cat /proc/sys/kernel/random/boot_id); [ \"$before\" = \"$after\" ]; printf '%s %s %s %s\\n' \"$before\" \"$iface\" \"$tx\" \"$rx\""
    );
    let value = parse_sample(&session.execute_read_only_checked(&command).await?)?;
    apply_sample(&state.pool, id, generation, value, captured_at).await
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
    let id = id.to_owned();
    db::write(pool,move |tx| {
        let (id,sample)=(id.clone(),sample.clone());
        Box::pin(async move {
            let active:Option<bool>=sqlx::query_scalar("SELECT true FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
                .bind(&id).fetch_optional(&mut **tx).await?;
            if active.is_none() {return Ok(());}
            let row=sqlx::query("SELECT * FROM node_packages WHERE node_id=$1 FOR NO KEY UPDATE")
                .bind(&id).fetch_optional(&mut **tx).await?;
            let Some(row)=row else {return Ok(());};
            if row.get::<i64,_>("generation")!=generation || row.get::<Option<DateTime<Utc>>,_>("sampled_at").is_some_and(|previous|previous>=now) {return Ok(());}
            rollover(tx,&id,now).await?;
            let row=sqlx::query("SELECT * FROM node_packages WHERE node_id=$1").bind(&id).fetch_one(&mut **tx).await?;
            let package:Package=serde_json::from_value(row.get("config"))?;
            let (tx_delta,rx_delta,gap)=delta(&row,&sample);
            let charged=package.charged(tx_delta,rx_delta);
            sqlx::query("UPDATE node_packages SET usage_bytes=usage_bytes+least($1,9223372036854775807-usage_bytes),boot_id=$2,interface=$3,tx_total=$4,rx_total=$5,sampled_at=$6,gap_reason=$7 WHERE node_id=$8")
                .bind(charged).bind(sample.boot).bind(sample.interface).bind(sample.tx).bind(sample.rx).bind(now).bind(gap).bind(&id).execute(&mut **tx).await?;
            sqlx::query("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,gap_reason,sampled_at,baseline_only) VALUES($1,$2,$3,$4,$5,$6,$7)")
                .bind(&id).bind(row.get::<String,_>("period_id")).bind(tx_delta).bind(rx_delta).bind(gap).bind(now).bind(row.get::<Option<String>,_>("boot_id").is_none()).execute(&mut **tx).await?;
            evaluate(tx,&id,now).await?;
            Ok(())
        })
    }).await
}

#[cfg(test)]
mod tests;

// Direct domain fixtures are test-only; production writes use protocol-2 handlers.
#[cfg(test)]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::*;

#[cfg(test)]
use axum::Json;
#[cfg(test)]
use axum::extract::Path;
#[cfg(test)]
use axum::extract::State;

#[cfg(test)]
mod migration_tests;

#[cfg(test)]
pub(crate) mod contention_benchmark;
