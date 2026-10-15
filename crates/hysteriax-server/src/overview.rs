//! Read-only dashboard aggregation. Accounting data is never rewritten here.
use crate::{error::ApiError, state::AppState};
use axum::{
    Json,
    extract::{Query, State},
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub(crate) mod summary;

pub fn real_connections(value: Value, users: &BTreeSet<String>) -> Option<BTreeMap<String, i64>> {
    let object = value.as_object()?;
    let mut result = BTreeMap::new();
    for (id, count) in object {
        let count = i64::try_from(count.as_u64()?).ok()?;
        if count > 0 && users.contains(id) {
            result.insert(id.clone(), count);
        }
    }
    Some(result)
}

pub async fn record_online(
    pool: &PgPool,
    node: &str,
    cycle: Uuid,
    cycle_started: DateTime<Utc>,
    expected_nodes: i64,
    body: Option<&[u8]>,
) -> anyhow::Result<()> {
    let ids: BTreeSet<String> =
        sqlx::query_scalar::<_, String>("SELECT user_id FROM node_assignments WHERE node_id=$1")
            .bind(node)
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
    let parsed = body
        .and_then(|b| serde_json::from_slice(b).ok())
        .and_then(|v| real_connections(v, &ids));
    let status = if parsed.is_some() { "ok" } else { "failed" };
    sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,status,connections,cycle_started_at,expected_nodes) VALUES($1,$2,now(),$3,$4,$5,$6) ON CONFLICT(cycle_id,node_id) DO NOTHING")
        .bind(cycle).bind(node).bind(status).bind(json!(parsed.unwrap_or_default())).bind(cycle_started).bind(expected_nodes).execute(pool).await?;
    Ok(())
}

pub async fn finish_traffic_sample(pool: &PgPool, node: &str, cycle: Uuid, status: &str) {
    if let Err(error) =
        sqlx::query("UPDATE online_samples SET traffic_status=$3 WHERE node_id=$1 AND cycle_id=$2")
            .bind(node)
            .bind(cycle)
            .bind(status)
            .execute(pool)
            .await
    {
        tracing::warn!(node_id=node,%error,"traffic monitoring quality could not be saved");
    }
}

fn issue(
    entity: &str,
    id: &str,
    name: &str,
    kind: &str,
    severity: i32,
    reason: &str,
    at: Value,
) -> Value {
    json!({"id":format!("{entity}:{id}:{kind}"),"entity_type":entity,"entity_id":id,"name":name,"kind":kind,"severity":severity,"reason":reason,"occurred_at":at})
}

fn package_warning(kind: &str) -> &str {
    match kind {
        "expired" => "套餐已到期",
        "expiry_warning" => "套餐即将到期",
        "quota_exhausted" => "节点额度已耗尽",
        "traffic_warning" => "节点用量达到预警阈值",
        _ => kind,
    }
}

#[derive(Default)]
struct MonitoringTotals {
    active_ids: BTreeSet<String>,
    connection_count: i64,
    covered: i64,
    eligible: i64,
    healthy: i64,
}

