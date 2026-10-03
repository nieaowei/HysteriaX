use crate::db;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use url::Url;
use uuid::Uuid;

use crate::{
    api::{
        cancel_queued_node_jobs_in_tx, enqueue_job_in_tx, enqueue_job_with_payload_in_tx,
        generate_token, now, supersede_queued_syncs_in_tx,
    },
    config::{
        DEFAULT_TRAFFIC_STATS_PORT, render_server_yaml_preview_with_traffic_stats_port,
        validate_server_options,
    },
    error::ApiError,
    security::token_digest,
    state::AppState,
};

#[derive(Deserialize)]
pub struct CreateNode {
    package: Option<crate::node_limits::Package>,
    initial_usage_bytes: Option<i64>,
    name: String,
    ssh_host: String,
    ssh_port: u16,
    ssh_username: String,
    ssh_auth_type: String,
    ssh_secret: String,
    ssh_passphrase: Option<String>,
    ssh_host_fingerprint: Option<String>,
    public_host: String,
    public_port: u16,
    listen_addr: String,
    #[serde(default = "default_traffic_stats_port")]
    traffic_stats_port: u16,
    proxy_probe_url: Option<String>,
    tls_sni: Option<String>,
    #[serde(default)]
    tls_skip_verify: bool,
    #[serde(default = "empty_config")]
    config: Value,
}

#[derive(Deserialize)]
pub struct PatchNode {
    package: Option<crate::node_limits::Package>,
    expected_revision: i64,
    name: Option<String>,
    ssh_host: Option<String>,
    ssh_port: Option<u16>,
    ssh_username: Option<String>,
    ssh_auth_type: Option<String>,
    ssh_secret: Option<String>,
    ssh_passphrase: Option<String>,
    ssh_host_fingerprint: Option<String>,
    public_host: Option<String>,
    public_port: Option<u16>,
    listen_addr: Option<String>,
    traffic_stats_port: Option<u16>,
    proxy_probe_url: Option<String>,
    tls_sni: Option<String>,
    tls_skip_verify: Option<bool>,
    config: Option<Value>,
}

#[derive(Deserialize)]
pub struct RevisionQuery {
    pub expected_revision: Option<i64>,
}

#[derive(Deserialize)]
pub struct NodeAction {
    expected_revision: i64,
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let rows = sqlx::query(
        "SELECT * FROM nodes ORDER BY lower(name) COLLATE \"C\", name COLLATE \"C\", id",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut values = Vec::with_capacity(rows.len());
    for row in rows {
        values.push(node_json(&state, &row).await?);
    }
    Ok(Json(values))
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT * FROM nodes WHERE id = $1")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("node"))?;
    Ok(Json(node_json(&state, &row).await?))
}

pub async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateNode>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if input.initial_usage_bytes.is_some_and(|x| x < 0) {
        return Err(ApiError::bad_request("initial usage must be nonnegative"));
    }
    if let Some(package) = &input.package {
        package.validate()?;
    }
    validate_text(&input.name, "name", 100)?;
    validate_text(&input.ssh_host, "ssh_host", 253)?;
    validate_text(&input.ssh_username, "ssh_username", 100)?;
    validate_text(&input.public_host, "public_host", 253)?;
    validate_text(&input.listen_addr, "listen_addr", 100)?;
    validate_traffic_stats_port(input.traffic_stats_port)?;
    let proxy_probe_url = normalize_proxy_probe_url(input.proxy_probe_url.as_deref())?;
    let (first_listen_port, hopping) = validate_listen_addr(&input.listen_addr)?;
    validate_hop_public_port(input.public_port, first_listen_port, hopping)?;
    validate_auth_type(&input.ssh_auth_type)?;
    if input.ssh_secret.is_empty() {
        return Err(ApiError::bad_request("ssh_secret cannot be empty"));
    }
    validate_server_options(&input.config)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    validate_listener_features(&input.config, &input.listen_addr)?;

