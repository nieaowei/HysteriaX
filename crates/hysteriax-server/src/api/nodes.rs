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
#[serde(deny_unknown_fields)]
pub struct CreateNode {
    package: Option<crate::node_limits::Package>,
    initial_usage_bytes: Option<i64>,
    name: String,
    ssh_host: String,
    ssh_port: u16,
    ssh_username: String,
    ssh_credential_id: String,
    ssh_credential_version: i64,
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
#[serde(deny_unknown_fields)]
pub struct PatchNode {
    package: Option<crate::node_limits::Package>,
    expected_revision: i64,
    name: Option<String>,
    ssh_host: Option<String>,
    ssh_port: Option<u16>,
    ssh_username: Option<String>,
    ssh_credential_id: Option<String>,
    ssh_credential_version: Option<i64>,
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
    validate_ssh_credential(
        &state,
        &input.ssh_credential_id,
        input.ssh_credential_version,
        false,
    )
    .await?;
    let id = Uuid::new_v4().to_string();
    crate::credentials::require_managed_config(&input.config)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let (resolved, _) =
        super::resources::resolve_config_resources(&state.pool, &state.secrets, &id, &input.config)
            .await
            .map_err(|_| ApiError::bad_request("invalid configuration credential"))?;
    validate_server_options(&resolved)
        .map_err(|_| ApiError::bad_request("invalid node configuration"))?;
    validate_listener_features(&input.config, &input.listen_addr)?;

    let token = generate_token();
    let stats_secret = generate_token();
    let now = now();
    let config_json = input.config.to_string();
    let deployment_snapshot = json!({
        "server_config": input.config.clone(),
        "listen_addr": input.listen_addr.clone(),
        "traffic_stats_port": input.traffic_stats_port,
        "proxy_probe_url": proxy_probe_url,
        "public_host": input.public_host.clone(), "public_port": input.public_port,
        "tls_sni": input.tls_sni.clone(), "tls_skip_verify": input.tls_skip_verify
    });
    let deployment_snapshot_json = deployment_snapshot.to_string();
    let config_enc = state.secrets.encrypt(&config_json)?;
    let node_token_enc = state.secrets.encrypt(&token)?;
    let stats_secret_enc = state.secrets.encrypt(&stats_secret)?;
    let digest = hex::encode(Sha256::digest(deployment_snapshot_json.as_bytes()));

