use crate::db;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    api::{enqueue_job_with_payload_in_tx, generate_token, now},
    error::ApiError,
    security::token_digest,
    state::AppState,
};

#[derive(Deserialize)]
pub struct CreateUser {
    name: String,
    #[serde(default = "default_true")]
    enabled: bool,
    expires_at: Option<String>,
    quota_bytes: Option<i64>,
}

#[derive(Deserialize)]
pub struct PatchUser {
    expected_revision: i64,
    name: Option<String>,
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    expires_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    quota_bytes: Option<Option<i64>>,
}

#[derive(Deserialize)]
pub struct RevisionRequest {
    expected_revision: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
// Retain the legacy body shape to return a clear migration error.
#[allow(dead_code)]
pub struct AssignRequest {
    expected_revision: i64,
    node_id: String,
    mtls_credential_id: Option<String>,
    mtls_credential_version: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentCertificateUpdate {
    expected_revision: i64,
    mtls_credential_id: String,
    mtls_credential_version: i64,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let rows = sqlx::query(
        "SELECT * FROM users ORDER BY lower(name) COLLATE \"C\", name COLLATE \"C\", id",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut users = Vec::with_capacity(rows.len());
    for row in rows {
        users.push(user_json(&state, &row).await?);
    }
    Ok(Json(users))
}

#[derive(Default, Deserialize)]
pub(crate) struct UsersPageQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
}

impl UsersPageQuery {
    fn validate(&self) -> Result<(i64, i64, &str, &str), ApiError> {
        let page = self.page.unwrap_or(1);
        let size = self.page_size.unwrap_or(50);
        if page < 1 || !(1..=200).contains(&size) {
            return Err(ApiError::bad_request(
                "page must be positive and page_size must be between 1 and 200",
            ));
        }
        let sort = match self.sort.as_deref().unwrap_or("name") {
            "name" => "lower(u.name) COLLATE \"C\", u.name COLLATE \"C\"",
            "created_at" => "u.created_at",
            _ => return Err(ApiError::bad_request("invalid user sort field")),
        };
        let order = match self.order.as_deref().unwrap_or("asc") {
            "asc" => "ASC",
            "desc" => "DESC",
            _ => return Err(ApiError::bad_request("invalid user sort order")),
        };
        Ok((page, size, sort, order))
    }
}

pub(crate) async fn list_page(
    State(state): State<AppState>,
    Query(query): Query<UsersPageQuery>,
) -> Result<Json<Value>, ApiError> {
    let (requested_page, page_size, sort, order) = query.validate()?;
    let search = query.q.as_deref().unwrap_or("").trim();
    let filter = "($1 = '' OR strpos(lower(u.name), lower($1)) > 0 OR strpos(lower(u.id), lower($1)) > 0 OR EXISTS (SELECT 1 FROM node_assignments na WHERE na.user_id=u.id AND strpos(lower(na.node_id), lower($1)) > 0))";
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let total: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM users u WHERE {filter}"))
        .bind(search)
        .fetch_one(&mut *tx)
        .await?;
    let page = requested_page.min(((total + page_size - 1) / page_size).max(1));
    // Apply the direction to every name component, then use ID to break ties.
    let order_by = if query.sort.as_deref().unwrap_or("name") == "name" {
        format!("lower(u.name) COLLATE \"C\" {order}, u.name COLLATE \"C\" {order}, u.id {order}")
    } else {
        format!("{sort} {order}, u.id {order}")
    };
    let rows = sqlx::query(&format!(
        "SELECT u.* FROM users u WHERE {filter} ORDER BY {order_by} LIMIT $2 OFFSET $3"
    ))
    .bind(search)
    .bind(page_size)
    .bind((page - 1) * page_size)
    .fetch_all(&mut *tx)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(user_json_on(&mut tx, &row).await?);
    }
    tx.commit().await?;
    Ok(Json(
        json!({"items": items, "total": total, "page": page, "page_size": page_size}),
    ))
}

#[cfg(test)]
#[path = "users_pagination_tests.rs"]
mod pagination_tests;

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT * FROM users WHERE id = $1")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    Ok(Json(user_json(&state, &row).await?))
}