    let id = Uuid::new_v4().to_string();
    let token = generate_token();
    let stats_secret = generate_token();
    let now = now();
    let config_json = input.config.to_string();
    let deployment_snapshot = json!({
        "server_config": input.config.clone(),
        "listen_addr": input.listen_addr.clone(),
        "traffic_stats_port": input.traffic_stats_port,
        "proxy_probe_url": proxy_probe_url
    });
    let deployment_snapshot_json = deployment_snapshot.to_string();
    let config_enc = state.secrets.encrypt(&config_json)?;
    let ssh_secret_enc = state.secrets.encrypt(&input.ssh_secret)?;
    let ssh_passphrase_enc = input
        .ssh_passphrase
        .as_deref()
        .map(|value| state.secrets.encrypt(value))
        .transpose()?;
    let node_token_enc = state.secrets.encrypt(&token)?;
    let stats_secret_enc = state.secrets.encrypt(&stats_secret)?;
    let digest = hex::encode(Sha256::digest(deployment_snapshot_json.as_bytes()));

    let mut tx = db::begin_write(&state.pool).await?;
    sqlx::query("INSERT INTO nodes (id, name, ssh_host, ssh_port, ssh_username, ssh_auth_type, ssh_secret_enc, ssh_passphrase_enc, ssh_host_fingerprint, public_host, public_port, listen_addr, traffic_stats_port, proxy_probe_url, tls_sni, tls_skip_verify, node_token_hash, node_token_enc, traffic_stats_secret_enc, desired_config_enc, desired_revision, state, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, 1, 'new', $21, $22)")
        .bind(&id).bind(input.name.trim()).bind(input.ssh_host.trim()).bind(i32::from(input.ssh_port))
        .bind(input.ssh_username.trim()).bind(&input.ssh_auth_type).bind(ssh_secret_enc).bind(ssh_passphrase_enc)
        .bind(input.ssh_host_fingerprint).bind(input.public_host.trim()).bind(i32::from(input.public_port))
        .bind(input.listen_addr.trim()).bind(i32::from(input.traffic_stats_port)).bind(&proxy_probe_url).bind(input.tls_sni).bind(input.tls_skip_verify).bind(token_digest(&token)).bind(node_token_enc)
        .bind(stats_secret_enc).bind(config_enc).bind(now).bind(now).execute(&mut *tx).await?;
    crate::node_limits::ensure(&mut tx, &id).await?;
    if let Some(package) = &input.package {
        crate::node_limits::save(&mut tx, &id, package).await?;
    }
    sqlx::query("UPDATE node_packages SET usage_bytes = $1 WHERE node_id = $2")
        .bind(input.initial_usage_bytes.unwrap_or(0))
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO config_versions (id, node_id, revision, config_enc, content_sha256, created_at) VALUES ($1, $2, 1, $3, $4, $5)")
        .bind(Uuid::new_v4().to_string()).bind(&id).bind(state.secrets.encrypt(&deployment_snapshot_json)?).bind(digest).bind(now)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'node.created', 'node', $2, $3, $4)")
        .bind(Uuid::new_v4().to_string()).bind(&id).bind(json!({"name": input.name.trim(), "package": input.package, "initial_usage_bytes": input.initial_usage_bytes.unwrap_or(0)})).bind(now)
        .execute(&mut *tx).await?;
    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "node": {"id": id, "name": input.name.trim(), "revision": 1, "state": "new", "traffic_stats_port": input.traffic_stats_port, "proxy_probe_url": proxy_probe_url},
            "node_auth_token": token,
            "note": "The node token is shown once here and is stored encrypted for server configuration generation."
        })),
    ))
}

