use crate::db;
use axum::{
    Json,
    extract::{Path, RawQuery, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{
    api::{generate_token, now},
    error::ApiError,
    security::token_digest,
    state::AppState,
};

mod formats;
use formats::{ClientInfo, SubscriptionFormat, SubscriptionNode, SubscriptionRealm};

#[derive(Deserialize)]
pub struct RotateSubscription {
    expected_revision: i64,
}

#[derive(Serialize)]
struct ClashProxy {
    name: String,
    #[serde(rename = "type")]
    protocol: String,
    server: String,
    port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    ports: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "hop-interval")]
    hop_interval: Option<u64>,
    password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sni: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    certificate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "private-key")]
    private_key: Option<String>,
    #[serde(rename = "skip-cert-verify")]
    skip_cert_verify: bool,
    udp: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    up: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    down: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    obfs: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "obfs-password")]
    obfs_password: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "obfs-min-packet-size"
    )]
    obfs_min_packet_size: Option<u64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "obfs-max-packet-size"
    )]
    obfs_max_packet_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "ech-opts")]
    ech_opts: Option<ClashEchOptions>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "realm-opts")]
    realm_opts: Option<ClashRealmOptions>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "handshake-timeout")]
    handshake_timeout: Option<u64>,
}

#[derive(Serialize)]
struct ClashEchOptions {
    enable: bool,
    config: String,
}

#[derive(Serialize)]
struct ClashRealmOptions {
    enable: bool,
    #[serde(rename = "server-url")]
    server_url: String,
    token: String,
    #[serde(rename = "realm-id")]
    realm_id: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "stun-servers")]
    stun_servers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "skip-cert-verify")]
    skip_cert_verify: Option<bool>,
}

pub async fn get_user(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = sqlx::query("SELECT id, revision FROM users WHERE id = $1")
        .bind(&user_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    let credential = sqlx::query("SELECT id, token_enc, created_at FROM subscription_credentials WHERE user_id = $1 AND revoked_at IS NULL ORDER BY created_at DESC LIMIT 1")
        .bind(&user_id).fetch_optional(&state.pool).await?;
    let active = if let Some(row) = credential {
        let token: String = row.get("token_enc");
        let token = state.secrets.decrypt(&token)?;
        Some(
            json!({"id": row.get::<String, _>("id"), "token": token, "url": subscription_url(&token), "auto_url": auto_subscription_url(&token), "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at")}),
        )
    } else {
        None
    };
    Ok(Json(
        json!({"user_id": user.get::<String, _>("id"), "revision": user.get::<i64, _>("revision"), "active": active}),
    ))
}

pub async fn rotate(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<RotateSubscription>,
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
            "user revision is {revision}; reload before rotating subscription"
        )));
    }
    let token = generate_token();
    let timestamp = now();
    sqlx::query("UPDATE subscription_credentials SET revoked_at = $1 WHERE user_id = $2 AND revoked_at IS NULL")
        .bind(timestamp).bind(&user_id).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO subscription_credentials (id, user_id, token_hash, token_enc, created_at) VALUES ($1, $2, $3, $4, $5)")
        .bind(Uuid::new_v4().to_string()).bind(&user_id).bind(token_digest(&token))
        .bind(state.secrets.encrypt(&token)?).bind(timestamp).execute(&mut *tx).await?;
    let next_revision = revision + 1;
    sqlx::query("UPDATE users SET revision = $1, updated_at = $2 WHERE id = $3 AND revision = $4")
        .bind(next_revision)
        .bind(timestamp)
        .bind(&user_id)
        .bind(revision)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'user.subscription_rotated', 'user', $2, $3, $4)")
        .bind(Uuid::new_v4().to_string()).bind(&user_id).bind(json!({"revision": next_revision})).bind(timestamp).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"user_id": user_id, "revision": next_revision, "token": token, "url": subscription_url(&token), "auto_url": auto_subscription_url(&token), "note": "The old subscription URL has been revoked."}),
    ))
}

