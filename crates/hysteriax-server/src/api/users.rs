use std::io::Cursor;

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
pub struct AssignRequest {
    expected_revision: i64,
    node_id: String,
    client_certificate: Option<String>,
    client_private_key: Option<String>,
}

#[derive(Deserialize)]
pub struct AssignmentCertificateUpdate {
    expected_revision: i64,
    client_certificate: String,
    client_private_key: String,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let rows = sqlx::query("SELECT * FROM users ORDER BY name COLLATE NOCASE")
        .fetch_all(&state.pool)
        .await?;
    let mut users = Vec::with_capacity(rows.len());
    for row in rows {
        users.push(user_json(&state, &row).await?);
    }
    Ok(Json(users))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT * FROM users WHERE id = ?")
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
    sqlx::query("INSERT INTO users (id, name, enabled, expires_at, quota_bytes, usage_bytes, revision, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 0, 1, ?, ?)")
        .bind(&id).bind(input.name.trim()).bind(i64::from(input.enabled)).bind(&expires_at)
        .bind(input.quota_bytes).bind(&timestamp).bind(&timestamp).execute(&state.pool).await?;
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
    let current = sqlx::query("SELECT * FROM users WHERE id = ?")
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
        .unwrap_or_else(|| current.get::<i64, _>("enabled") != 0);
    let expires_at = input
        .expires_at
        .unwrap_or_else(|| current.get("expires_at"));
    let expires_at = normalize_expiry(expires_at)?;
    let quota_bytes = input
        .quota_bytes
        .unwrap_or_else(|| current.get("quota_bytes"));
    validate_name(&name)?;
    validate_quota(quota_bytes)?;
    validate_expiry(expires_at.as_deref())?;
    let next_revision = revision + 1;
    let timestamp = now();
    let was_enabled = current.get::<i64, _>("enabled") != 0;
    let usage_bytes: i64 = current.get("usage_bytes");
    let expired_now = expires_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .is_some_and(|expiration| expiration <= Utc::now());
    let over_quota = quota_bytes.is_some_and(|limit| limit <= usage_bytes);
    let access_restricted = (was_enabled && !enabled) || expired_now || over_quota;
    let previous_marker: Option<String> = current.get("access_kick_enqueued_at");
    let kick_marker = if access_restricted {
        Some(timestamp.clone())
    } else if enabled && !expired_now && !over_quota {
        None
    } else {
        previous_marker
    };
    let mut tx = state.pool.begin().await?;
    let result = sqlx::query("UPDATE users SET name = ?, enabled = ?, expires_at = ?, quota_bytes = ?, access_kick_enqueued_at = ?, revision = ?, updated_at = ? WHERE id = ? AND revision = ?")
        .bind(name.trim()).bind(i64::from(enabled)).bind(&expires_at).bind(quota_bytes)
        .bind(&kick_marker).bind(next_revision).bind(&timestamp).bind(&id).bind(revision).execute(&mut *tx).await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "user changed while saving; reload before editing",
        ));
    }
    if access_restricted {
        let node_ids = sqlx::query_scalar::<_, String>(
            "SELECT node_id FROM node_assignments WHERE user_id = ?",
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
                json!({"user_id": id}),
            )
            .await?;
        }
    }
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES (?, 'admin', 'user.updated', 'user', ?, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(&id).bind(json!({"revision": next_revision, "enabled": enabled}).to_string()).bind(&timestamp).execute(&mut *tx).await?;
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
    let mut tx = state.pool.begin().await?;
    let row = sqlx::query("SELECT revision FROM users WHERE id = ?")
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
    let node_ids =
        sqlx::query_scalar::<_, String>("SELECT node_id FROM node_assignments WHERE user_id = ?")
            .bind(&id)
            .fetch_all(&mut *tx)
            .await?;
    for node_id in node_ids {
        enqueue_job_with_payload_in_tx(
            &mut tx,
            "kick",
            Some(&node_id),
            None,
            json!({"user_id": id}),
        )
        .await?;
    }
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES (?, 'admin', 'user.deleted', 'user', ?, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(&id).bind(json!({"revision": expected}).to_string()).bind(now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn assign(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<AssignRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = state.pool.begin().await?;
    let user = sqlx::query("SELECT revision FROM users WHERE id = ?")
        .bind(&user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    let revision: i64 = user.get("revision");
    if input.expected_revision != revision {
        return Err(ApiError::conflict(format!(
            "user revision is {revision}; reload before editing"
        )));
    }
    let node_config_enc: Option<String> =
        sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id = ?")
            .bind(&input.node_id)
            .fetch_optional(&mut *tx)
            .await?;
    let node_config_enc = node_config_enc.ok_or_else(|| ApiError::not_found("node"))?;
    let node_config_json = state.secrets.decrypt(&node_config_enc)?;
    let node_config: Value =
        serde_json::from_str(&node_config_json).map_err(|_| ApiError::internal())?;
    let mtls_required = node_config
        .get("tls")
        .and_then(Value::as_object)
        .and_then(|tls| tls.get("clientCA"))
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    let client_certificate = input.client_certificate.as_deref();
    let client_private_key = input.client_private_key.as_deref();
    match (client_certificate, client_private_key) {
        (Some(certificate), Some(private_key)) => {
            validate_client_certificate_pair(certificate, private_key)?;
        }
        (None, None) if !mtls_required => {}
        (None, None) => {
            return Err(ApiError::bad_request(
                "this node requires a matching client certificate and private key",
            ));
        }
        _ => {
            return Err(ApiError::bad_request(
                "client certificate and private key must be provided together",
            ));
        }
    }
    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM node_assignments WHERE user_id = ? AND node_id = ?",
    )
    .bind(&user_id)
    .bind(&input.node_id)
    .fetch_one(&mut *tx)
    .await?;
    if exists > 0 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_assigned",
            "user is already assigned to this node",
        ));
    }
    let credential = generate_token();
    let credential_hash = token_digest(&credential);
    let credential_enc = state.secrets.encrypt(&credential)?;
    let client_certificate_enc = client_certificate
        .map(|certificate| state.secrets.encrypt(certificate))
        .transpose()?;
    let client_private_key_enc = client_private_key
        .map(|private_key| state.secrets.encrypt(private_key))
        .transpose()?;
    let timestamp = now();
    sqlx::query("INSERT INTO node_assignments (user_id, node_id, credential_hash, credential_enc, client_certificate_enc, client_private_key_enc, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(&user_id).bind(&input.node_id).bind(credential_hash).bind(credential_enc)
        .bind(client_certificate_enc).bind(client_private_key_enc).bind(&timestamp).execute(&mut *tx).await?;
    let next_revision = revision + 1;
    sqlx::query("UPDATE users SET revision = ?, updated_at = ? WHERE id = ? AND revision = ?")
        .bind(next_revision)
        .bind(&timestamp)
        .bind(&user_id)
        .bind(revision)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES (?, 'admin', 'user.assigned', 'user', ?, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(&user_id).bind(json!({"node_id": input.node_id, "revision": next_revision}).to_string()).bind(&timestamp).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"user_id": user_id, "node_id": input.node_id, "revision": next_revision, "hy2_credential": credential, "note": "Credential is shown once here; rotate it to replace it later."}),
        ),
    ))
}