pub async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateUser>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    validate_name(&input.name)?;
    validate_quota(input.quota_bytes)?;
    let expires_at = normalize_expiry(input.expires_at)?;
    let id = Uuid::new_v4().to_string();
    let timestamp = now();
    sqlx::query("INSERT INTO users (id, name, enabled, expires_at, quota_bytes, usage_bytes, revision, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 0, 1, $6, $7)")
        .bind(&id).bind(input.name.trim()).bind(input.enabled).bind(expires_at)
        .bind(input.quota_bytes).bind(timestamp).bind(timestamp).execute(&state.pool).await?;
    audit(
        &state,
        "user.created",
        &id,
        json!({"name": input.name.trim()}),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id": id, "name": input.name.trim(), "enabled": input.enabled, "revision": 1})),
    ))
}

pub async fn patch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<PatchUser>,
) -> Result<Json<Value>, ApiError> {
    if input.expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let current = sqlx::query("SELECT * FROM users WHERE id = $1")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    let revision: i64 = current.get("revision");
    if revision != input.expected_revision {
        return Err(ApiError::conflict(format!(
            "user revision is {revision}; reload before editing"
        )));
    }
    let name = input.name.unwrap_or_else(|| current.get("name"));
    let enabled = input
        .enabled
        .unwrap_or_else(|| current.get::<bool, _>("enabled"));
    let expires_at = match input.expires_at {
        Some(Some(value)) => normalize_expiry(Some(value))?,
        Some(None) => None,
        None => current.get("expires_at"),
    };
    let quota_bytes = input
        .quota_bytes
        .unwrap_or_else(|| current.get("quota_bytes"));
    validate_name(&name)?;
    validate_quota(quota_bytes)?;
    validate_expiry(expires_at.as_ref())?;
    let next_revision = revision + 1;
    let timestamp = now();
    let was_enabled = current.get::<bool, _>("enabled");
    let usage_bytes: i64 = current.get("usage_bytes");
    let expired_now = expires_at.is_some_and(|expiration| expiration <= Utc::now());
    let over_quota = quota_bytes.is_some_and(|limit| limit <= usage_bytes);
    let access_restricted = (was_enabled && !enabled) || expired_now || over_quota;
    let previous_marker: Option<DateTime<Utc>> = current.get("access_kick_enqueued_at");
    let kick_marker = if access_restricted {
        Some(timestamp)
    } else if enabled && !expired_now && !over_quota {
        None
    } else {
        previous_marker
    };
    let mut tx = db::begin_write(&state.pool).await?;
    let result = sqlx::query("UPDATE users SET name = $1, enabled = $2, expires_at = $3, quota_bytes = $4, access_kick_enqueued_at = $5, revision = $6, updated_at = $7 WHERE id = $8 AND revision = $9")
        .bind(name.trim()).bind(enabled).bind(expires_at).bind(quota_bytes)
        .bind(kick_marker).bind(next_revision).bind(timestamp).bind(&id).bind(revision).execute(&mut *tx).await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "user changed while saving; reload before editing",
        ));
    }
    if access_restricted {
        let node_ids = sqlx::query_scalar::<_, String>(
            "SELECT node_id FROM node_assignments WHERE user_id = $1",
        )
        .bind(&id)
        .fetch_all(&mut *tx)
        .await?;
        for node_id in node_ids {
            enqueue_job_with_payload_in_tx(
                &mut tx,
                "kick",
                Some(&node_id),
                None,
                json!({"user_id": id,"kick_reason":"access_restricted"}),
            )
            .await?;
        }
    }
    if enabled && !expired_now && !over_quota {
        crate::kick_requests::clear_reason(&mut tx, None, Some(&id), "access_restricted").await?;
    }
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'user.updated', 'user', $2, $3, $4)")
        .bind(Uuid::new_v4().to_string()).bind(&id).bind(json!({"revision": next_revision, "enabled": enabled})).bind(timestamp).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"id": id, "name": name.trim(), "enabled": enabled, "expires_at": expires_at, "quota_bytes": quota_bytes, "revision": next_revision}),
    ))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<super::nodes::RevisionQuery>,
) -> Result<StatusCode, ApiError> {
    let expected = query
        .expected_revision
        .ok_or_else(|| ApiError::bad_request("expected_revision query parameter is required"))?;
    let mut tx = db::begin_write(&state.pool).await?;
    let row = sqlx::query("SELECT revision FROM users WHERE id = $1")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    let revision: i64 = row.get("revision");
    if revision != expected {
        return Err(ApiError::conflict(
            "user revision changed; reload before deleting",
        ));
    }
    sqlx::query("UPDATE authorization_groups SET revision=revision+1,updated_at=$2 WHERE id IN (SELECT group_id FROM authorization_group_users WHERE user_id=$1)")
        .bind(&id)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    let node_ids =
        sqlx::query_scalar::<_, String>("SELECT node_id FROM node_assignments WHERE user_id = $1")
            .bind(&id)
            .fetch_all(&mut *tx)
            .await?;
    for node_id in node_ids {
        enqueue_job_with_payload_in_tx(
            &mut tx,
            "kick",
            Some(&node_id),
            None,
            json!({"user_id": id,"kick_reason":"user_deleted"}),
        )
        .await?;
    }
    sqlx::query("UPDATE credentials SET archived=TRUE,revision=revision+1,updated_at=$1 WHERE owner_user_id=$2 AND archived=FALSE").bind(now()).bind(&id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'user.deleted', 'user', $2, $3, $4)")
        .bind(Uuid::new_v4().to_string()).bind(&id).bind(json!({"revision": expected})).bind(now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn assign(
    State(_state): State<AppState>,
    Path(_user_id): Path<String>,
    Json(_input): Json<AssignRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    Err(legacy_assignment_error())
}

pub async fn update_assignment_client_certificate(
    State(state): State<AppState>,
    Path((user_id, node_id)): Path<(String, String)>,
    Json(input): Json<AssignmentCertificateUpdate>,
) -> Result<Json<Value>, ApiError> {
    if input.expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let mut tx = db::begin_write(&state.pool).await?;
    validate_mtls_binding(
        &mut tx,
        &input.mtls_credential_id,
        input.mtls_credential_version,
        &user_id,
    )
    .await?;
    let user = sqlx::query("SELECT revision FROM users WHERE id = $1")
        .bind(&user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    let revision: i64 = user.get("revision");
    if revision != input.expected_revision {
        return Err(ApiError::conflict(format!(
            "user revision is {revision}; reload before editing"
        )));
    }
    let updated_at = now();
    let update = sqlx::query("UPDATE node_assignments SET mtls_credential_id = $1, mtls_credential_version = $2 WHERE user_id = $3 AND node_id = $4")
        .bind(&input.mtls_credential_id)
        .bind(input.mtls_credential_version)
        .bind(&user_id)
        .bind(&node_id)
        .execute(&mut *tx)
        .await?;
    if update.rows_affected() == 0 {
        return Err(ApiError::not_found("node assignment"));
    }
    let next_revision = revision + 1;
    sqlx::query("UPDATE users SET revision = $1, updated_at = $2 WHERE id = $3 AND revision = $4")
        .bind(next_revision)
        .bind(updated_at)
        .bind(&user_id)
        .bind(revision)
        .execute(&mut *tx)
        .await?;
    enqueue_job_with_payload_in_tx(
        &mut tx,
        "kick",
        Some(&node_id),
        None,
        json!({"user_id": user_id}),
    )
    .await?;
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'user.client_certificate_updated', 'user', $2, $3, $4)")
        .bind(Uuid::new_v4().to_string())
        .bind(&user_id)
        .bind(json!({"node_id": node_id, "revision": next_revision}))
        .bind(updated_at)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(
        json!({"user_id": user_id, "node_id": node_id, "revision": next_revision}),
    ))
}

pub async fn unassign(
    State(_state): State<AppState>,
    Path((_user_id, _node_id)): Path<(String, String)>,
    Json(_input): Json<RevisionRequest>,
) -> Result<Json<Value>, ApiError> {
    Err(legacy_assignment_error())
}

fn legacy_assignment_error() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "authorization_groups_required",
        "Direct user-to-node assignments are disabled; manage access through authorization groups.",
    )
}