pub async fn download(
    State(state): State<AppState>,
    Path(token): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let result = async {
        let user_id = validate_subscription(&state, &token).await?;
        // The fixed YAML endpoint keeps its version-independent node contract.
        let detected = ClientInfo::detect(
            headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
        );
        let client = ClientInfo {
            platform: formats::Platform::requested(query.as_deref())?.unwrap_or(detected.platform),
            ..ClientInfo::default()
        };
        cached_subscription(&state, &user_id, SubscriptionFormat::Mihomo, &client).await
    }
    .await;
    subscription_response(result, true)
}

pub async fn download_auto(
    State(state): State<AppState>,
    Path(token): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let result = async {
        let user_id = validate_subscription(&state, &token).await?;
        let requested = formats::requested_format(query.as_deref())?;
        let mut client = ClientInfo::detect(
            headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or(""),
        );
        if let Some(platform) = formats::Platform::requested(query.as_deref())? {
            client.platform = platform;
        }
        let format = requested.or(client.format);
        tracing::info!(client = client.name, version = ?client.version, format = ?format, "subscription format selected");
        if let Some(format) = format {
            cached_subscription(&state,&user_id,format,&client).await
        } else {
            Ok(formats::selection_page(&auto_subscription_url(&token)))
        }
    }
    .await;
    subscription_response(result, true)
}

fn subscription_response(result: Result<Response, ApiError>, automatic: bool) -> Response {
    let mut response = match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    if automatic {
        headers.insert(header::VARY, HeaderValue::from_static("User-Agent"));
    }
    response
}

async fn validate_subscription(state: &AppState, token: &str) -> Result<String, ApiError> {
    let token_hash = token_digest(token);
    let credential = sqlx::query(
        "SELECT user_id FROM subscription_credentials WHERE token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(token_hash)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("subscription"))?;
    let user_id: String = credential.get("user_id");
    let user = sqlx::query(
        "SELECT enabled, expires_at, quota_bytes, usage_bytes FROM users WHERE id = $1",
    )
    .bind(&user_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::not_found("subscription"))?;
    let enabled: bool = user.get("enabled");
    let expires_at: Option<chrono::DateTime<chrono::Utc>> = user.get("expires_at");
    let quota: Option<i64> = user.get("quota_bytes");
    let usage: i64 = user.get("usage_bytes");
    if !enabled || is_expired(expires_at) || quota.is_some_and(|limit| usage >= limit) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "subscription_inactive",
            "subscription is inactive",
        ));
    }

    Ok(user_id)
}

