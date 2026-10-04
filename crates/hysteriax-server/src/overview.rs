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

pub async fn get(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let Json(nodes) = crate::api::nodes::list(State(state.clone())).await?;
    let Json(users) = crate::api::users::list(State(state.clone())).await?;
    let mut issues = Vec::new();
    let mut monitor_nodes = Vec::new();
    let mut risk_nodes = Vec::new();
    let mut active_ids = BTreeSet::new();
    let mut connection_count = 0_i64;
    let mut covered = 0;
    let mut eligible = 0;
    let mut states = BTreeMap::<String, usize>::new();
    for node in &nodes {
        let id = node["id"].as_str().unwrap_or_default();
        let name = node["name"].as_str().unwrap_or_default();
        let status = node["state"].as_str().unwrap_or("unknown");
        *states.entry(status.into()).or_default() += 1;
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
            risk_nodes.push(node.clone());
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
        let online = sqlx::query("SELECT sampled_at,status,connections FROM online_samples WHERE node_id=$1 ORDER BY sampled_at DESC LIMIT 1").bind(id).fetch_optional(&state.pool).await?;
        let mut online_status = "unknown";
        let mut sampled_at: Option<DateTime<Utc>> = None;
        let mut node_users: Option<usize> = None;
        let mut node_connections: Option<i64> = None;
        let is_eligible =
            !node["deployed_revision"].is_null() && !matches!(status, "deleting" | "delete_failed");
        if is_eligible {
            eligible += 1;
        }
        if let Some(row) = online {
            let at: DateTime<Utc> = row.get("sampled_at");
            sampled_at = Some(at);
            online_status = if Utc::now() - at > Duration::seconds(90) {
                "stale"
            } else if row.get::<String, _>("status") == "ok" {
                "fresh"
            } else {
                "failed"
            };
            if online_status == "fresh" && is_eligible {
                covered += 1;
                let values: Value = row.get("connections");
                let map = values.as_object().cloned().unwrap_or_default();
                node_users = Some(map.len());
                let total = map.values().filter_map(Value::as_i64).sum::<i64>();
                node_connections = Some(total);
                connection_count += total;
                active_ids.extend(map.keys().cloned());
            }
        }
        let probes = sqlx::query("SELECT revision,sampled_at,status,external_status,connection_ms,latency_ms,reason FROM proxy_probe_samples WHERE node_id=$1 ORDER BY sampled_at DESC LIMIT 8").bind(id).fetch_all(&state.pool).await?;
        let mut probe_status = "unknown";
        let mut inlet_status = "unknown";
        let mut probe_at: Option<DateTime<Utc>> = None;
        let mut latency: Option<f64> = None;
        let mut connection_ms: Option<f64> = None;
        let mut external: Option<String> = None;
        let mut reason: Option<String> = None;
        if let Some(last) = probes.first() {
            let at: DateTime<Utc> = last.get("sampled_at");
            probe_at = Some(at);
            latency = last.get("latency_ms");
            connection_ms = last.get("connection_ms");
            external = last.get("external_status");
            reason = last.get("reason");
            let revision: i64 = last.get("revision");
            inlet_status = if last.get::<String, _>("status") == "ok" {
                "ok"
            } else if last.get::<String, _>("status") == "unconfigured" {
                "unconfigured"
            } else {
                "failed"
            };
            let health: Option<String> = sqlx::query_scalar(
                "SELECT health FROM monitoring_probe_state WHERE node_id=$1 AND revision=$2",
            )
            .bind(id)
            .bind(revision)
            .fetch_optional(&state.pool)
            .await?;
            let current = node["deployed_revision"].as_i64();
            probe_status = if Utc::now() - at > Duration::seconds(180)
                || current != Some(revision)
                || !is_eligible
            {
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
        monitor_nodes.push(json!({"node_id":id,"name":name,"deployment_state":status,"online_status":online_status,"online_users":node_users,"connections":node_connections,"online_sampled_at":sampled_at,"probe_status":probe_status,"inlet_status":if probe_status=="unknown"{"unknown"}else{inlet_status},"probe_sampled_at":probe_at,"latency_ms":latency,"connection_ms":connection_ms,"external_status":external,"reason":reason}));
    }
    for user in &users {
        let id = user["id"].as_str().unwrap_or_default();
        let name = user["name"].as_str().unwrap_or_default();
        if let Some(expiry) = user["expires_at"]
            .as_str()
            .and_then(|v| v.parse::<DateTime<Utc>>().ok())
            && user["enabled"] == true
            && expiry <= Utc::now() + Duration::days(7)
        {
            issues.push(issue(
                "user",
                id,
                name,
                "expiry",
                if expiry <= Utc::now() { 1 } else { 2 },
                if expiry <= Utc::now() {
                    "用户已过期"
                } else {
                    "用户将在 7 天内到期"
                },
                json!(expiry),
            ));
        }
        if user["enabled"] == true
            && user["quota_bytes"]
                .as_i64()
                .is_some_and(|q| user["usage_bytes"].as_i64().unwrap_or(0) >= q)
        {
            issues.push(issue(
                "user",
                id,
                name,
                "quota",
                1,
                "用户额度已耗尽",
                user["updated_at"].clone(),
            ));
        }
    }
    let task = sqlx::query("SELECT count(*) FILTER(WHERE status='queued')::bigint queued,count(*) FILTER(WHERE status='running')::bigint running,count(*) FILTER(WHERE status='failed' AND finished_at>=now()-interval '24 hours')::bigint failed FROM jobs").fetch_one(&state.pool).await?;
    // Follow only explicit retry links. Successful leaves resolve the reminder;
    // failed leaves replace ancestors, while active/cancelled attempts keep it visible.
    let failures = sqlx::query(r#"
        WITH RECURSIVE attempts AS (
            SELECT id,kind,node_name,status,error_message,finished_at,retry_of_job_id,
                   error_message last_error,finished_at last_failure_at
            FROM jobs WHERE status='failed' AND finished_at>=now()-interval '24 hours'
            UNION ALL
            SELECT child.id,child.kind,child.node_name,child.status,child.error_message,child.finished_at,child.retry_of_job_id,
                   CASE WHEN child.status IN ('failed','rolled_back') THEN child.error_message ELSE a.last_error END,
                   CASE WHEN child.status IN ('failed','rolled_back') THEN child.finished_at ELSE a.last_failure_at END
            FROM jobs child JOIN attempts a ON child.retry_of_job_id=a.id
        )
        SELECT DISTINCT ON(a.id) a.id,COALESCE(a.node_name,a.kind) name,a.status,a.last_error error_message,a.last_failure_at finished_at
        FROM attempts a WHERE a.status!='succeeded' AND NOT EXISTS(SELECT 1 FROM jobs child WHERE child.retry_of_job_id=a.id)
        ORDER BY a.id,a.last_failure_at DESC
    "#).fetch_all(&state.pool).await?;
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
    let quota_rank: Vec<_> = nodes
        .iter()
        .filter(|n| n["package"]["quota_bytes"].as_i64().is_some_and(|q| q > 0))
        .cloned()
        .collect();
    let mut quota_rank = quota_rank;
    quota_rank.sort_by(|a, b| {
        let ratio = |v: &Value| {
            let quota = v["package"]["quota_bytes"].as_f64().unwrap_or(1.0);
            if quota == 0.0 {
                f64::INFINITY
            } else {
                v["package_usage"]["usage_bytes"]
                    .as_f64()
                    .map(|used| used / quota)
                    .unwrap_or(-1.0)
            }
        };
        ratio(b).total_cmp(&ratio(a))
    });
    quota_rank.truncate(5);
    Ok(Json(
        json!({"generated_at":Utc::now(),"node_count":nodes.len(),"node_states":states,"attention_nodes":attention,"risk_nodes":risk_nodes.len(),"queued_jobs":task.get::<i64,_>("queued"),"running_jobs":task.get::<i64,_>("running"),"failed_jobs_24h":task.get::<i64,_>("failed"),"online_users":if covered>0 {Some(active_ids.len())}else{None},"connections":if covered>0 {Some(connection_count)}else{None},"eligible_nodes":eligible,"covered_nodes":covered,"issues":issues,"nodes":monitor_nodes,"quota_rank":quota_rank}),
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
}

pub async fn history(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    history_at(state, query, Utc::now()).await
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
    if let Some(id) = &query.node_id {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM nodes WHERE id=$1)")
            .bind(id)
            .fetch_one(&state.pool)
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
    let sql = format!(
        "SELECT CASE WHEN $1='hour' THEN date_bin(interval '1 hour',sampled_at,$3) WHEN $1='rolling_day' THEN date_bin(interval '24 hours',sampled_at,$3) ELSE date_trunc($1, sampled_at AT TIME ZONE $2) AT TIME ZONE $2 END bucket,sum(delta_tx)::bigint tx,sum(delta_rx)::bigint rx,count(DISTINCT node_id)::bigint covered_nodes,bool_or(gap_reason IS NOT NULL OR baseline_only) incomplete FROM {table} WHERE sampled_at >= $3 AND sampled_at < $4 AND ($5::text IS NULL OR node_id=$5) GROUP BY bucket ORDER BY bucket"
    );
    let traffic = sqlx::query(&sql)
        .bind(unit)
        .bind(&query.timezone)
        .bind(start)
        .bind(now)
        .bind(&query.node_id)
        .fetch_all(&state.pool)
        .await?;
    let online=sqlx::query("WITH cycles AS (SELECT cycle_id,CASE WHEN $1='hour' THEN date_bin(interval '1 hour',cycle_started_at,$3) WHEN $1='rolling_day' THEN date_bin(interval '24 hours',cycle_started_at,$3) ELSE date_trunc($1,cycle_started_at AT TIME ZONE $2) AT TIME ZONE $2 END bucket,count(*) FILTER(WHERE status='ok') covered,bool_or(status!='ok') OR count(*) FILTER(WHERE status='ok')<CASE WHEN $5::text IS NULL THEN max(expected_nodes) ELSE 1 END incomplete FROM online_samples WHERE cycle_started_at >= $3 AND cycle_started_at < $4 AND ($5::text IS NULL OR node_id=$5) GROUP BY cycle_id,bucket), counts AS (SELECT s.cycle_id,count(DISTINCT e.key)::double precision users,COALESCE(sum(e.value::bigint),0)::double precision connections FROM online_samples s LEFT JOIN LATERAL jsonb_each_text(s.connections) e ON true WHERE s.cycle_started_at >= $3 AND s.cycle_started_at < $4 AND s.status='ok' AND ($5::text IS NULL OR s.node_id=$5) GROUP BY s.cycle_id) SELECT c.bucket,avg(n.users) users_avg,max(n.users) users_peak,avg(n.connections) connections_avg,max(n.connections) connections_peak,min(c.covered)::bigint covered_nodes,bool_or(c.incomplete) incomplete FROM cycles c LEFT JOIN counts n USING(cycle_id) GROUP BY c.bucket ORDER BY c.bucket")
        .bind(unit).bind(&query.timezone).bind(start).bind(now).bind(&query.node_id).fetch_all(&state.pool).await?;
    let probes=sqlx::query("SELECT CASE WHEN $1='hour' THEN date_bin(interval '1 hour',sampled_at,$3) WHEN $1='rolling_day' THEN date_bin(interval '24 hours',sampled_at,$3) ELSE date_trunc($1,sampled_at AT TIME ZONE $2) AT TIME ZONE $2 END bucket,count(*)::bigint attempts,count(*) FILTER(WHERE status='ok' AND external_status IS DISTINCT FROM 'failed')::bigint successes,percentile_cont(0.5) WITHIN GROUP(ORDER BY latency_ms) FILTER(WHERE status='ok' AND external_status IS DISTINCT FROM 'failed') p50,percentile_cont(0.95) WITHIN GROUP(ORDER BY latency_ms) FILTER(WHERE status='ok' AND external_status IS DISTINCT FROM 'failed') p95 FROM proxy_probe_samples WHERE sampled_at >= $3 AND sampled_at < $4 AND status IN ('ok','failed') AND ($5::text IS NULL OR node_id=$5) GROUP BY bucket ORDER BY bucket")
        .bind(unit).bind(&query.timezone).bind(start).bind(now).bind(&query.node_id).fetch_all(&state.pool).await?;
    let quality=sqlx::query("WITH cycles AS (SELECT cycle_id,CASE WHEN $1='hour' THEN date_bin(interval '1 hour',cycle_started_at,$3) WHEN $1='rolling_day' THEN date_bin(interval '24 hours',cycle_started_at,$3) ELSE date_trunc($1,cycle_started_at AT TIME ZONE $2) AT TIME ZONE $2 END bucket,count(*) FILTER(WHERE traffic_status='ok')::bigint covered,CASE WHEN $5::text IS NULL THEN max(expected_nodes) ELSE 1 END expected FROM online_samples WHERE cycle_started_at >= $3 AND cycle_started_at < $4 AND ($5::text IS NULL OR node_id=$5) GROUP BY cycle_id,bucket) SELECT bucket,min(covered)::bigint covered,max(expected)::bigint expected,bool_or(covered<expected) incomplete FROM cycles GROUP BY bucket")
        .bind(unit).bind(&query.timezone).bind(start).bind(now).bind(&query.node_id).fetch_all(&state.pool).await?;
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
    for row in quality {
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
                if row.get::<bool, _>("incomplete") {
                    v["incomplete"] = json!(true);
                    v["missing_reason"] = json!("部分节点流量采集失败或尚无有效样本");
                }
            }
        }
    }
    for row in online {
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
    let gaps = if source == "users" {
        sqlx::query("SELECT opened_at start,COALESCE(resolved_at,now()) finish FROM data_gaps WHERE opened_at <= $2 AND COALESCE(resolved_at,now()) >= $1 AND ($3::text IS NULL OR node_id=$3)").bind(start).bind(now).bind(&query.node_id).fetch_all(&state.pool).await?
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
    Ok(Json(
        json!({"generated_at":now,"range":query.range,"timezone":query.timezone,"source":source,"buckets":buckets.into_values().collect::<Vec<_>>()}),
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