    let mut tx = db::begin_write(&state.pool).await?;
    crate::credentials::check_bindings(
        &mut tx,
        &input.config,
        None,
        Some((
            &input.ssh_credential_id,
            input.ssh_credential_version,
            false,
        )),
    )
    .await
    .map_err(|e| ApiError::conflict(e.to_string()))?;
    sqlx::query("INSERT INTO nodes (id, name, ssh_host, ssh_port, ssh_username, ssh_credential_id, ssh_credential_version, ssh_host_fingerprint, public_host, public_port, listen_addr, traffic_stats_port, proxy_probe_url, tls_sni, tls_skip_verify, node_token_hash, node_token_enc, traffic_stats_secret_enc, desired_config_enc, desired_revision, state, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, 1, 'new', $20, $21)")
        .bind(&id).bind(input.name.trim()).bind(input.ssh_host.trim()).bind(i32::from(input.ssh_port))
        .bind(input.ssh_username.trim()).bind(&input.ssh_credential_id).bind(input.ssh_credential_version)
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
    if input.ssh_credential_id.is_some() != input.ssh_credential_version.is_some() {
        return Err(ApiError::bad_request(
            "SSH credential ID and version must be supplied together",
        ));
    }
    let ssh_credential_id: String = input
        .ssh_credential_id
        .unwrap_or_else(|| current.get("ssh_credential_id"));
    let ssh_credential_version: i64 = input
        .ssh_credential_version
        .unwrap_or_else(|| current.get("ssh_credential_version"));
    let ssh_changed = ssh_credential_id != current.get::<String, _>("ssh_credential_id")
        || ssh_credential_version != current.get::<i64, _>("ssh_credential_version");
    validate_ssh_credential(
        &state,
        &ssh_credential_id,
        ssh_credential_version,
        !ssh_changed,
    )
    .await?;
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
    if ssh_changed {
        let (kind, _, _, payload, _) = crate::credentials::load(
            &state.pool,
            &state.secrets,
            &ssh_credential_id,
            ssh_credential_version,
        )
        .await?;
        let candidate = crate::ssh::SshNode {
            host: ssh_host.clone(),
            port: ssh_port,
            username: ssh_username.clone(),
            auth_type: if kind == "ssh_password" {
                "password"
            } else {
                "private_key"
            }
            .into(),
            secret: payload["secret"]
                .as_str()
                .ok_or_else(ApiError::internal)?
                .into(),
            passphrase: payload["passphrase"].as_str().map(str::to_owned),
            host_fingerprint: host_fingerprint.clone(),
        };
        match crate::ssh::connect(&candidate).await.map_err(|_| {
            ApiError::bad_request("SSH credential verification failed; previous binding remains")
        })? {
            crate::ssh::FingerprintResult::Trusted(session) => {
                crate::ssh::inspect_connected(&session).await.map_err(|_| {
                    ApiError::bad_request(
                        "SSH privilege verification failed; previous binding remains",
                    )
                })?;
            }
            _ => {
                return Err(ApiError::conflict(
                    "SSH host fingerprint must be confirmed before changing credentials",
                ));
            }
        }
    }
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
    crate::credentials::require_managed_config(&config)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let (resolved, _) =
        crate::credentials::resolve_config(&state.pool, &state.secrets, &config, false)
            .await
            .map_err(|_| ApiError::bad_request("invalid configuration credential"))?;
    validate_server_options(&resolved)
        .map_err(|_| ApiError::bad_request("invalid node configuration"))?;
    validate_listener_features(&config, &listen_addr)?;
    super::resources::resolve_config_resources(&state.pool, &state.secrets, &id, &config).await?;