pub async fn update_assignment_client_certificate(
    State(state): State<AppState>,
    Path((user_id, node_id)): Path<(String, String)>,
    Json(input): Json<AssignmentCertificateUpdate>,
) -> Result<Json<Value>, ApiError> {
    if input.expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    validate_client_certificate_pair(&input.client_certificate, &input.client_private_key)?;
    let mut tx = state.pool.begin().await?;
    let user = sqlx::query("SELECT revision FROM users WHERE id = ?")
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
    let update = sqlx::query("UPDATE node_assignments SET client_certificate_enc = ?, client_private_key_enc = ? WHERE user_id = ? AND node_id = ?")
        .bind(state.secrets.encrypt(&input.client_certificate)?)
        .bind(state.secrets.encrypt(&input.client_private_key)?)
        .bind(&user_id)
        .bind(&node_id)
        .execute(&mut *tx)
        .await?;
    if update.rows_affected() == 0 {
        return Err(ApiError::not_found("node assignment"));
    }
    let next_revision = revision + 1;
    sqlx::query("UPDATE users SET revision = ?, updated_at = ? WHERE id = ? AND revision = ?")
        .bind(next_revision)
        .bind(&updated_at)
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
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES (?, 'admin', 'user.client_certificate_updated', 'user', ?, ?, ?)")
        .bind(Uuid::new_v4().to_string())
        .bind(&user_id)
        .bind(json!({"node_id": node_id, "revision": next_revision}).to_string())
        .bind(&updated_at)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(
        json!({"user_id": user_id, "node_id": node_id, "revision": next_revision}),
    ))
}

pub async fn unassign(
    State(state): State<AppState>,
    Path((user_id, node_id)): Path<(String, String)>,
    Json(input): Json<RevisionRequest>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let user = sqlx::query("SELECT revision FROM users WHERE id = ?")
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
    let deleted = sqlx::query("DELETE FROM node_assignments WHERE user_id = ? AND node_id = ?")
        .bind(&user_id)
        .bind(&node_id)
        .execute(&mut *tx)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::not_found("node assignment"));
    }
    let next_revision = revision + 1;
    sqlx::query("UPDATE users SET revision = ?, updated_at = ? WHERE id = ? AND revision = ?")
        .bind(next_revision)
        .bind(now())
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
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES (?, 'admin', 'user.unassigned', 'user', ?, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(&user_id).bind(json!({"node_id": node_id, "revision": next_revision}).to_string()).bind(now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"user_id": user_id, "node_id": node_id, "revision": next_revision, "kick_queued": true}),
    ))
}