fn monitoring_node(
    reading: &summary::NodeReading,
    node: &Value,
    now: DateTime<Utc>,
    totals: &mut MonitoringTotals,
    issues: &mut Vec<Value>,
) -> Result<Value, ApiError> {
    let id = node["id"].as_str().unwrap_or_default();
    let name = node["name"].as_str().unwrap_or_default();
    let status = node["state"].as_str().unwrap_or("unknown");
    let mut online_status = "unknown";
    let mut sampled_at: Option<DateTime<Utc>> = None;
    let mut node_users: Option<usize> = None;
    let mut node_connections: Option<i64> = None;
    let is_eligible =
        !node["deployed_revision"].is_null() && !matches!(status, "deleting" | "delete_failed");
    if is_eligible {
        totals.eligible += 1;
    }
    if let Some(at) = reading.online_sampled_at {
        sampled_at = Some(at);
        online_status = if now - at > Duration::seconds(90) {
            "stale"
        } else if reading.online_status.as_deref() == Some("ok") {
            "fresh"
        } else {
            "failed"
        };
        if online_status == "fresh" && is_eligible {
            totals.covered += 1;
            let values = reading
                .online_connections
                .as_ref()
                .ok_or_else(ApiError::internal)?;
            let map = values.as_object().cloned().unwrap_or_default();
            node_users = Some(map.len());
            let total = map.values().filter_map(Value::as_i64).sum::<i64>();
            node_connections = Some(total);
            totals.connection_count += total;
            totals.active_ids.extend(map.keys().cloned());
        }
    }
    let mut probe_status = "unknown";
    let mut inlet_status = "unknown";
    let mut probe_at: Option<DateTime<Utc>> = None;
    let mut latency: Option<f64> = None;
    let mut connection_ms: Option<f64> = None;
    let mut external: Option<String> = None;
    let mut reason: Option<String> = None;
    if let Some(at) = reading.probe_sampled_at {
        probe_at = Some(at);
        latency = reading.probe_latency_ms;
        connection_ms = reading.probe_connection_ms;
        external = reading.probe_external_status.clone();
        reason = reading.probe_reason.clone();
        let revision = reading.probe_revision.ok_or_else(ApiError::internal)?;
        inlet_status = if reading.probe_status.as_deref() == Some("ok") {
            "ok"
        } else if reading.probe_status.as_deref() == Some("unconfigured") {
            "unconfigured"
        } else {
            "failed"
        };
        let health = &reading.probe_health;
        let current = node["deployed_revision"].as_i64();
        probe_status =
            if now - at > Duration::seconds(180) || current != Some(revision) || !is_eligible {
                "unknown"
            } else {
                match health.as_deref() {
                    Some("healthy") => "healthy",
                    Some("failed") => "failed",
                    Some("unconfigured") => "unconfigured",
                    _ => "checking",
                }
            };
        if probe_status == "failed" {
            issues.push(issue(
                "node",
                id,
                name,
                "proxy_probe",
                1,
                if external.as_deref() == Some("failed") {
                    "外部目标持续探测失败"
                } else {
                    "公网入口与转发持续探测失败"
                },
                json!(at),
            ));
        } else if matches!(probe_status, "unknown" | "unconfigured") && is_eligible {
            issues.push(issue(
                "node",
                id,
                name,
                "proxy_unknown",
                3,
                "代理探测未配置或已陈旧",
                json!(at),
            ));
        }
    } else if is_eligible {
        issues.push(issue(
            "node",
            id,
            name,
            "proxy_unknown",
            3,
            "尚无代理探测数据",
            Value::Null,
        ));
    }
    if probe_status == "healthy" {
        totals.healthy += 1;
    }
    Ok(
        json!({"node_id":id,"name":name,"deployment_state":status,"online_status":online_status,"online_users":node_users,"connections":node_connections,"online_sampled_at":sampled_at,"probe_status":probe_status,"inlet_status":if probe_status=="unknown"{"unknown"}else{inlet_status},"probe_sampled_at":probe_at,"latency_ms":latency,"connection_ms":connection_ms,"external_status":external,"reason":reason}),
    )
}