pub async fn patch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<PatchNode>,
) -> Result<Json<Value>, ApiError> {
    if input.expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let current = sqlx::query("SELECT * FROM nodes WHERE id = $1")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("node"))?;
    let revision: i64 = current.get("desired_revision");
    if current.get::<String, _>("state") == "deleting" {
        return Err(ApiError::conflict("node deletion is in progress"));
    }
    if current.get::<String, _>("state") == "deleting" {
        return Err(ApiError::conflict("node deletion is already in progress"));
    }
    if revision != input.expected_revision {
        return Err(ApiError::conflict(format!(
            "node revision is {revision}; reload before editing"
        )));
    }

    let name: String = input.name.unwrap_or_else(|| current.get("name"));
    let ssh_host: String = input.ssh_host.unwrap_or_else(|| current.get("ssh_host"));
    let ssh_port: u16 = input
        .ssh_port
        .unwrap_or_else(|| current.get::<i32, _>("ssh_port") as u16);
    let ssh_username: String = input
        .ssh_username
        .unwrap_or_else(|| current.get("ssh_username"));
    let ssh_auth_type: String = input
        .ssh_auth_type
        .unwrap_or_else(|| current.get("ssh_auth_type"));
    let public_host: String = input
        .public_host
        .unwrap_or_else(|| current.get("public_host"));
    let public_port: u16 = input
        .public_port
        .unwrap_or_else(|| current.get::<i32, _>("public_port") as u16);
    let previous_traffic_stats_port = current.get::<i32, _>("traffic_stats_port") as u16;
    let traffic_stats_port = input
        .traffic_stats_port
        .unwrap_or(previous_traffic_stats_port);
    let traffic_stats_port_changed = traffic_stats_port != previous_traffic_stats_port;
    let listener_changed = input.listen_addr.is_some();
    let listen_addr: String = input
        .listen_addr
        .unwrap_or_else(|| current.get("listen_addr"));
    let previous_proxy_probe_url: Option<String> = current.get("proxy_probe_url");
    let proxy_probe_url = match input.proxy_probe_url.as_deref() {
        Some(value) => normalize_proxy_probe_url(Some(value))?,
        None => previous_proxy_probe_url.clone(),
    };
    let proxy_probe_url_changed = proxy_probe_url != previous_proxy_probe_url;
    let tls_sni = input.tls_sni.or_else(|| current.get("tls_sni"));
    let tls_skip_verify = input
        .tls_skip_verify
        .unwrap_or_else(|| current.get::<bool, _>("tls_skip_verify"));
    let fingerprint_supplied = input.ssh_host_fingerprint.is_some();
    let host_fingerprint = input
        .ssh_host_fingerprint
        .or_else(|| current.get("ssh_host_fingerprint"));
    let old_config_enc: String = current.get("desired_config_enc");
    let config_changed = input.config.is_some();
    let config: Value = if let Some(config) = input.config {
        config
    } else {
        serde_json::from_str(&state.secrets.decrypt(&old_config_enc)?)
            .map_err(|_| ApiError::internal())?
    };
    validate_text(&name, "name", 100)?;
    validate_text(&ssh_host, "ssh_host", 253)?;
    validate_text(&ssh_username, "ssh_username", 100)?;
    validate_text(&public_host, "public_host", 253)?;
    validate_text(&listen_addr, "listen_addr", 100)?;
    validate_traffic_stats_port(traffic_stats_port)?;
    let (first_listen_port, hopping) = validate_listen_addr(&listen_addr)?;
    validate_hop_public_port(public_port, first_listen_port, hopping)?;
    validate_auth_type(&ssh_auth_type)?;
    validate_server_options(&config).map_err(|error| ApiError::bad_request(error.to_string()))?;
    validate_listener_features(&config, &listen_addr)?;
    super::resources::resolve_config_resources(&state.pool, &state.secrets, &id, &config).await?;

    let secret_enc = match input.ssh_secret {
        Some(secret) if !secret.is_empty() => state.secrets.encrypt(&secret)?,
        Some(_) => return Err(ApiError::bad_request("ssh_secret cannot be empty")),
        None => current.get("ssh_secret_enc"),
    };
    let passphrase_enc = match input.ssh_passphrase {
        Some(passphrase) => Some(state.secrets.encrypt(&passphrase)?),
        None => current.get("ssh_passphrase_enc"),
    };
    let config_json = config.to_string();
    let deployment_snapshot = json!({
        "server_config": config.clone(),
        "listen_addr": listen_addr.clone(),
        "traffic_stats_port": traffic_stats_port,
        "proxy_probe_url": proxy_probe_url.clone()
    });
    let deployment_snapshot_json = deployment_snapshot.to_string();
    let config_enc = state.secrets.encrypt(&config_json)?;
    let updated_at = now();
    let next_revision = revision + 1;
    let mut tx = db::begin_write(&state.pool).await?;
    let m_tls_enabled = config
        .get("tls")
        .and_then(Value::as_object)
        .and_then(|tls| tls.get("clientCA"))
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if m_tls_enabled {
        let missing_client_credentials: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM node_assignments WHERE node_id = $1 AND (client_certificate_enc IS NULL OR client_private_key_enc IS NULL)",
        )
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
        if missing_client_credentials > 0 {
            return Err(ApiError::conflict(
                "assign matching client certificates to every user on this node before enabling mTLS",
            ));
        }
    }
    let updated = sqlx::query("UPDATE nodes SET name = $1, ssh_host = $2, ssh_port = $3, ssh_username = $4, ssh_auth_type = $5, ssh_secret_enc = $6, ssh_passphrase_enc = $7, ssh_host_fingerprint = $8, public_host = $9, public_port = $10, listen_addr = $11, traffic_stats_port = $12, proxy_probe_url = $13, tls_sni = $14, tls_skip_verify = $15, desired_config_enc = $16, desired_revision = $17, state = CASE WHEN $18 = TRUE AND state IN ('needs_fingerprint', 'fingerprint_changed') THEN 'ready' ELSE state END, updated_at = $19 WHERE id = $20 AND desired_revision = $21")
        .bind(name.trim()).bind(ssh_host.trim()).bind(i32::from(ssh_port)).bind(ssh_username.trim()).bind(ssh_auth_type)
        .bind(secret_enc).bind(passphrase_enc).bind(host_fingerprint).bind(public_host.trim()).bind(i32::from(public_port)).bind(listen_addr.trim())
        .bind(i32::from(traffic_stats_port)).bind(&proxy_probe_url).bind(tls_sni).bind(tls_skip_verify).bind(&config_enc).bind(next_revision).bind(fingerprint_supplied).bind(updated_at).bind(&id).bind(revision)
        .execute(&mut *tx).await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "node changed while saving; reload before editing",
        ));
    }
    sqlx::query("INSERT INTO config_versions (id, node_id, revision, config_enc, content_sha256, created_at) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(Uuid::new_v4().to_string()).bind(&id).bind(next_revision).bind(state.secrets.encrypt(&deployment_snapshot_json)?)
        .bind(hex::encode(Sha256::digest(deployment_snapshot_json.as_bytes()))).bind(updated_at).execute(&mut *tx).await?;
    if let Some(package) = &input.package {
        crate::node_limits::save(&mut tx, &id, package).await?;
    }
    let sync_required =
        config_changed || listener_changed || proxy_probe_url_changed || traffic_stats_port_changed;
    if sync_required {
        supersede_queued_syncs_in_tx(&mut tx, &id).await?;
        enqueue_job_in_tx(&mut tx, "sync", Some(&id), Some(next_revision)).await?;
    }
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'node.updated', 'node', $2, $3, $4)")
        .bind(Uuid::new_v4().to_string()).bind(&id).bind(json!({"revision": next_revision, "config_changed": config_changed, "proxy_probe_url_changed": proxy_probe_url_changed, "traffic_stats_port_changed": traffic_stats_port_changed, "package_changed": input.package.is_some(), "package": input.package})).bind(updated_at)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"id": id, "name": name.trim(), "revision": next_revision, "state": current.get::<String, _>("state"), "traffic_stats_port": traffic_stats_port, "proxy_probe_url": proxy_probe_url, "sync_job_queued": sync_required}),
    ))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RevisionQuery>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let expected = query
        .expected_revision
        .ok_or_else(|| ApiError::bad_request("expected_revision query parameter is required"))?;
    let node =
        sqlx::query("SELECT desired_revision, deployed_revision, state FROM nodes WHERE id = $1")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| ApiError::not_found("node"))?;
    let revision: i64 = node.get("desired_revision");
    if revision != expected {
        return Err(ApiError::conflict(
            "node revision changed; reload before deleting",
        ));
    }
    let deployed_revision: Option<i64> = node.get("deployed_revision");
    let mut tx = db::begin_write(&state.pool).await?;
    let node_state: String = sqlx::query_scalar("SELECT state FROM nodes WHERE id = $1")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
    if node_state == "deleting" {
        let existing: Option<String> = sqlx::query_scalar("SELECT id FROM jobs WHERE node_id = $1 AND kind = 'uninstall' AND status IN ('queued', 'running') ORDER BY created_at DESC LIMIT 1")
            .bind(&id).fetch_optional(&mut *tx).await?;
        if let Some(job_id) = existing {
            tx.rollback().await?;
            return Ok((
                StatusCode::ACCEPTED,
                Json(json!({"job_id": job_id, "status": "queued"})),
            ));
        }
    }
    let active_deployment: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE node_id = $1 AND status = 'running' AND kind IN ('deploy', 'sync', 'rollback', 'uninstall')")
        .bind(&id).fetch_one(&mut *tx).await?;
    let never_installed = deployed_revision.is_none()
        && active_deployment == 0
        && matches!(
            node_state.as_str(),
            "new" | "ready" | "needs_fingerprint" | "fingerprint_changed"
        );
    if never_installed {
        cancel_queued_node_jobs_in_tx(&mut tx, &id).await?;
        sqlx::query("DELETE FROM nodes WHERE id = $1 AND desired_revision = $2")
            .bind(&id)
            .bind(expected)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'node.deleted', 'node', $2, $3, $4)")
            .bind(Uuid::new_v4().to_string()).bind(&id).bind(json!({"expected_revision": expected, "remote_install": false})).bind(now()).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok((StatusCode::NO_CONTENT, Json(Value::Null)));
    }
    if node_state != "deleting" {
        let updated = sqlx::query("UPDATE nodes SET state = 'deleting', updated_at = $1 WHERE id = $2 AND desired_revision = $3 AND state = $4")
            .bind(now()).bind(&id).bind(expected).bind(&node_state).execute(&mut *tx).await?;
        if updated.rows_affected() == 0 {
            let existing: Option<String> = sqlx::query_scalar("SELECT id FROM jobs WHERE node_id = $1 AND kind = 'uninstall' AND status IN ('queued', 'running') ORDER BY created_at DESC LIMIT 1")
                .bind(&id).fetch_optional(&mut *tx).await?;
            if let Some(job_id) = existing {
                tx.rollback().await?;
                return Ok((
                    StatusCode::ACCEPTED,
                    Json(json!({"job_id": job_id, "status": "queued"})),
                ));
            }
            return Err(ApiError::conflict(
                "node changed while deletion was starting",
            ));
        }
    }
    cancel_queued_node_jobs_in_tx(&mut tx, &id).await?;
    let job_id = enqueue_job_in_tx(&mut tx, "uninstall", Some(&id), Some(revision)).await?;
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'node.uninstall_requested', 'node', $2, $3, $4)")
        .bind(Uuid::new_v4().to_string()).bind(&id).bind(json!({"job_id": job_id, "expected_revision": expected})).bind(now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"job_id": job_id, "status": "queued"})),
    ))
}