pub async fn rotate_credentials(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<RevisionRequest>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let user = sqlx::query("SELECT revision FROM users WHERE id = ?")
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
        sqlx::query("SELECT node_id FROM node_assignments WHERE user_id = ? ORDER BY node_id")
            .bind(&user_id)
            .fetch_all(&mut *tx)
            .await?;
    let timestamp = now();
    let next_revision = revision + 1;
    let mut credentials = Vec::with_capacity(rows.len());
    for row in rows {
        let node_id: String = row.get("node_id");
        let credential = generate_token();
        sqlx::query("UPDATE node_assignments SET credential_hash = ?, credential_enc = ? WHERE user_id = ? AND node_id = ?")
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
    sqlx::query("UPDATE users SET revision = ?, updated_at = ? WHERE id = ? AND revision = ?")
        .bind(next_revision)
        .bind(&timestamp)
        .bind(&user_id)
        .bind(revision)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES (?, 'admin', 'user.credentials_rotated', 'user', ?, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(&user_id).bind(json!({"revision": next_revision, "nodes": credentials.len()}).to_string()).bind(&timestamp).execute(&mut *tx).await?;
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
        "SELECT id, name, enabled, expires_at, usage_bytes, quota_bytes, quota_reset_at FROM users WHERE id = ?",
    )
    .bind(&user_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("user"))?;
    let rows = sqlx::query(
        "WITH user_nodes AS (
            SELECT node_id FROM node_assignments WHERE user_id = ?
            UNION
            SELECT node_id FROM traffic_records WHERE user_id = ?
            UNION
            SELECT node_id FROM traffic_baselines WHERE user_id = ?
        )
        SELECT n.node_id, COALESCE(SUM(t.delta_tx), 0) AS tx, COALESCE(SUM(t.delta_rx), 0) AS rx,
            COALESCE(MAX(t.sampled_at), b.sampled_at) AS sampled_at,
            EXISTS(SELECT 1 FROM node_assignments a WHERE a.user_id = ? AND a.node_id = n.node_id) AS assigned
        FROM user_nodes n
        LEFT JOIN traffic_records t ON t.node_id = n.node_id AND t.user_id = ?
        LEFT JOIN traffic_baselines b ON b.node_id = n.node_id AND b.user_id = ?
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
        "rx_bytes": row.get::<Option<i64>, _>("rx").unwrap_or(0), "sampled_at": row.get::<Option<String>, _>("sampled_at"),
        "assigned": row.get::<i64, _>("assigned") != 0
    })).collect();
    let assigned_samples: Vec<Option<String>> = rows
        .iter()
        .filter(|row| row.get::<i64, _>("assigned") != 0)
        .map(|row| row.get("sampled_at"))
        .collect();
    let parsed_samples: Vec<DateTime<Utc>> = assigned_samples
        .iter()
        .filter_map(|sample| {
            sample
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|sample| sample.with_timezone(&Utc))
        })
        .collect();
    let freshness = parsed_samples
        .iter()
        .max()
        .map(|sample| sample.to_rfc3339());
    let now_utc = Utc::now();
    let freshness_status = if parsed_samples.is_empty() {
        "not_collected"
    } else if assigned_samples.iter().any(|sample| {
        sample
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_none_or(|sample| (now_utc - sample.with_timezone(&Utc)).num_seconds() > 30)
    }) {
        "stale"
    } else {
        "fresh"
    };
    let gap_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM data_gaps g JOIN node_assignments a ON a.node_id = g.node_id WHERE a.user_id = ? AND g.resolved_at IS NULL")
        .bind(&user_id).fetch_one(&state.pool).await?;
    let is_restricted = user.get::<i64, _>("enabled") == 0
        || user
            .get::<Option<String>, _>("expires_at")
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|expiration| expiration <= Utc::now())
        || user
            .get::<Option<i64>, _>("quota_bytes")
            .is_some_and(|quota| user.get::<i64, _>("usage_bytes") >= quota);
    let pending_rows = sqlx::query("SELECT id, node_id, status, stage, updated_at FROM jobs WHERE kind = 'kick' AND node_id IS NOT NULL AND (status IN ('queued', 'running') OR (status = 'failed' AND ? = 1)) AND payload_json = ? ORDER BY created_at")
        .bind(i64::from(is_restricted))
        .bind(json!({"user_id": user_id}).to_string())
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
                "updated_at": row.get::<String, _>("updated_at")
            })
        })
        .collect();
    Ok(Json(json!({
        "user_id": user.get::<String, _>("id"), "name": user.get::<String, _>("name"),
        "usage_bytes": user.get::<i64, _>("usage_bytes"), "quota_bytes": user.get::<Option<i64>, _>("quota_bytes"),
        "quota_reset_at": user.get::<Option<String>, _>("quota_reset_at"), "by_node": by_node,
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
    let mut tx = state.pool.begin().await?;
    let result = sqlx::query("UPDATE users SET usage_bytes = 0, quota_reset_at = ?, access_kick_enqueued_at = NULL, revision = revision + 1, updated_at = ? WHERE id = ? AND revision = ?")
        .bind(&timestamp).bind(&timestamp).bind(&user_id).bind(input.expected_revision).execute(&mut *tx).await?;
    if result.rows_affected() == 0 {
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = ?")
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
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES (?, 'admin', 'user.quota_reset', 'user', ?, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(&user_id).bind(json!({"quota_reset_at": timestamp}).to_string()).bind(&timestamp).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"user_id": user_id, "usage_bytes": 0, "quota_reset_at": timestamp, "revision": input.expected_revision + 1}),
    ))
}

async fn user_json(state: &AppState, row: &sqlx::sqlite::SqliteRow) -> Result<Value, ApiError> {
    let id: String = row.get("id");
    let assignments = sqlx::query(
        "SELECT node_id, created_at FROM node_assignments WHERE user_id = ? ORDER BY node_id",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await?;
    let assigned: Vec<Value> = assignments.iter().map(|assignment| json!({"node_id": assignment.get::<String, _>("node_id"), "created_at": assignment.get::<String, _>("created_at")})).collect();
    Ok(json!({
        "id": id, "name": row.get::<String, _>("name"), "enabled": row.get::<i64, _>("enabled") != 0,
        "expires_at": row.get::<Option<String>, _>("expires_at"), "quota_bytes": row.get::<Option<i64>, _>("quota_bytes"),
        "usage_bytes": row.get::<i64, _>("usage_bytes"), "revision": row.get::<i64, _>("revision"),
        "quota_reset_at": row.get::<Option<String>, _>("quota_reset_at"), "assignments": assigned,
        "created_at": row.get::<String, _>("created_at"), "updated_at": row.get::<String, _>("updated_at")
    }))
}

async fn audit(state: &AppState, action: &str, id: &str, detail: Value) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES (?, 'admin', ?, 'user', ?, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(action).bind(id).bind(detail.to_string()).bind(now()).execute(&state.pool).await?;
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

fn validate_client_certificate_pair(certificate: &str, private_key: &str) -> Result<(), ApiError> {
    if certificate.len() > 1_048_576 || private_key.len() > 1_048_576 {
        return Err(ApiError::bad_request(
            "client certificate and private key must each be at most 1 MiB",
        ));
    }
    let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ApiError::bad_request("client certificate is not valid PEM"))?;
    if certificates.is_empty() {
        return Err(ApiError::bad_request(
            "client certificate must contain at least one PEM certificate",
        ));
    }
    let private_key = rustls_pemfile::private_key(&mut Cursor::new(private_key.as_bytes()))
        .map_err(|_| ApiError::bad_request("client private key is not valid PEM"))?
        .ok_or_else(|| ApiError::bad_request("client private key PEM block was not found"))?;
    rustls::sign::CertifiedKey::from_der(
        certificates,
        private_key,
        &rustls::crypto::ring::default_provider(),
    )
    .map_err(|_| ApiError::bad_request("client certificate and private key do not match"))?;
    Ok(())
}

fn validate_quota(value: Option<i64>) -> Result<(), ApiError> {
    if value.is_some_and(|quota| quota < 0) {
        return Err(ApiError::bad_request("quota_bytes cannot be negative"));
    }
    Ok(())
}

fn validate_expiry(value: Option<&str>) -> Result<(), ApiError> {
    if let Some(value) = value {
        DateTime::parse_from_rfc3339(value).map_err(|_| {
            ApiError::bad_request("expires_at must be an RFC 3339 timestamp in UTC")
        })?;
    }
    Ok(())
}

fn normalize_expiry(value: Option<String>) -> Result<Option<String>, ApiError> {
    value
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|parsed| parsed.with_timezone(&Utc).to_rfc3339())
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

#[cfg(test)]
mod tests {
    use axum::{Json, extract::Path};
    use serde_json::Value;
    use sqlx::sqlite::SqlitePoolOptions;

    use crate::{security::SecretBox, state::AppState};

    use super::{RevisionRequest, reset_quota};

    #[tokio::test]
    async fn quota_reset_starts_a_new_usage_period_and_checks_revision() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO users (id, name, enabled, quota_bytes, usage_bytes, revision, access_kick_enqueued_at, created_at, updated_at) VALUES ('user-id', 'Quota user', 1, 1000, 750, 2, 'pending-kick', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
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