async fn cached_subscription(
    state: &AppState,
    user_id: &str,
    format: SubscriptionFormat,
    client: &ClientInfo,
) -> Result<Response, ApiError> {
    // Qualification is always live; the cache contains only a response for this exact eligible set.
    let rows=sqlx::query("SELECT n.id,p.config,coalesce(p.usage_bytes,0) usage_bytes,p.next_reset_at FROM node_assignments a JOIN nodes n ON n.id=a.node_id LEFT JOIN node_packages p ON p.node_id=n.id WHERE a.user_id=$1 AND n.state NOT IN ('deleting','delete_failed') AND n.mtls_state='ready' AND n.deployed_revision IS NOT NULL AND n.deployed_config_enc IS NOT NULL ORDER BY lower(n.name) COLLATE \"C\",n.name COLLATE \"C\",n.id")
        .bind(user_id).fetch_all(&state.pool).await?;
    let now = Utc::now();
    let mut eligible = Vec::<String>::new();
    for row in rows {
        let package: crate::node_limits::Package =
            serde_json::from_value(row.get::<Option<Value>, _>("config").unwrap_or(json!({})))
                .map_err(|_| ApiError::internal())?;
        let usage = if row
            .get::<Option<DateTime<Utc>>, _>("next_reset_at")
            .is_some_and(|at| at <= now)
        {
            0
        } else {
            row.get::<i64, _>("usage_bytes")
        };
        if !package.expires_at.is_some_and(|at| at <= now)
            && !package.quota_bytes.is_some_and(|quota| usage >= quota)
        {
            eligible.push(row.get("id"));
        }
    }
    let version = crate::cache_domains::version(
        &state.pool,
        &["node_config", "user_config", "credentials", "resources"],
    )
    .await?;
    let boundaries = sqlx::query("SELECT min(boundary) FILTER(WHERE boundary>$2) AS boundary,coalesce(jsonb_agg(jsonb_build_array(v.identity_id,v.credential_version,v.ca_version) ORDER BY v.identity_id,v.credential_version) FILTER(WHERE v.not_before<=$2 AND v.expires_at>$2),'[]'::jsonb) AS valid_versions FROM mtls_user_identities i JOIN mtls_user_versions v ON v.identity_id=i.id CROSS JOIN LATERAL (VALUES (v.not_before),(v.expires_at)) b(boundary) WHERE i.user_id=$1 AND NOT i.archived AND v.released AND NOT v.discarded")
        .bind(user_id).bind(now).fetch_one(&state.pool).await?;
    let boundary: Option<DateTime<Utc>> = boundaries.get("boundary");
    let key = format!(
        "subscription:v3-platform:{user_id}:{version}:{format:?}:{}:{:?}:{:?}:{}:{}",
        client.name,
        client.version,
        client.platform,
        token_digest(&eligible.join("|")),
        token_digest(&boundaries.get::<Value, _>("valid_versions").to_string())
    );
    let monotonic_now = std::time::Instant::now();
    let deadline =
        boundary.map(|at| monotonic_now + (at - Utc::now()).to_std().unwrap_or_default());
    let response = state
        .caches
        .responses
        .get_until(
            key,
            std::time::Duration::from_secs(300),
            deadline,
            false,
            || async {
                let nodes = load_subscription_nodes_eligible(state, user_id, Some(&eligible))
                    .await
                    .map_err(IntoResponse::into_response)?;
                let response = formats::render_subscription(&nodes, format, client)
                    .map_err(IntoResponse::into_response)?;
                crate::cache::CachedResponse::capture(response).await
            },
        )
        .await;
    if boundary.is_some_and(|at| at <= Utc::now()) {
        let nodes = load_subscription_nodes_eligible(state, user_id, Some(&eligible)).await?;
        return formats::render_subscription(&nodes, format, client);
    }
    Ok(match response {
        Ok(value) => value.response(),
        Err(response) => *response,
    })
}