pub async fn deploy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<NodeAction>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    action(&state, &id, input.expected_revision, "deploy").await
}

pub async fn ssh_test(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<NodeAction>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    action(&state, &id, input.expected_revision, "ssh-test").await
}

pub async fn sync(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<NodeAction>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    action(&state, &id, input.expected_revision, "sync").await
}

pub async fn rollback(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<NodeAction>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let row = sqlx::query("SELECT deployed_revision FROM nodes WHERE id = $1")
        .bind(&id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("node"))?;
    let deployed: Option<i64> = row.get("deployed_revision");
    let Some(deployed) = deployed else {
        return Err(ApiError::conflict(
            "node has no successful deployment to roll back",
        ));
    };
    let previous: Option<i64> = sqlx::query_scalar("SELECT MAX(revision) FROM config_versions WHERE node_id = $1 AND deployed_success = TRUE AND revision < $2")
        .bind(&id).bind(deployed).fetch_one(&state.pool).await?;
    let Some(previous) = previous else {
        return Err(ApiError::conflict(
            "node has no earlier successful configuration to restore",
        ));
    };
    action_to(
        &state,
        &id,
        input.expected_revision,
        "rollback",
        previous,
        json!({}),
    )
    .await
}

async fn action(
    state: &AppState,
    id: &str,
    expected: i64,
    kind: &str,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let row = sqlx::query("SELECT desired_revision, state FROM nodes WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("node"))?;
    let revision: i64 = row.get("desired_revision");
    let state_value: String = row.get("state");
    if matches!(state_value.as_str(), "deleting" | "delete_failed") {
        return Err(ApiError::conflict(
            "node is being deleted; retry deletion after checking the SSH state",
        ));
    }
    if expected != revision {
        return Err(ApiError::conflict(format!(
            "node revision is {revision}; reload before starting {kind}"
        )));
    }
    let payload = if kind == "sync" {
        json!({"force": true})
    } else {
        json!({})
    };
    action_to(state, id, expected, kind, revision, payload).await
}

async fn action_to(
    state: &AppState,
    id: &str,
    expected: i64,
    kind: &str,
    target_revision: i64,
    payload: Value,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let row = sqlx::query("SELECT desired_revision, state FROM nodes WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("node"))?;
    let desired_revision: i64 = row.get("desired_revision");
    let state_value: String = row.get("state");
    if matches!(state_value.as_str(), "deleting" | "delete_failed") {
        return Err(ApiError::conflict(
            "node is being deleted; retry deletion after checking the SSH state",
        ));
    }
    if expected != desired_revision {
        return Err(ApiError::conflict(format!(
            "node revision is {desired_revision}; reload before starting {kind}"
        )));
    }
    let mut tx = db::begin_write(&state.pool).await?;
    let job_id =
        enqueue_job_with_payload_in_tx(&mut tx, kind, Some(id), Some(target_revision), payload)
            .await?;
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', $2, 'node', $3, $4, $5)")
        .bind(Uuid::new_v4().to_string()).bind(format!("node.{kind}" )).bind(id).bind(json!({"job_id": job_id, "target_revision": target_revision})).bind(now())
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"job_id": job_id, "status": "queued"})),
    ))
}