pub async fn rotate_credentials(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<RevisionRequest>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let user = sqlx::query("SELECT revision FROM users WHERE id = $1")
        .bind(&user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    let revision: i64 = user.get("revision");
    if revision != input.expected_revision {
        return Err(ApiError::conflict(format!(
            "user revision is {revision}; reload before editing"
        )));
    }
    let rows =
        sqlx::query("SELECT node_id FROM node_assignments WHERE user_id = $1 ORDER BY node_id")
            .bind(&user_id)
            .fetch_all(&mut *tx)
            .await?;
    let timestamp = now();
    let next_revision = revision + 1;
    let mut credentials = Vec::with_capacity(rows.len());
    for row in rows {
        let node_id: String = row.get("node_id");
        let credential = generate_token();
        sqlx::query("UPDATE node_assignments SET credential_hash = $1, credential_enc = $2 WHERE user_id = $3 AND node_id = $4")
            .bind(token_digest(&credential)).bind(state.secrets.encrypt(&credential)?).bind(&user_id).bind(&node_id).execute(&mut *tx).await?;
        enqueue_job_with_payload_in_tx(
            &mut tx,
            "kick",
            Some(&node_id),
            None,
            json!({"user_id": user_id}),
        )
        .await?;
        credentials.push(json!({"node_id": node_id, "credential": credential}));
    }
    sqlx::query("UPDATE users SET revision = $1, updated_at = $2 WHERE id = $3 AND revision = $4")
        .bind(next_revision)
        .bind(timestamp)
        .bind(&user_id)
        .bind(revision)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'user.credentials_rotated', 'user', $2, $3, $4)")
        .bind(Uuid::new_v4().to_string()).bind(&user_id).bind(json!({"revision": next_revision, "nodes": credentials.len()})).bind(timestamp).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"user_id": user_id, "revision": next_revision, "credentials": credentials, "note": "Save these credentials now; this response is the only plaintext copy."}),
    ))
}