async fn load_subscription_nodes_eligible(
    state: &AppState,
    user_id: &str,
    eligible: Option<&[String]>,
) -> Result<Vec<SubscriptionNode>, ApiError> {
    let rows = sqlx::query("SELECT n.id, n.name, n.public_host, n.public_port, n.listen_addr, n.tls_sni, n.tls_skip_verify, n.state, n.deployed_config_enc, n.published_connection, a.credential_enc, a.user_id FROM node_assignments a JOIN nodes n ON n.id = a.node_id WHERE a.user_id = $1 AND n.state NOT IN ('deleting', 'delete_failed') AND n.mtls_state='ready' AND n.deployed_revision IS NOT NULL AND n.deployed_config_enc IS NOT NULL AND ($2::text[] IS NULL OR n.id=ANY($2)) ORDER BY lower(n.name) COLLATE \"C\", n.name COLLATE \"C\", n.id")
        .bind(user_id).bind(eligible).fetch_all(&state.pool).await?;
    let projection_version =
        crate::cache_domains::version(&state.pool, &["node_config", "credentials", "resources"])
            .await?;
    let mut configs = std::collections::BTreeMap::new();
    let mut keys = std::collections::BTreeSet::new();
    for row in &rows {
        let id: String = row.get("id");
        let cipher: String = row.get("deployed_config_enc");
        let key = format!(
            "subscription-projection:{id}:{projection_version}:{}",
            token_digest(&cipher)
        );
        let config = state
            .caches
            .values
            .get(key, std::time::Duration::from_secs(300), false, || async {
                let raw: Value = serde_json::from_str(&state.secrets.decrypt(&cipher)?)
                    .map_err(|_| ApiError::internal())?;
                let mut projection = serde_json::Map::new();
                for field in ["bandwidth", "obfs", "realm", "ech", "mtls"] {
                    if let Some(value) = raw.get(field) {
                        projection.insert(field.into(), value.clone());
                    }
                }
                Ok::<_, ApiError>(Value::Object(projection))
            })
            .await?;
        if let Some(path) = config["ech"]["keyPath"].as_str() {
            let reference = crate::credentials::Reference::parse(path)
                .map_err(|_| ApiError::bad_request("invalid ECH reference"))?;
            keys.insert((reference.id, reference.version));
        }
        configs.insert(id, config);
    }
    let materials = crate::credentials::load_many(
        &state.pool,
        &state.secrets,
        &keys.into_iter().collect::<Vec<_>>(),
    )
    .await?;
    let mut nodes = Vec::new();
    for row in rows {
        let node_id: String = row.get("id");
        if eligible.is_none()
            && crate::node_limits::restricted(&state.pool, &node_id)
                .await
                .map_err(|_| ApiError::internal())?
        {
            continue;
        }
        let config = configs.get(&node_id).ok_or_else(ApiError::internal)?;
        let ech_config = subscription_ech_from_loaded(config, &materials)?;
        let realm_opts = subscription_realm_options(config)?;
        let password_enc: String = row.get("credential_enc");
        let identity = if config["mtls"]["mode"] == "managed" {
            let ca = config["mtls"]["ca_credential_id"]
                .as_str()
                .ok_or_else(ApiError::internal)?;
            let Some(material) = crate::credentials::identities::for_node(
                &state.pool,
                &state.secrets,
                user_id,
                &node_id,
                ca,
            )
            .await?
            else {
                continue;
            };
            Some(material)
        } else {
            None
        };
        let (client_certificate, client_private_key) = identity
            .map(|(c, k)| (Some(c), Some(k)))
            .unwrap_or((None, None));
        let name: String = row.get("name");
        // Legacy/test rows without a snapshot retain their existing connection values.
        let connection: Value = row.get::<Option<Value>, _>("published_connection").unwrap_or_else(|| json!({
            "public_host": row.get::<String,_>("public_host"), "public_port": row.get::<i32,_>("public_port"),
            "listen_addr": row.get::<String,_>("listen_addr"), "tls_sni": row.get::<Option<String>,_>("tls_sni"),
            "tls_skip_verify": row.get::<bool,_>("tls_skip_verify")}));
        let host = connection["public_host"]
            .as_str()
            .ok_or_else(ApiError::internal)?;
        let port = connection["public_port"]
            .as_u64()
            .filter(|p| (1..=65535).contains(p))
            .ok_or_else(ApiError::internal)? as u16;
        let listen = connection["listen_addr"]
            .as_str()
            .ok_or_else(ApiError::internal)?;
        nodes.push(build_node(
            &name,
            &node_id,
            host,
            port,
            crate::config::listener_hop_ports(listen).map(str::to_owned),
            connection["tls_sni"].as_str().map(str::to_owned),
            connection["tls_skip_verify"].as_bool().unwrap_or(false),
            &state.secrets.decrypt(&password_enc)?,
            client_certificate,
            client_private_key,
            ech_config,
            realm_opts,
            config,
        ));
    }

    Ok(nodes)
}

#[cfg(test)]
async fn load_subscription_nodes(
    state: &AppState,
    user_id: &str,
) -> Result<Vec<SubscriptionNode>, ApiError> {
    load_subscription_nodes_eligible(state, user_id, None).await
}

#[cfg(test)]
pub(crate) async fn subscription_node_count(
    state: &AppState,
    user_id: &str,
) -> Result<usize, ApiError> {
    Ok(load_subscription_nodes(state, user_id).await?.len())
}