async fn node_json(state: &AppState, row: &sqlx::postgres::PgRow) -> Result<Value, ApiError> {
    let config_enc: String = row.get("desired_config_enc");
    let config: Value = serde_json::from_str(&state.secrets.decrypt(&config_enc)?)
        .map_err(|_| ApiError::internal())?;
    let node_id: String = row.get("id");
    let last_sample_at: Option<chrono::DateTime<chrono::Utc>> = row.get("last_sample_at");
    let data_freshness = match last_sample_at.as_ref() {
        None => "not_collected",
        Some(sample) if (chrono::Utc::now() - *sample).num_seconds() <= 30 => "fresh",
        Some(_) => "stale",
    };
    let telemetry = sqlx::query(
        "SELECT
            (SELECT COUNT(*) FROM data_gaps WHERE node_id = $1 AND resolved_at IS NULL) AS open_gaps,
            (SELECT COUNT(*) FROM jobs WHERE node_id = $2 AND kind = 'kick' AND status IN ('queued', 'running')) AS pending_revocations",
    )
    .bind(&node_id)
    .bind(&node_id)
    .fetch_one(&state.pool)
    .await?;
    let (resolved_config, _) =
        super::resources::resolve_config_resources(&state.pool, &state.secrets, &node_id, &config)
            .await?;
    let traffic_stats_port = row.get::<i32, _>("traffic_stats_port") as u16;
    let yaml_preview = render_server_yaml_preview_with_traffic_stats_port(
        &resolved_config,
        row.get::<String, _>("listen_addr").as_str(),
        &node_id,
        "<managed-node-token>",
        "<managed-stats-secret>",
        &std::env::var("HYSTERIAX_PUBLIC_URL")
            .unwrap_or_else(|_| "https://management.example.invalid".into()),
        traffic_stats_port,
    )
    .map_err(|_| ApiError::internal())?;
    let (package, package_usage) = crate::node_limits::snapshot(&state.pool, &node_id)
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(json!({
        "package": package, "package_usage": package_usage,
        "id": node_id, "name": row.get::<String, _>("name"),
        "ssh": {"host": row.get::<String, _>("ssh_host"), "port": row.get::<i32, _>("ssh_port"), "username": row.get::<String, _>("ssh_username"), "auth_type": row.get::<String, _>("ssh_auth_type"), "secret_configured": true, "host_fingerprint": row.get::<Option<String>, _>("ssh_host_fingerprint")},
        "public": {"host": row.get::<String, _>("public_host"), "port": row.get::<i32, _>("public_port"), "listen_addr": row.get::<String, _>("listen_addr"), "tls_sni": row.get::<Option<String>, _>("tls_sni"), "skip_cert_verify": row.get::<bool, _>("tls_skip_verify")},
        "traffic_stats_port": traffic_stats_port,
        "config": config, "yaml_preview": yaml_preview,
        "revision": row.get::<i64, _>("desired_revision"), "deployed_revision": row.get::<Option<i64>, _>("deployed_revision"),
        "state": row.get::<String, _>("state"), "last_seen_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_seen_at"),
        "proxy_probe_url": row.get::<Option<String>, _>("proxy_probe_url"),
        "last_sample_at": last_sample_at, "data_freshness": data_freshness,
        "open_gaps": telemetry.get::<i64, _>("open_gaps"),
        "pending_revocations": telemetry.get::<i64, _>("pending_revocations"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"), "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
    }))
}