pub async fn get(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let summary::SummaryData {
        nodes,
        users,
        node_states: states,
        node_count,
        quota_ids,
        task,
        failures,
        generated_at: now,
    } = summary::load(&state.pool, &state.quota_rank).await?;
    let mut quota_candidates = BTreeMap::new();
    let mut issues = Vec::new();
    let mut totals = MonitoringTotals::default();
    let mut risk_nodes = 0;
    for reading in &nodes {
        let node = reading.summary(now)?;
        let id = node["id"].as_str().unwrap_or_default();
        let name = node["name"].as_str().unwrap_or_default();
        let status = node["state"].as_str().unwrap_or("unknown");
        if quota_ids.iter().any(|ranked| ranked == id) {
            quota_candidates.insert(id.to_owned(), node.clone());
        }
        if matches!(
            status,
            "fingerprint_changed"
                | "sync_failed"
                | "rollback_failed"
                | "drift"
                | "unreachable"
                | "delete_failed"
        ) {
            issues.push(issue(
                "node",
                id,
                name,
                status,
                if status == "fingerprint_changed" {
                    0
                } else {
                    1
                },
                status,
                node["updated_at"].clone(),
            ));
        }
        if node["data_freshness"] == "stale"
            || (!node["deployed_revision"].is_null() && node["data_freshness"] == "not_collected")
            || node["open_gaps"].as_i64().unwrap_or(0) > 0
        {
            issues.push(issue(
                "node",
                id,
                name,
                "sampling",
                3,
                "流量采样陈旧或存在缺口",
                node["last_sample_at"].clone(),
            ));
        }
        let usage = &node["package_usage"];
        if usage["restricted"] == true || usage["alerts"].as_array().is_some_and(|a| !a.is_empty())
        {
            risk_nodes += 1;
            let warnings: Vec<_> = usage["alerts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a["kind"].as_str())
                .map(package_warning)
                .collect();
            let warning_at = usage["alerts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a["created_at"].as_str())
                .min()
                .map(|a| json!(a))
                .unwrap_or_else(|| usage["sampled_at"].clone());
            let warning = if warnings.is_empty() {
                "套餐已受限".into()
            } else {
                warnings.join("、")
            };

            issues.push(issue(
                "node",
                id,
                name,
                "package",
                if usage["restricted"] == true { 1 } else { 2 },
                &warning,
                warning_at,
            ));
        }
        if !node["package"]["quota_bytes"].is_null() && usage["freshness"] != "fresh" {
            issues.push(issue(
                "node",
                id,
                name,
                "network_sampling",
                3,
                "网卡用量未采集或已陈旧",
                usage["sampled_at"].clone(),
            ));
        }
        if usage["failed_disconnects"].as_i64().unwrap_or(0) > 0 {
            issues.push(issue(
                "node",
                id,
                name,
                "disconnect_failed",
                0,
                "断开连接失败",
                usage["sampled_at"].clone(),
            ));
        }
        if node["pending_revocations"].as_i64().unwrap_or(0) > 0
            || usage["pending_disconnects"].as_i64().unwrap_or(0) > 0
        {
            issues.push(issue(
                "node",
                id,
                name,
                "revocation",
                2,
                "撤权或断开连接尚未完成",
                usage["sampled_at"].clone(),
            ));
        }
        monitoring_node(reading, &node, now, &mut totals, &mut issues)?;
    }
    for user in &users {
        if let Some(expiry) = user.expires_at
            && expiry <= now + Duration::days(7)
        {
            issues.push(issue(
                "user",
                &user.id,
                &user.name,
                "expiry",
                if expiry <= now { 1 } else { 2 },
                if expiry <= now {
                    "用户已过期"
                } else {
                    "用户将在 7 天内到期"
                },
                json!(expiry),
            ));
        }
        if user
            .quota_bytes
            .is_some_and(|quota| user.usage_bytes >= quota)
        {
            issues.push(issue(
                "user",
                &user.id,
                &user.name,
                "quota",
                1,
                "用户额度已耗尽",
                json!(user.updated_at),
            ));
        }
    }
    for job in failures {
        let status: String = job.get("status");
        let error: Option<String> = job.get("error_message");
        let error = error.as_deref().unwrap_or("任务失败");
        let reason = match status.as_str() {
            "queued" | "running" => format!("正在重试：{error}"),
            "cancelled" => format!("重试已取消：{error}"),
            "rolled_back" => format!("重试失败，已回滚：{error}"),
            _ => error.to_string(),
        };
        issues.push(issue(
            "job",
            &job.get::<String, _>("id"),
            &job.get::<String, _>("name"),
            "job_failed",
            1,
            &reason,
            json!(job.get::<Option<DateTime<Utc>>, _>("finished_at")),
        ));
    }
    issues.sort_by(|a, b| {
        a["severity"]
            .as_i64()
            .cmp(&b["severity"].as_i64())
            .then_with(|| b["occurred_at"].as_str().cmp(&a["occurred_at"].as_str()))
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let attention = issues
        .iter()
        .filter(|v| v["entity_type"] == "node")
        .filter_map(|v| v["entity_id"].as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let quota_rank: Vec<Value> = quota_ids
        .iter()
        .map(|id| quota_candidates.remove(id).ok_or_else(ApiError::internal))
        .collect::<Result<_, _>>()?;
    Ok(Json(
        json!({"generated_at":now,"node_count":node_count,"node_states":states,"attention_nodes":attention,"risk_nodes":risk_nodes,"queued_jobs":task.get::<i64,_>("queued"),"running_jobs":task.get::<i64,_>("running"),"failed_jobs_24h":task.get::<i64,_>("failed"),"online_users":if totals.covered>0 {Some(totals.active_ids.len())}else{None},"connections":if totals.covered>0 {Some(totals.connection_count)}else{None},"eligible_nodes":totals.eligible,"covered_nodes":totals.covered,"issues":issues,"healthy_nodes":totals.healthy,"quota_rank":quota_rank}),
    ))
}

#[derive(Default, Deserialize)]
pub(crate) struct MonitoringNodesQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    q: Option<String>,
}

pub(crate) async fn list_nodes(
    State(state): State<AppState>,
    Query(query): Query<MonitoringNodesQuery>,
) -> Result<Json<Value>, ApiError> {
    let requested = query.page.unwrap_or(1);
    let size = query.page_size.unwrap_or(20);
    if requested < 1 || !(1..=200).contains(&size) {
        return Err(ApiError::bad_request(
            "page must be positive and page_size must be between 1 and 200",
        ));
    }
    let search = query.q.as_deref().unwrap_or("").trim();
    let pattern = crate::api::literal_search_pattern(search);
    let filter =
        "($1='' OR hysteriax_search_text(id,name,public_host,ssh_host) LIKE lower($1) ESCAPE '\\')";
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL plan_cache_mode=force_custom_plan")
        .execute(&mut *tx)
        .await?;
    let total: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM nodes WHERE {filter}"))
        .bind(&pattern)
        .fetch_one(&mut *tx)
        .await?;
    let page = requested.min(((total + size - 1) / size).max(1));
    let ids:Vec<String>=sqlx::query_scalar(&format!("SELECT id FROM nodes WHERE {filter} ORDER BY lower(name) COLLATE \"C\",name COLLATE \"C\",id LIMIT $2 OFFSET $3"))
        .bind(&pattern).bind(size).bind((page-1)*size).fetch_all(&mut *tx).await?;
    let readings = summary::read_nodes_on(&mut tx, Some(&ids)).await?;
    let now = Utc::now();
    let mut totals = MonitoringTotals::default();
    let mut issues = Vec::new();
    let items = readings
        .iter()
        .map(|reading| {
            monitoring_node(
                reading,
                &reading.summary(now)?,
                now,
                &mut totals,
                &mut issues,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    tx.commit().await?;
    Ok(Json(
        json!({"items":items,"total":total,"page":page,"page_size":size,"generated_at":now}),
    ))
}

/// Persistent hysteresis prevents a long run of mixed results from hiding an outage.
pub fn next_probe_health(
    previous: &str,
    failures: i32,
    successes: i32,
    effective: &str,
) -> (&'static str, i32, i32) {
    match effective {
        "unconfigured" => ("unconfigured", 0, 0),
        "ok" => {
            let successes = (successes + 1).min(2);
            (
                if successes >= 2 {
                    "healthy"
                } else if previous == "failed" {
                    "failed"
                } else {
                    "checking"
                },
                0,
                successes,
            )
        }
        _ => {
            let failures = (failures + 1).min(3);
            (
                if failures >= 3 || previous == "failed" {
                    "failed"
                } else if previous == "healthy" {
                    "healthy"
                } else {
                    "checking"
                },
                failures,
                0,
            )
        }
    }
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    pub range: String,
    pub timezone: String,
    pub node_id: Option<String>,
    pub source: Option<String>,
    pub metrics: Option<String>,
}

pub async fn history(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    let now = Utc::now();
    let as_of = Utc
        .timestamp_opt(now.timestamp().div_euclid(60) * 60, 0)
        .single()
        .ok_or_else(ApiError::internal)?;
    history_at(state, query, as_of).await
}

async fn history_at(
    state: AppState,
    query: HistoryQuery,
    now: DateTime<Utc>,
) -> Result<Json<Value>, ApiError> {
    // PostgreSQL timestamps have microsecond precision; anchors and map keys
    // must match the precision used by date_bin after SQL parameter binding.
    let now = now - Duration::nanoseconds(i64::from(now.timestamp_subsec_nanos() % 1_000));
    let tz: chrono_tz::Tz = query
        .timezone
        .parse()
        .map_err(|_| ApiError::bad_request("timezone must be an IANA timezone"))?;
    let days = match query.range.as_str() {
        "today" | "24h" => 1,
        "7d" => 7,
        "30d" => 30,
        _ => {
            return Err(ApiError::bad_request(
                "range must be 24h, 7d, 30d or today (legacy)",
            ));
        }
    };
    let source = query.source.as_deref().unwrap_or("users");
    if !matches!(source, "users" | "network") {
        return Err(ApiError::bad_request("source must be users or network"));
    }
    let metrics = query.metrics.as_deref().unwrap_or("all");
    if !matches!(metrics, "all" | "traffic" | "quality") {
        return Err(ApiError::bad_request(
            "metrics must be traffic, quality or all",
        ));
    }
    let want_traffic = metrics != "quality";
    let want_quality = metrics != "traffic";
    let mut tx = crate::analytics::snapshot(
        &state.pool,
        now,
        want_quality || (want_traffic && source == "users"),
    )
    .await?;
    // Window length and node scope change selectivity substantially. Generic
    // prepared plans overestimate the edge ranges and spend seconds in JIT.
    sqlx::query("SET LOCAL plan_cache_mode = force_custom_plan")
        .execute(&mut *tx)
        .await?;
    // These bounded history aggregations finish faster than JIT initialization
    // on a fresh connection. Keep this setting local to the read transaction.
    sqlx::query("SET LOCAL jit = off").execute(&mut *tx).await?;
    if let Some(id) = &query.node_id {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM nodes WHERE id=$1)")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            return Err(ApiError::not_found("node"));
        }
    }
    // Keep calendar-today compatibility for older clients. New ranges are fixed
    // elapsed-time windows, independent of midnight and daylight-saving changes.
    let legacy_today = query.range == "today";
    let start = if legacy_today {
        let date = now.with_timezone(&tz).date_naive();
        tz.from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .ok_or_else(|| ApiError::bad_request("local midnight does not exist"))?
            .with_timezone(&Utc)
    } else {
        now - Duration::days(days)
    };
    let unit = if days == 1 { "hour" } else { "rolling_day" };
    let table = if source == "users" {
        "traffic_records"
    } else {
        "node_network_samples"
    };
    let sql = include_str!("analytics/traffic_history.sql").replace("RAW_TABLE", table);
    let traffic = if want_traffic {
        sqlx::query(&sql)
            .bind(unit)
            .bind(source)
            .bind(start)
            .bind(now)
            .bind(&query.node_id)
            .fetch_all(&mut *tx)
            .await?
    } else {
        Vec::new()
    };
    let cycles = if want_quality || (want_traffic && source == "users") {
        sqlx::query(include_str!("analytics/online_history.sql"))
            .bind(unit)
            .bind(&query.timezone)
            .bind(start)
            .bind(now)
            .bind(&query.node_id)
            .fetch_all(&mut *tx)
            .await?
    } else {
        Vec::new()
    };
    let probes = if want_quality {
        sqlx::query(include_str!("analytics/probe_history.sql"))
            .bind(unit)
            .bind(start)
            .bind(now)
            .bind(&query.node_id)
            .fetch_all(&mut *tx)
            .await?
    } else {
        Vec::new()
    };
    let mut buckets = BTreeMap::<DateTime<Utc>, Value>::new();
    let mut cursor = start;
    while cursor < now {
        let end = (cursor
            + if unit == "hour" {
                Duration::hours(1)
            } else {
                Duration::days(1)
            })
        .min(now);
        buckets.insert(cursor,json!({"start":cursor,"end":end,"tx_bytes":null,"rx_bytes":null,"incomplete":true,"traffic_covered_nodes":0,"traffic_expected_nodes":0,"online_users_avg":null,"online_users_peak":null,"connections_avg":null,"connections_peak":null,"covered_nodes":0,"online_incomplete":true,"missing_reason":"无流量样本","probe_attempts":0,"probe_successes":0,"latency_p50_ms":null,"latency_p95_ms":null}));
        cursor = end;
    }
    for row in traffic {
        if let Some(v) = buckets.get_mut(&row.get::<DateTime<Utc>, _>("bucket")) {
            if source == "network" {
                v["traffic_covered_nodes"] = json!(row.get::<i64, _>("covered_nodes"));
            }
            v["tx_bytes"] = json!(row.get::<i64, _>("tx"));
            v["rx_bytes"] = json!(row.get::<i64, _>("rx"));
            v["incomplete"] = json!(row.get::<bool, _>("incomplete"));
            v["missing_reason"] = if row.get::<bool, _>("incomplete") {
                json!("累计首次采样、计数器变更或采样缺口")
            } else {
                Value::Null
            };
        }
    }
    for row in cycles.iter().filter(|_| want_traffic && source == "users") {
        if let Some(v) = buckets.get_mut(&row.get::<DateTime<Utc>, _>("bucket")) {
            let covered: i64 = row.get("covered");
            let expected: i64 = row.get("expected");
            if source == "users" {
                v["traffic_covered_nodes"] = json!(covered);
                v["traffic_expected_nodes"] = json!(expected);
                if v["tx_bytes"].is_null() && covered > 0 {
                    v["tx_bytes"] = json!(0);
                    v["rx_bytes"] = json!(0);
                    v["incomplete"] = json!(false);
                    v["missing_reason"] = Value::Null;
                }
                if row.get::<bool, _>("traffic_incomplete") {
                    v["incomplete"] = json!(true);
                    v["missing_reason"] = json!("部分节点流量采集失败或尚无有效样本");
                }
            }
        }
    }
    for row in cycles.iter().filter(|_| want_quality) {
        if let Some(v) = buckets.get_mut(&row.get::<DateTime<Utc>, _>("bucket")) {
            v["online_users_avg"] = json!(row.get::<Option<f64>, _>("users_avg"));
            v["online_users_peak"] = json!(row.get::<Option<f64>, _>("users_peak"));
            v["connections_avg"] = json!(row.get::<Option<f64>, _>("connections_avg"));
            v["connections_peak"] = json!(row.get::<Option<f64>, _>("connections_peak"));
            v["covered_nodes"] = json!(row.get::<i64, _>("covered_nodes"));
            v["online_incomplete"] = json!(row.get::<bool, _>("incomplete"));
        }
    }
    for row in probes {
        if let Some(v) = buckets.get_mut(&row.get::<DateTime<Utc>, _>("bucket")) {
            v["probe_attempts"] = json!(row.get::<i64, _>("attempts"));
            v["probe_successes"] = json!(row.get::<i64, _>("successes"));
            v["latency_p50_ms"] = json!(row.get::<Option<f64>, _>("p50"));
            v["latency_p95_ms"] = json!(row.get::<Option<f64>, _>("p95"));
        }
    }
    // Gaps can overlap multiple buckets even when no record was written in the gap.
    let gaps = if want_traffic && source == "users" {
        sqlx::query("SELECT opened_at start,COALESCE(resolved_at,now()) finish FROM data_gaps WHERE opened_at <= $2 AND COALESCE(resolved_at,now()) >= $1 AND ($3::text IS NULL OR node_id=$3)").bind(start).bind(now).bind(&query.node_id).fetch_all(&mut *tx).await?
    } else {
        Vec::new()
    };
    for gap in gaps {
        let begin: DateTime<Utc> = gap.get("start");
        let finish: DateTime<Utc> = gap.get("finish");
        for (at, v) in &mut buckets {
            let end = v["end"]
                .as_str()
                .and_then(|s| s.parse::<DateTime<Utc>>().ok())
                .unwrap_or(now);
            if *at <= finish && end > begin {
                v["incomplete"] = json!(true);
                v["missing_reason"] = json!("采集缺口覆盖此时段");
            }
        }
    }
    tx.commit().await?;
    Ok(Json(
        json!({"metrics":metrics,"generated_at":now,"range":query.range,"timezone":query.timezone,"source":source,"buckets":buckets.into_values().collect::<Vec<_>>()}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn online_only_counts_known_users() {
        let ids = BTreeSet::from(["u".into()]);
        assert_eq!(
            real_connections(json!({"u":2,"monitor-node":1,"unknown":5}), &ids).unwrap(),
            BTreeMap::from([("u".into(), 2)])
        );
        assert!(real_connections(json!({"u":-1}), &ids).is_none());
    }
    #[test]
    fn probe_requires_failure_and_recovery_thresholds() {
        let mut state = ("checking", 0, 0);
        for _ in 0..2 {
            state = next_probe_health(state.0, state.1, state.2, "failed");
            assert_ne!(state.0, "failed");
        }
        state = next_probe_health(state.0, state.1, state.2, "failed");
        assert_eq!(state.0, "failed");
        for _ in 0..20 {
            state = next_probe_health(state.0, state.1, state.2, "ok");
            assert_eq!(state.0, "failed");
            state = next_probe_health(state.0, state.1, state.2, "failed");
        }
        state = next_probe_health(state.0, state.1, state.2, "ok");
        state = next_probe_health(state.0, state.1, state.2, "ok");
        assert_eq!(state.0, "healthy");
    }
}

#[cfg(test)]
#[path = "overview_tests.rs"]
mod integration_tests;