pub async fn hy2_auth(
    State(state): State<AppState>,
    Path((node_id, node_token)): Path<(String, String)>,
    Json(input): Json<AuthRequest>,
) -> Result<Json<AuthResponse>, ApiError> {
    let node = sqlx::query("SELECT node_token_hash, state,mtls_state FROM nodes WHERE id = $1")
        .bind(&node_id)
        .fetch_optional(&state.pool)
        .await?;
    let Some(node) = node else {
        return Ok(Json(AuthResponse::denied()));
    };
    let node_state: String = node.get("state");
    if matches!(node_state.as_str(), "deleting" | "delete_failed") {
        return Ok(Json(AuthResponse::denied()));
    }
    let expected: String = node.get("node_token_hash");
    let provided = token_digest(&node_token);
    if expected.len() != provided.len()
        || !bool::from(expected.as_bytes().ct_eq(provided.as_bytes()))
    {
        return Ok(Json(AuthResponse::denied()));
    }
    if input.auth.len() > 1024 {
        return Ok(Json(AuthResponse::denied()));
    }

    let auth_hash = token_digest(&input.auth);
    let probe_hash: Option<String> = sqlx::query_scalar(
        "SELECT p.token_hash FROM deployment_probe_tokens p JOIN jobs j ON j.id=p.job_id JOIN nodes n ON n.id=p.node_id WHERE p.node_id=$1 AND p.expires_at>$2 AND j.status='running' AND j.node_id=p.node_id AND j.kind IN ('deploy','sync','rollback') AND (n.mtls_state='ready' OR EXISTS(SELECT 1 FROM mtls_activations a WHERE a.id=n.mtls_activation_id AND a.job_id=j.id AND a.phase IN ('activating','verified','recovery_required')))",
    )
    .bind(&node_id)
    .bind(now())
    .fetch_optional(&state.pool)
    .await?;
    if probe_hash.is_some_and(|expected| {
        expected.len() == auth_hash.len()
            && bool::from(expected.as_bytes().ct_eq(auth_hash.as_bytes()))
    }) {
        return Ok(Json(AuthResponse {
            ok: true,
            id: format!("probe-{node_id}"),
        }));
    }
    if node.get::<String, _>("mtls_state") != "ready" {
        return Ok(Json(AuthResponse::denied()));
    }
    let monitor_hash: Option<String> = sqlx::query_scalar(
        "SELECT token_hash FROM monitoring_probe_tokens WHERE node_id = $1 AND expires_at > now()",
    )
    .bind(&node_id)
    .fetch_optional(&state.pool)
    .await?;
    if monitor_hash.is_some_and(|expected| {
        expected.len() == auth_hash.len()
            && bool::from(expected.as_bytes().ct_eq(auth_hash.as_bytes()))
    }) {
        return Ok(Json(AuthResponse {
            ok: true,
            id: format!("monitor-{node_id}"),
        }));
    }
    if crate::node_limits::restricted(&state.pool, &node_id)
        .await
        .map_err(|_| ApiError::internal())?
    {
        return Ok(Json(AuthResponse::denied()));
    }
    let assignment = sqlx::query("SELECT u.id, u.enabled, u.expires_at, u.quota_bytes, u.usage_bytes FROM node_assignments a JOIN users u ON u.id = a.user_id WHERE a.node_id = $1 AND a.credential_hash = $2")
        .bind(&node_id).bind(auth_hash).fetch_optional(&state.pool).await?;
    let Some(assignment) = assignment else {
        return Ok(Json(AuthResponse::denied()));
    };
    let enabled: bool = assignment.get("enabled");
    let expires_at: Option<chrono::DateTime<chrono::Utc>> = assignment.get("expires_at");
    let quota: Option<i64> = assignment.get("quota_bytes");
    let usage: i64 = assignment.get("usage_bytes");
    if !enabled || is_expired(expires_at) || quota.is_some_and(|limit| usage >= limit) {
        return Ok(Json(AuthResponse::denied()));
    }
    Ok(Json(AuthResponse {
        ok: true,
        id: assignment.get("id"),
    }))
}