fn validate_auth_type(value: &str) -> Result<(), ApiError> {
    if matches!(value, "password" | "private_key") {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "ssh_auth_type must be password or private_key",
        ))
    }
}

fn normalize_proxy_probe_url(value: Option<&str>) -> Result<Option<String>, ApiError> {
    let Some(value) = value.map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > 2048 {
        return Err(ApiError::bad_request(
            "proxy_probe_url must be at most 2048 characters",
        ));
    }
    let url = Url::parse(value)
        .map_err(|_| ApiError::bad_request("proxy_probe_url must be a valid HTTP URL"))?;
    if url.scheme() != "http"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError::bad_request(
            "proxy_probe_url must be an HTTP URL without credentials, query, or fragment",
        ));
    }
    Ok(Some(url.to_string()))
}

fn validate_listen_addr(value: &str) -> Result<(u16, bool), ApiError> {
    let Some((_, port)) = value.rsplit_once(':') else {
        return Err(ApiError::bad_request(
            "listen_addr must contain a host and port",
        ));
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut first = None;
    for item in port.split(',') {
        if let Some((start, end)) = item.split_once('-') {
            if end.contains('-') {
                return Err(invalid_listen_ports());
            }
            let start = parse_listen_port(start).ok_or_else(invalid_listen_ports)?;
            let end = parse_listen_port(end).ok_or_else(invalid_listen_ports)?;
            if start >= end {
                return Err(invalid_listen_ports());
            }
            first.get_or_insert(start);
            for value in start..=end {
                if !seen.insert(value) {
                    return Err(invalid_listen_ports());
                }
            }
        } else {
            let value = parse_listen_port(item).ok_or_else(invalid_listen_ports)?;
            first.get_or_insert(value);
            if !seen.insert(value) {
                return Err(invalid_listen_ports());
            }
        }
    }
    Ok((
        first.ok_or_else(invalid_listen_ports)?,
        port.contains(',') || port.contains('-'),
    ))
}

fn parse_listen_port(value: &str) -> Option<u16> {
    if value.is_empty() || value.trim() != value {
        return None;
    }
    value
        .parse::<u16>()
        .ok()
        .filter(|port| (1..=65535).contains(port))
}

fn invalid_listen_ports() -> ApiError {
    ApiError::bad_request(
        "listen_addr ports must be single ports or non-overlapping ranges between 1 and 65535",
    )
}

fn validate_hop_public_port(
    public_port: u16,
    first_listen_port: u16,
    hopping: bool,
) -> Result<(), ApiError> {
    if hopping && public_port != first_listen_port {
        return Err(ApiError::bad_request(
            "public_port must match the first listen port when port hopping is enabled",
        ));
    }
    Ok(())
}

fn validate_listener_features(config: &Value, listen_addr: &str) -> Result<(), ApiError> {
    if crate::config::realm_connection(config)
        .map_err(|error| ApiError::bad_request(error.to_string()))?
        .is_some()
        && crate::config::listener_hop_ports(listen_addr).is_some()
    {
        return Err(ApiError::bad_request(
            "port hopping cannot be combined with Hysteria Realms mode",
        ));
    }
    Ok(())
}

fn validate_text(value: &str, field: &str, max: usize) -> Result<(), ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!(
            "{field} must contain 1 to {max} printable characters"
        )));
    }
    Ok(())
}