pub async fn usage(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = sqlx::query(
        "SELECT id, name, enabled, expires_at, usage_bytes, quota_bytes, quota_reset_at FROM users WHERE id = $1",
    )
    .bind(&user_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("user"))?;
    let rows = sqlx::query(
        "WITH user_nodes AS (
            SELECT node_id FROM node_assignments WHERE user_id = $1
            UNION
            SELECT node_id FROM traffic_records WHERE user_id = $2
            UNION
            SELECT node_id FROM traffic_baselines WHERE user_id = $3
        )
        SELECT n.node_id, COALESCE(SUM(t.delta_tx)::BIGINT, 0::BIGINT) AS tx,
            COALESCE(SUM(t.delta_rx)::BIGINT, 0::BIGINT) AS rx,
            COALESCE(MAX(t.sampled_at), MAX(b.sampled_at)) AS sampled_at,
            EXISTS(SELECT 1 FROM node_assignments a WHERE a.user_id = $4 AND a.node_id = n.node_id) AS assigned
        FROM user_nodes n
        LEFT JOIN traffic_records t ON t.node_id = n.node_id AND t.user_id = $5
        LEFT JOIN traffic_baselines b ON b.node_id = n.node_id AND b.user_id = $6
        GROUP BY n.node_id ORDER BY n.node_id",
    )
    .bind(&user_id)
    .bind(&user_id)
    .bind(&user_id)
    .bind(&user_id)
    .bind(&user_id)
    .bind(&user_id)
    .fetch_all(&state.pool)
    .await?;
    let by_node: Vec<Value> = rows.iter().map(|row| json!({
        "node_id": row.get::<String, _>("node_id"), "tx_bytes": row.get::<Option<i64>, _>("tx").unwrap_or(0),
        "rx_bytes": row.get::<Option<i64>, _>("rx").unwrap_or(0), "sampled_at": row.get::<Option<DateTime<Utc>>, _>("sampled_at"),
        "assigned": row.get::<bool, _>("assigned")
    })).collect();
    let assigned_samples: Vec<Option<DateTime<Utc>>> = rows
        .iter()
        .filter(|row| row.get::<bool, _>("assigned"))
        .map(|row| row.get("sampled_at"))
        .collect();
    let parsed_samples: Vec<DateTime<Utc>> = assigned_samples
        .iter()
        .filter_map(|sample| *sample)
        .collect();
    let freshness = parsed_samples
        .iter()
        .max()
        .map(|sample| sample.to_rfc3339());
    let now_utc = Utc::now();
    let freshness_status = if parsed_samples.is_empty() {
        "not_collected"
    } else if assigned_samples
        .iter()
        .any(|sample| sample.is_none_or(|sample| (now_utc - sample).num_seconds() > 30))
    {
        "stale"
    } else {
        "fresh"
    };
    let gap_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM data_gaps g JOIN node_assignments a ON a.node_id = g.node_id WHERE a.user_id = $1 AND g.resolved_at IS NULL")
        .bind(&user_id).fetch_one(&state.pool).await?;
    let is_restricted = !user.get::<bool, _>("enabled")
        || user
            .get::<Option<DateTime<Utc>>, _>("expires_at")
            .is_some_and(|expiration| expiration <= Utc::now())
        || user
            .get::<Option<i64>, _>("quota_bytes")
            .is_some_and(|quota| user.get::<i64, _>("usage_bytes") >= quota);
    let pending_rows = sqlx::query("SELECT id, node_id, status, stage, updated_at FROM jobs WHERE kind = 'kick' AND node_id IS NOT NULL AND (status IN ('queued', 'running') OR (status = 'failed' AND $1 = TRUE)) AND payload_json->>'user_id' = $2 ORDER BY created_at")
        .bind(is_restricted)
        .bind(&user_id)
        .fetch_all(&state.pool)
        .await?;
    let pending_revocations: Vec<Value> = pending_rows
        .iter()
        .map(|row| {
            json!({
                "job_id": row.get::<String, _>("id"),
                "node_id": row.get::<String, _>("node_id"),
                "status": row.get::<String, _>("status"),
                "stage": row.get::<String, _>("stage"),
                "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            })
        })
        .collect();
    Ok(Json(json!({
        "user_id": user.get::<String, _>("id"), "name": user.get::<String, _>("name"),
        "usage_bytes": user.get::<i64, _>("usage_bytes"), "quota_bytes": user.get::<Option<i64>, _>("quota_bytes"),
        "quota_reset_at": user.get::<Option<chrono::DateTime<chrono::Utc>>, _>("quota_reset_at"), "by_node": by_node,
        "data_freshness": {"status": freshness_status, "last_sample_at": freshness, "open_gaps": gap_count},
        "pending_revocations": pending_revocations
    })))
}

pub async fn reset_quota(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<RevisionRequest>,
) -> Result<Json<Value>, ApiError> {
    let timestamp = now();
    let mut tx = db::begin_write(&state.pool).await?;
    let result = sqlx::query("UPDATE users SET usage_bytes = 0, quota_reset_at = $1, access_kick_enqueued_at = NULL, revision = revision + 1, updated_at = $2 WHERE id = $3 AND revision = $4")
        .bind(timestamp).bind(timestamp).bind(&user_id).bind(input.expected_revision).execute(&mut *tx).await?;
    if result.rows_affected() == 0 {
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = $1")
            .bind(&user_id)
            .fetch_one(&mut *tx)
            .await?;
        return if exists == 0 {
            Err(ApiError::not_found("user"))
        } else {
            Err(ApiError::conflict(
                "user revision changed; reload before resetting quota",
            ))
        };
    }
    let unrestricted: bool = sqlx::query_scalar("SELECT enabled AND (expires_at IS NULL OR expires_at>now()) AND (quota_bytes IS NULL OR usage_bytes<quota_bytes) FROM users WHERE id=$1")
        .bind(&user_id).fetch_one(&mut *tx).await?;
    if unrestricted {
        crate::kick_requests::clear_reason(&mut tx, None, Some(&user_id), "access_restricted")
            .await?;
    }
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'user.quota_reset', 'user', $2, $3, $4)")
        .bind(Uuid::new_v4().to_string()).bind(&user_id).bind(json!({"quota_reset_at": timestamp})).bind(timestamp).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"user_id": user_id, "usage_bytes": 0, "quota_reset_at": timestamp, "revision": input.expected_revision + 1}),
    ))
}