#[derive(Deserialize)]
pub struct AuthRequest {
    #[allow(dead_code)]
    addr: String,
    auth: String,
    #[allow(dead_code)]
    tx: u64,
}

#[derive(Serialize)]
pub struct AuthResponse {
    ok: bool,
    id: String,
}

impl AuthResponse {
    fn denied() -> Self {
        Self {
            ok: false,
            id: String::new(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_node(
    name: &str,
    id: &str,
    server: &str,
    port: u16,
    ports: Option<String>,
    sni: Option<String>,
    skip_cert_verify: bool,
    password: &str,
    certificate: Option<String>,
    private_key: Option<String>,
    ech_config: Option<String>,
    realm_opts: Option<SubscriptionRealm>,
    config: &Value,
) -> SubscriptionNode {
    let suffix = id.split('-').next().unwrap_or(id);
    let hop_interval = ports.as_ref().map(|_| 30);
    let bandwidth = config.get("bandwidth").and_then(Value::as_object);
    let obfs = config.get("obfs").and_then(Value::as_object);
    let obfs_type = obfs
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let obfs_password = obfs_type
        .as_ref()
        .and_then(|kind| obfs.and_then(|value| value.get(kind)))
        .and_then(Value::as_object)
        .and_then(|value| value.get("password"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let obfs_details = obfs_type
        .as_ref()
        .and_then(|kind| obfs.and_then(|value| value.get(kind)))
        .and_then(Value::as_object);
    SubscriptionNode {
        name: format!("{name}-{suffix}"),
        server: server.to_owned(),
        port,
        ports,
        hop_interval,
        password: password.to_owned(),
        sni,
        certificate,
        private_key,
        skip_cert_verify,
        up: bandwidth
            .and_then(|value| value.get("up"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        down: bandwidth
            .and_then(|value| value.get("down"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        obfs: obfs_type,
        obfs_password,
        obfs_min_packet_size: obfs_details
            .and_then(|value| value.get("minPacketSize"))
            .and_then(Value::as_u64),
        obfs_max_packet_size: obfs_details
            .and_then(|value| value.get("maxPacketSize"))
            .and_then(Value::as_u64),
        ech_config,
        realm: realm_opts,
    }
}

fn subscription_realm_options(config: &Value) -> Result<Option<SubscriptionRealm>, ApiError> {
    let connection = crate::config::realm_connection(config)
        .map_err(|_| ApiError::bad_request("stored Realm configuration is invalid"))?;
    Ok(connection.map(|connection| SubscriptionRealm {
        server_url: connection.server_url,
        token: connection.token,
        realm_id: connection.realm_id,
        stun_servers: (!connection.stun_servers.is_empty()).then_some(connection.stun_servers),
        skip_cert_verify: connection.insecure.then_some(true),
    }))
}

fn subscription_ech_from_loaded(
    config: &Value,
    materials: &std::collections::BTreeMap<(String, i64), crate::credentials::LoadedCredential>,
) -> Result<Option<String>, ApiError> {
    let Some(path) = config["ech"]["keyPath"].as_str() else {
        return Ok(None);
    };
    let reference = crate::credentials::Reference::parse(path)
        .map_err(|_| ApiError::bad_request("invalid ECH reference"))?;
    let (kind, owner, _, payload, _) = materials
        .get(&(reference.id, reference.version))
        .ok_or_else(ApiError::internal)?;
    if kind != "ech_key" || owner.is_some() || reference.field != "content" {
        return Err(ApiError::bad_request("invalid ECH credential"));
    }
    Ok(Some(extract_ech_config_list(
        payload["content"].as_str().ok_or_else(ApiError::internal)?,
    )?))
}

pub(crate) fn extract_ech_config_list(pem: &str) -> Result<String, ApiError> {
    const BEGIN: &str = "-----BEGIN ECH CONFIGS-----";
    const END: &str = "-----END ECH CONFIGS-----";
    let body = pem
        .split_once(BEGIN)
        .and_then(|(_, tail)| tail.split_once(END).map(|(body, _)| body))
        .ok_or_else(|| ApiError::bad_request("ECH key resource has no ECH CONFIGS block"))?;
    let encoded: String = body
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if encoded.is_empty() || STANDARD.decode(encoded.as_bytes()).is_err() {
        return Err(ApiError::bad_request(
            "ECH key resource contains an invalid ECH CONFIGS block",
        ));
    }
    Ok(encoded)
}

fn is_expired(value: Option<DateTime<Utc>>) -> bool {
    value.is_some_and(|expiration| expiration <= Utc::now())
}

fn auto_subscription_url(token: &str) -> String {
    let base =
        std::env::var("HYSTERIAX_PUBLIC_URL").unwrap_or_else(|_| "https://localhost".to_owned());
    format!("{}/sub/{token}", base.trim_end_matches('/'))
}

fn subscription_url(token: &str) -> String {
    format!("{}/clash.yaml", auto_subscription_url(token))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{build_node, extract_ech_config_list, subscription_realm_options};

    #[test]
    fn serializes_special_node_names_ipv6_and_port_hopping_as_yaml_strings() {
        let proxy = build_node(
            "東京: edge",
            "7ed8a6e1-1234-5678-9abc-def012345678",
            "2001:db8::1",
            443,
            Some("443,445-446".to_owned()),
            Some("edge.example".to_owned()),
            false,
            "pass:word/with?symbols",
            None,
            None,
            None,
            None,
            &json!({}),
        );
        let yaml = serde_yaml::to_string(&proxy.to_mihomo()).unwrap();
        let value: serde_yaml::Value = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(value["name"].as_str(), Some("東京: edge-7ed8a6e1"));
        assert_eq!(value["server"].as_str(), Some("2001:db8::1"));
        assert_eq!(value["password"].as_str(), Some("pass:word/with?symbols"));
        assert_eq!(value["ports"].as_str(), Some("443,445-446"));
        assert!(value.get("handshake-timeout").is_none());
    }

    #[test]
    fn extracts_the_public_ech_config_list_from_the_uploaded_key_file() {
        let pem = "-----BEGIN ECH KEYS-----\nprivate-key\n-----END ECH KEYS-----\n-----BEGIN ECH CONFIGS-----\nQUJD\n-----END ECH CONFIGS-----\n";
        assert_eq!(extract_ech_config_list(pem).unwrap(), "QUJD");
        assert!(extract_ech_config_list("-----BEGIN ECH KEYS-----\nprivate\n").is_err());
        assert!(
            extract_ech_config_list(
                "-----BEGIN ECH CONFIGS-----\nnot-base64!\n-----END ECH CONFIGS-----"
            )
            .is_err()
        );
    }

    #[test]
    fn maps_realm_registration_secrets_to_mihomo_realm_options() {
        let config = json!({
            "realm": {
                "connection": {
                    "serverURL": "https://rendezvous.example",
                    "token": "realm-token",
                    "realmID": "node-realm-1"
                },
                "insecure": true,
                "stunServers": ["stun.example:3478"]
            }
        });
        let realm = subscription_realm_options(&config).unwrap().unwrap();
        let yaml = serde_yaml::to_string(&realm.to_mihomo()).unwrap();
        assert!(yaml.contains("server-url: https://rendezvous.example"));
        assert!(yaml.contains("token: realm-token"));
        assert!(yaml.contains("realm-id: node-realm-1"));
        assert!(yaml.contains("stun-servers:"));
        assert!(yaml.contains("skip-cert-verify: true"));

        let proxy = build_node(
            "Realm node",
            "node-realm-id",
            "node.example",
            443,
            None,
            Some("node.example".to_owned()),
            false,
            "user-pass",
            None,
            None,
            None,
            Some(realm),
            &config,
        );
        let proxy_yaml = serde_yaml::to_string(&proxy.to_mihomo()).unwrap();
        let proxy_value: serde_yaml::Value = serde_yaml::from_str(&proxy_yaml).unwrap();
        assert_eq!(proxy_value["handshake-timeout"].as_u64(), Some(30));
    }
}