fn empty_config() -> Value {
    json!({})
}

fn default_traffic_stats_port() -> u16 {
    DEFAULT_TRAFFIC_STATS_PORT
}

fn validate_traffic_stats_port(port: u16) -> Result<(), ApiError> {
    if port == 0 {
        return Err(ApiError::bad_request(
            "traffic_stats_port must be between 1 and 65535",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        normalize_proxy_probe_url, validate_hop_public_port, validate_listen_addr,
        validate_listener_features,
    };

    #[test]
    fn validates_optional_http_probe_targets_without_embedded_credentials() {
        assert_eq!(normalize_proxy_probe_url(None).unwrap(), None);
        assert_eq!(normalize_proxy_probe_url(Some("  ")).unwrap(), None);
        assert_eq!(
            normalize_proxy_probe_url(Some("http://status.example.test/health")).unwrap(),
            Some("http://status.example.test/health".to_owned())
        );
        assert!(normalize_proxy_probe_url(Some("https://status.example.test/health")).is_err());
        assert!(normalize_proxy_probe_url(Some("http://user:pass@status.example.test")).is_err());
        assert!(
            normalize_proxy_probe_url(Some("http://status.example.test/health?token=secret"))
                .is_err()
        );
    }

    #[test]
    fn validates_single_and_hopping_listener_port_lists() {
        assert_eq!(validate_listen_addr(":443").unwrap(), (443, false));
        assert_eq!(validate_listen_addr("0.0.0.0:443").unwrap(), (443, false));
        assert_eq!(validate_listen_addr(":443-445").unwrap(), (443, true));
        assert_eq!(validate_listen_addr(":443,445-446").unwrap(), (443, true));
        assert!(validate_listen_addr(":0").is_err());
        assert!(validate_listen_addr(":445-443").is_err());
        assert!(validate_listen_addr(":443,443-445").is_err());
        assert!(validate_listen_addr(":443, 445").is_err());
        assert!(validate_hop_public_port(443, 443, true).is_ok());
        assert!(validate_hop_public_port(8443, 443, true).is_err());
    }

    #[test]
    fn realm_requires_a_single_listener_port() {
        let realm = json!({
            "realm": {"connection": {
                "serverURL": "https://rendezvous.example",
                "token": "realm-token",
                "realmID": "realm-test"
            }}
        });
        assert!(validate_listener_features(&realm, ":443").is_ok());
        assert!(validate_listener_features(&realm, ":443-445").is_err());
        assert!(validate_listener_features(&realm, ":443,445-446").is_err());
    }
}