    let config_json = config.to_string();
    let deployment_snapshot = json!({
        "server_config": config.clone(),
        "listen_addr": listen_addr.clone(),
        "traffic_stats_port": traffic_stats_port,
        "proxy_probe_url": proxy_probe_url.clone(),
        "public_host": public_host.clone(), "public_port": public_port,
        "tls_sni": tls_sni.clone(), "tls_skip_verify": tls_skip_verify
    });
    let deployment_snapshot_json = deployment_snapshot.to_string();
    let config_enc = state.secrets.encrypt(&config_json)?;
    let updated_at = now();
    let next_revision = revision + 1;
    let old_config: Value = serde_json::from_str(&state.secrets.decrypt(&old_config_enc)?)
        .map_err(|_| ApiError::internal())?;
    let mut tx = db::begin_write(&state.pool).await?;
    crate::credentials::check_bindings(
        &mut tx,
        &config,
        Some(&old_config),
        Some((&ssh_credential_id, ssh_credential_version, !ssh_changed)),
    )
    .await
    .map_err(|e| ApiError::conflict(e.to_string()))?;
    let m_tls_enabled = config
        .get("tls")
        .and_then(Value::as_object)
        .and_then(|tls| tls.get("clientCA"))
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    if m_tls_enabled {
        let missing_client_credentials: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM node_assignments WHERE node_id = $1 AND (mtls_credential_id IS NULL OR mtls_credential_version IS NULL)",
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
    let updated = sqlx::query("UPDATE nodes SET name = $1, ssh_host = $2, ssh_port = $3, ssh_username = $4, ssh_credential_id = $5, ssh_credential_version = $6, ssh_host_fingerprint = $7, public_host = $8, public_port = $9, listen_addr = $10, traffic_stats_port = $11, proxy_probe_url = $12, tls_sni = $13, tls_skip_verify = $14, desired_config_enc = $15, desired_revision = $16, state = CASE WHEN $17 = TRUE AND state IN ('needs_fingerprint', 'fingerprint_changed') THEN 'ready' ELSE state END, updated_at = $18 WHERE id = $19 AND desired_revision = $20")
        .bind(name.trim()).bind(ssh_host.trim()).bind(i32::from(ssh_port)).bind(ssh_username.trim()).bind(&ssh_credential_id)
        .bind(ssh_credential_version).bind(host_fingerprint).bind(public_host.trim()).bind(i32::from(public_port)).bind(listen_addr.trim())
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

/// Forget a node without connecting to it or claiming remote uninstall success.
pub async fn remove_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RevisionQuery>,
) -> Result<StatusCode, ApiError> {
    let expected = query.expected_revision.filter(|r| *r > 0).ok_or_else(|| {
        ApiError::bad_request("positive expected_revision query parameter is required")
    })?;
    let mut tx = db::begin_write(&state.pool).await?;
    let node = sqlx::query("SELECT name, desired_revision FROM nodes WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("node"))?;
    if node.get::<i64, _>("desired_revision") != expected {
        return Err(ApiError::conflict(
            "node revision changed; reload before removing its record",
        ));
    }
    // Cancelling the local future cannot retract an already-issued remote script.
    // Wait for these operations before promising a record-only removal.
    let mutating: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs WHERE node_id=$1 AND status='running' AND kind IN ('deploy','sync','rollback','uninstall','credential-apply'))")
        .bind(&id).fetch_one(&mut *tx).await?;
    if mutating {
        return Err(ApiError::conflict(
            "a remote deployment, uninstall or credential update is still running; wait for it to finish before removing the node record",
        ));
    }
    let cancelled = sqlx::query("UPDATE jobs SET status='cancelled',stage='node_removed',updated_at=now(),finished_at=now() WHERE node_id=$1 AND status IN ('queued','running') RETURNING id,kind")
        .bind(&id).fetch_all(&mut *tx).await?;
    for job in &cancelled {
        let job_id: String = job.get("id");
        crate::kick_requests::event(&mut tx,&job_id,"job.cancelled",json!({"id":job_id,"kind":job.get::<String,_>("kind"),"node_id":id,"status":"cancelled","stage":"node_removed"})).await?;
    }
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM kick_requests WHERE node_id=$1 AND state NOT IN ('completed','cancelled')")
        .bind(&id).fetch_one(&mut *tx).await?;
    let assignments: i64 =
        sqlx::query_scalar("SELECT count(*) FROM node_assignments WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
    // Child configuration, assignments, monitoring data and kick obligations
    // cascade; jobs retain their historical node ID, name snapshot and events.
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,'admin','node.record_removed','node',$2,$3,now())")
        .bind(Uuid::new_v4().to_string()).bind(&id).bind(json!({"node_name":node.get::<String,_>("name"),"expected_revision":expected,"remote_uninstall":false,"remote_service_may_be_running":true,"cancelled_jobs":cancelled.len(),"removed_assignments":assignments,"removed_pending_kicks":pending})).execute(&mut *tx).await?;
    tx.commit().await?;
    let _ = state.removed_nodes.send(id);
    Ok(StatusCode::NO_CONTENT)
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
    let active_deployment: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE node_id = $1 AND status = 'running' AND kind IN ('deploy', 'sync', 'rollback', 'uninstall', 'credential-apply')")
        .bind(&id).fetch_one(&mut *tx).await?;
    let never_installed = deployed_revision.is_none()
        && active_deployment == 0
        && matches!(
            node_state.as_str(),
            "new" | "ready" | "needs_fingerprint" | "fingerprint_changed"
        );
    if never_installed {
        cancel_queued_node_jobs_in_tx(&mut tx, &id).await?;
        cancel_running_node_jobs_in_tx(&mut tx, &id).await?;
        sqlx::query("DELETE FROM nodes WHERE id = $1 AND desired_revision = $2")
            .bind(&id)
            .bind(expected)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'node.deleted', 'node', $2, $3, $4)")
            .bind(Uuid::new_v4().to_string()).bind(&id).bind(json!({"expected_revision": expected, "remote_install": false})).bind(now()).execute(&mut *tx).await?;
        tx.commit().await?;
        let _ = state.removed_nodes.send(id);
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

async fn cancel_running_node_jobs_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    node_id: &str,
) -> Result<(), ApiError> {
    let rows = sqlx::query("UPDATE jobs SET status = 'cancelled', stage = 'node_removed', updated_at = $1, finished_at = $1 WHERE node_id = $2 AND status = 'running' RETURNING id, kind")
        .bind(now())
        .bind(node_id)
        .fetch_all(&mut **tx)
        .await?;
    for row in rows {
        let id: String = row.get("id");
        let kind: String = row.get("kind");
        let payload = json!({"id": id, "kind": kind, "node_id": node_id, "status": "cancelled", "stage": "node_removed"});
        sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ($1, 'job.cancelled', $2, $3)")
            .bind(&id)
            .bind(payload)
            .bind(now())
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
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
    let credential_kind: String = sqlx::query_scalar("SELECT kind FROM credentials WHERE id=$1")
        .bind(row.get::<String, _>("ssh_credential_id"))
        .fetch_one(&state.pool)
        .await?;
    Ok(json!({
        "package": package, "package_usage": package_usage,
        "id": node_id, "name": row.get::<String, _>("name"),
        "ssh": {"host": row.get::<String, _>("ssh_host"), "port": row.get::<i32, _>("ssh_port"), "username": row.get::<String, _>("ssh_username"), "auth_type": if credential_kind == "ssh_password" { "password" } else { "private_key" }, "credential_id": row.get::<String,_>("ssh_credential_id"), "credential_version": row.get::<i64,_>("ssh_credential_version"), "secret_configured": true, "host_fingerprint": row.get::<Option<String>, _>("ssh_host_fingerprint")},
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

async fn validate_ssh_credential(
    state: &AppState,
    id: &str,
    version: i64,
    allow_archived: bool,
) -> Result<(), ApiError> {
    let (kind, owner, archived, _, _) =
        crate::credentials::load(&state.pool, &state.secrets, id, version)
            .await
            .map_err(|_| ApiError::bad_request("SSH credential not found"))?;
    if owner.is_some()
        || (archived && !allow_archived)
        || !matches!(kind.as_str(), "ssh_private_key" | "ssh_password")
    {
        return Err(ApiError::bad_request("invalid or archived SSH credential"));
    }
    Ok(())
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

#[cfg(test)]
mod removal_tests {
    use super::*;

    async fn remove(state: &AppState, revision: Option<i64>) -> Result<StatusCode, ApiError> {
        remove_record(
            State(state.clone()),
            Path("node".into()),
            Query(RevisionQuery {
                expected_revision: revision,
            }),
        )
        .await
    }

    #[tokio::test]
    async fn record_removal_cancels_work_and_keeps_history_without_ssh() {
        let state = crate::kick_requests::tests::fixture().await;
        sqlx::query("UPDATE nodes SET deployed_revision=1,state='unreachable' WHERE id='node'")
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('user','node','hash','unused',now())").execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES('config','node',1,'unused','hash',now())").execute(&state.pool).await.unwrap();
        let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
        sqlx::query("UPDATE jobs SET status='running' WHERE id=$1")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        let queued = enqueue_job_in_tx(&mut tx, "sync", Some("node"), Some(1))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut receiver = state.removed_nodes.subscribe();
        assert_eq!(
            remove(&state, Some(1)).await.unwrap(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(receiver.try_recv().unwrap(), "node");
        for table in [
            "nodes",
            "node_assignments",
            "config_versions",
            "kick_requests",
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(count, 0, "{table}");
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            1
        );
        for job in [&id, &queued] {
            let row = sqlx::query("SELECT status,stage,node_id,node_name FROM jobs WHERE id=$1")
                .bind(job)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(row.get::<String, _>("status"), "cancelled");
            assert_eq!(row.get::<String, _>("stage"), "node_removed");
            assert_eq!(row.get::<Option<String>, _>("node_id"), Some("node".into()));
            assert_eq!(row.get::<String, _>("node_name"), "Node");
        }
        let events: i64 =
            sqlx::query_scalar("SELECT count(*) FROM job_events WHERE event_type='job.cancelled'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(events, 2);
        let detail: Value = sqlx::query_scalar(
            "SELECT detail_json FROM audit_records WHERE action='node.record_removed'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(detail["remote_uninstall"], false);
        assert_eq!(detail["removed_pending_kicks"], 1);
        assert_eq!(detail["removed_assignments"], 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind='uninstall'")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            0
        );
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        crate::kick_requests::schedule(&mut tx, None, true)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn record_removal_rejects_missing_stale_revision_and_running_remote_mutation() {
        let state = crate::kick_requests::tests::fixture().await;
        for revision in [None, Some(0), Some(-1)] {
            assert_eq!(
                remove(&state, revision).await.unwrap_err().status,
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            remove(&state, Some(99)).await.unwrap_err().status,
            StatusCode::CONFLICT
        );
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        let id = enqueue_job_in_tx(&mut tx, "uninstall", Some("node"), None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        for kind in [
            "deploy",
            "sync",
            "rollback",
            "uninstall",
            "credential-apply",
        ] {
            sqlx::query("UPDATE jobs SET kind=$2,status='running' WHERE id=$1")
                .bind(&id)
                .bind(kind)
                .execute(&state.pool)
                .await
                .unwrap();
            assert_eq!(
                remove(&state, Some(1)).await.unwrap_err().status,
                StatusCode::CONFLICT
            );
            assert_eq!(
                sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id=$1")
                    .bind(&id)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap(),
                "running"
            );
        }
        sqlx::query("UPDATE jobs SET status='failed' WHERE id=$1")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE nodes SET state='delete_failed',deployed_revision=1")
            .execute(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            remove(&state, Some(1)).await.unwrap(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            remove(&state, Some(1)).await.unwrap_err().status,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn record_removal_event_failure_rolls_back_everything_and_sends_no_signal() {
        let state = crate::kick_requests::tests::fixture().await;
        let id = crate::kick_requests::tests::enqueue(&state, "credentials_revoked").await;
        sqlx::raw_sql("CREATE FUNCTION reject_remove_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.cancelled' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_remove_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_remove_event();").execute(&state.pool).await.unwrap();
        let mut receiver = state.removed_nodes.subscribe();
        assert!(remove(&state, Some(1)).await.is_err());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id=$1")
                .bind(&id)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            "queued"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM kick_requests")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            1
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn regular_delete_still_queues_uninstall_for_deployed_node() {
        let state = crate::kick_requests::tests::fixture().await;
        sqlx::query("UPDATE nodes SET deployed_revision=1,state='unreachable'")
            .execute(&state.pool)
            .await
            .unwrap();
        let (status, Json(reply)) = delete(
            State(state.clone()),
            Path("node".into()),
            Query(RevisionQuery {
                expected_revision: Some(1),
            }),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT kind FROM jobs WHERE id=$1")
                .bind(reply["job_id"].as_str().unwrap())
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            "uninstall"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM nodes")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            "deleting"
        );
    }

    #[tokio::test]
    async fn http_record_removal_requires_auth_and_advertises_capability() {
        let state = crate::kick_requests::tests::fixture().await;
        let token = "record-removal-test-token-with-256-bits-of-entropy";
        sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('admin',$1,'test',now())").bind(crate::security::token_digest(token)).execute(&state.pool).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = crate::api::router(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        let endpoint = format!("http://{address}/api/v1/nodes/node/record");
        assert_eq!(
            client
                .delete(format!("{endpoint}?expected_revision=1"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let version: Value = client
            .get(format!("http://{address}/api/v1/version"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(
            version["features"]
                .as_array()
                .unwrap()
                .contains(&json!("node_record_removal"))
        );
        assert_eq!(
            client
                .delete(&endpoint)
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            client
                .delete(format!("{endpoint}?expected_revision=99"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        let response = client
            .delete(format!("{endpoint}?expected_revision=1"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(response.bytes().await.unwrap().is_empty());
        assert_eq!(
            client
                .delete(format!("{endpoint}?expected_revision=1"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        task.abort();
    }
}