async fn user_json(state: &AppState, row: &sqlx::postgres::PgRow) -> Result<Value, ApiError> {
    let mut connection = state.pool.acquire().await?;
    user_json_on(&mut connection, row).await
}

async fn user_json_on(
    connection: &mut sqlx::PgConnection,
    row: &sqlx::postgres::PgRow,
) -> Result<Value, ApiError> {
    let id: String = row.get("id");
    let assignments = sqlx::query(
        "SELECT node_id, mtls_credential_id, mtls_credential_version, created_at FROM node_assignments WHERE user_id = $1 ORDER BY node_id",
    )
    .bind(&id)
    .fetch_all(&mut *connection)
    .await?;
    let mut assigned = Vec::with_capacity(assignments.len());
    for assignment in assignments {
        let node_id: String = assignment.get("node_id");
        let source_groups = sqlx::query("SELECT g.id,g.name FROM authorization_group_users gu JOIN authorization_group_nodes gn ON gn.group_id=gu.group_id JOIN authorization_groups g ON g.id=gu.group_id WHERE gu.user_id=$1 AND gn.node_id=$2 ORDER BY lower(g.name) COLLATE \"C\",g.name COLLATE \"C\",g.id")
            .bind(&id)
            .bind(&node_id)
            .fetch_all(&mut *connection)
            .await?
            .iter()
            .map(|source| json!({"id":source.get::<String,_>("id"),"name":source.get::<String,_>("name")}))
            .collect::<Vec<_>>();
        assigned.push(json!({
            "node_id":node_id,
            "created_at":assignment.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
            "mtls_credential_id":assignment.get::<Option<String>,_>("mtls_credential_id"),
            "mtls_credential_version":assignment.get::<Option<i64>,_>("mtls_credential_version"),
            "source_groups":source_groups,
        }));
    }
    let authorization_groups = sqlx::query("SELECT g.id,g.name,g.revision FROM authorization_group_users gu JOIN authorization_groups g ON g.id=gu.group_id WHERE gu.user_id=$1 ORDER BY lower(g.name) COLLATE \"C\",g.name COLLATE \"C\",g.id")
        .bind(&id)
        .fetch_all(&mut *connection)
        .await?
        .iter()
        .map(|group| json!({"id":group.get::<String,_>("id"),"name":group.get::<String,_>("name"),"revision":group.get::<i64,_>("revision")}))
        .collect::<Vec<_>>();
    Ok(json!({
        "id": id, "name": row.get::<String, _>("name"), "enabled": row.get::<bool, _>("enabled"),
        "expires_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("expires_at"), "quota_bytes": row.get::<Option<i64>, _>("quota_bytes"),
        "usage_bytes": row.get::<i64, _>("usage_bytes"), "revision": row.get::<i64, _>("revision"),
        "quota_reset_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("quota_reset_at"), "assignments": assigned, "authorization_groups":authorization_groups,
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"), "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
    }))
}

async fn audit(state: &AppState, action: &str, id: &str, detail: Value) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', $2, 'user', $3, $4, $5)")
        .bind(Uuid::new_v4().to_string()).bind(action).bind(id).bind(detail).bind(now()).execute(&state.pool).await?;
    Ok(())
}

fn validate_name(value: &str) -> Result<(), ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 100 || value.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "name must contain 1 to 100 printable characters",
        ));
    }
    Ok(())
}

fn validate_quota(value: Option<i64>) -> Result<(), ApiError> {
    if value.is_some_and(|quota| quota < 0) {
        return Err(ApiError::bad_request("quota_bytes cannot be negative"));
    }
    Ok(())
}

fn validate_expiry(value: Option<&DateTime<Utc>>) -> Result<(), ApiError> {
    let _ = value;
    Ok(())
}

fn normalize_expiry(value: Option<String>) -> Result<Option<DateTime<Utc>>, ApiError> {
    value
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|parsed| parsed.with_timezone(&Utc))
                .map_err(|_| ApiError::bad_request("expires_at must be an RFC 3339 timestamp"))
        })
        .transpose()
}

fn default_true() -> bool {
    true
}

fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

async fn validate_mtls_binding(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
    version: i64,
    user: &str,
) -> Result<(), ApiError> {
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials c JOIN credential_versions v ON v.credential_id=c.id WHERE c.id=$1 AND v.version=$2 AND c.kind='tls_identity' AND c.owner_user_id=$3 AND c.archived=FALSE)").bind(id).bind(version).bind(user).fetch_one(&mut **tx).await?;
    if !valid {
        return Err(ApiError::bad_request(
            "invalid, archived, or incorrectly owned mTLS credential",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::{Json, extract::Path};
    use serde_json::Value;

    use crate::{security::SecretBox, state::AppState};

    use super::{RevisionRequest, reset_quota};

    #[tokio::test]
    async fn quota_reset_starts_a_new_usage_period_and_checks_revision() {
        let pool = crate::db::test_pool().await;
        sqlx::query("INSERT INTO users (id, name, enabled, quota_bytes, usage_bytes, revision, access_kick_enqueued_at, created_at, updated_at) VALUES ('user-id', 'Quota user', TRUE, 1000, 750, 2, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();

        let state = AppState::new(
            pool.clone(),
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
        );
        let Json(response): Json<Value> = reset_quota(
            axum::extract::State(state.clone()),
            Path("user-id".to_owned()),
            Json(RevisionRequest {
                expected_revision: 2,
            }),
        )
        .await
        .unwrap();
        assert_eq!(response["usage_bytes"], 0);
        assert_eq!(response["revision"], 3);

        let (usage, revision, marker): (i64, i64, Option<String>) = sqlx::query_as(
            "SELECT usage_bytes, revision, access_kick_enqueued_at FROM users WHERE id = 'user-id'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((usage, revision, marker), (0, 3, None));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_records WHERE action = 'user.quota_reset'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        assert!(
            reset_quota(
                axum::extract::State(state),
                Path("user-id".to_owned()),
                Json(RevisionRequest {
                    expected_revision: 2,
                }),
            )
            .await
            .is_err()
        );
    }
}
