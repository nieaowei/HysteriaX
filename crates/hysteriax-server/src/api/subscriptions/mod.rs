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

use crate::{error::ApiError, security::token_digest, state::AppState};

mod formats;
pub(crate) mod templates;
use formats::{ClientInfo, SubscriptionFormat, SubscriptionNode, SubscriptionRealm};

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
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let user = sqlx::query("SELECT id, revision FROM users WHERE id = $1")
        .bind(&user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    let credential = sqlx::query("SELECT id, secret_enc AS token_enc, created_at FROM user_credentials WHERE kind='subscription' AND user_id = $1 AND revoked_at IS NULL ORDER BY created_at DESC,id DESC LIMIT 1")
        .bind(&user_id).fetch_optional(&mut *tx).await?;
    tx.commit().await?;
    let active = if let Some(row) = credential {
        let token: String = row.get("token_enc");
        let token = state.secrets.decrypt(&token)?;
        Some(
            json!({"id": row.get::<String, _>("id"), "token": token, "url": auto_subscription_url(&token), "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at")}),
        )
    } else {
        None
    };
    Ok(Json(
        json!({"user_id": user.get::<String, _>("id"), "revision": user.get::<i64, _>("revision"), "active": active}),
    ))
}

pub async fn download_auto(
    State(state): State<AppState>,
    Path(token): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let result = crate::read_snapshot::scope(&state.pool, async {
        let user_id = validate_subscription(&state, &token).await?;
        let requested = formats::requested_format(query.as_deref())?;
        let mut client = ClientInfo::detect(
            headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
        );
        let requested_platform = formats::Platform::requested(query.as_deref())?;
        if let Some(platform) = requested_platform {
            client.platform = platform;
        }
        let templates: Vec<_> =
            url::form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes())
                .filter(|(key, _)| key == "template")
                .map(|(_, v)| v.into_owned())
                .collect();
        if templates.len() > 1 || templates.first().is_some_and(|v| v.is_empty()) {
            return Err(ApiError::bad_request(
                "template must occur once and be nonempty",
            ));
        }
        let id = templates.first().map(String::as_str);
        if requested
            .is_some_and(|f| matches!(f, SubscriptionFormat::Uri | SubscriptionFormat::Base64))
            && id.is_some()
        {
            return Err(ApiError::bad_request(
                "URI/Base64 cannot be combined with a template",
            ));
        }
        let format = requested.or(client.format);
        if matches!(
            format,
            Some(SubscriptionFormat::Uri | SubscriptionFormat::Base64)
        ) && id.is_none()
        {
            return cached_template(&state, &user_id, format.unwrap(), &client, None).await;
        }
        if id.is_none() && format.is_none() {
            // The browser's platform identifies the eventual client target, but the
            // selection page must still expose every platform unless the URL filters it.
            return template_selection(
                &state,
                &token,
                None,
                requested_platform.unwrap_or_default(),
                client.platform,
            )
            .await;
        }
        let format_name = match format {
            Some(SubscriptionFormat::Mihomo) => Some("mihomo"),
            Some(SubscriptionFormat::Singbox) => Some("singbox"),
            _ => None,
        };
        let published = templates::published(&state, id, format_name).await?;
        let selected = if published.2 == "mihomo" {
            SubscriptionFormat::Mihomo
        } else {
            SubscriptionFormat::Singbox
        };
        if requested.is_some_and(|f| f != selected) {
            return Err(ApiError::bad_request("format conflicts with template"));
        }
        if templates::platform_name(client.platform).is_none() {
            return template_selection(
                &state,
                &token,
                Some(&published.0),
                client.platform,
                client.platform,
            )
            .await;
        }
        cached_template(&state, &user_id, selected, &client, Some(&published)).await
    })
    .await;
    subscription_response(result, true)
}
async fn template_selection(
    state: &AppState,
    token: &str,
    id: Option<&str>,
    filter_platform: formats::Platform,
    default_platform: formats::Platform,
) -> Result<Response, ApiError> {
    fn escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\"', "&quot;")
            .replace('\'', "&#39;")
    }
    let rows=sqlx::query("SELECT id,name,format FROM subscription_templates WHERE NOT archived AND published_version IS NOT NULL AND ($1::text IS NULL OR id=$1) ORDER BY name,id").bind(id).fetch_all(&mut *crate::read_snapshot::begin(&state.pool).await?).await?;
    let mut cards = String::new();
    let default_target = templates::platform_name(default_platform);
    let mut targets = Vec::new();
    if let Some(target) = default_target {
        targets.push(target);
    }
    targets.extend(
        templates::document::PLATFORMS
            .into_iter()
            .filter(|target| Some(*target) != default_target),
    );
    let initial_target = templates::platform_name(filter_platform)
        .or(default_target)
        .or_else(|| targets.first().copied());
    let mut platform_tabs = String::new();
    for target in &targets {
        if templates::platform_name(filter_platform).is_some_and(|p| p != *target) {
            continue;
        }
        let platform_name = if *target == "linux" {
            "Linux/OpenWrt"
        } else {
            *target
        };
        let recommended = default_target == Some(*target);
        let active = initial_target == Some(*target);
        let badge = if recommended {
            "<span class=\"platform-badge\">推荐</span>"
        } else {
            ""
        };
        platform_tabs.push_str(&format!(
            "<button type=\"button\" class=\"platform-tab{}\" data-platform-tab=\"{}\" aria-selected=\"{}\">{} {}</button>",
            if active { " active" } else { "" },
            escape(target),
            active,
            escape(platform_name),
            badge,
        ));
    }
    for row in rows {
        let name: String = row.get("name");
        let format: String = row.get("format");
        let file_kind = match format.as_str() {
            "mihomo" => ".yaml",
            "singbox" => ".json",
            other => other,
        };
        let description = match format.as_str() {
            "mihomo" => "适用于 Mihomo / Clash.Meta 内核客户端。",
            "singbox" => "适用于 sing-box 客户端。",
            _ => "适用于对应模板客户端。",
        };
        let compatibility = match format.as_str() {
            "mihomo" => {
                "<p>高级功能请使用项目验证版本 v1.19.31 或更新版本。普通 Clash 不支持全部功能。</p>"
            }
            "singbox" => {
                "<p>mTLS 需要 1.13+，Gecko / Realm 需要 1.14+，ECH 使用验证版本 1.14.2+。版本未知时提供全部可表示节点，并保留完整高级配置。</p><p>已识别的旧版内核仍按兼容性过滤。请确认实际内核满足所用功能的最低版本要求。</p>"
            }
            _ => "<p>请使用对应格式的客户端，并确认客户端支持模板中的功能。</p>",
        };
        cards.push_str(&format!(
            "<article><div class=\"card-top\"><span class=\"file-kind\">{}</span><span class=\"category\">完整配置</span></div><h2>{} <span class=\"category\">({})</span></h2><p class=\"description\">{}<br>复制对应目标平台的订阅地址。</p><div class=\"addresses\">",
            escape(file_kind),
            escape(&name),
            escape(&format),
            escape(description),
        ));
        for target in &targets {
            if templates::platform_name(filter_platform).is_some_and(|p| p != *target) {
                continue;
            }
            let url = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("template", &row.get::<String, _>("id"))
                .append_pair("format", &row.get::<String, _>("format"))
                .append_pair("platform", target)
                .finish();
            let url = format!("{}?{url}", auto_subscription_url(token));
            let platform_name = if *target == "linux" {
                "Linux/OpenWrt"
            } else {
                *target
            };
            let recommended = default_target == Some(*target);
            let platform_class = if recommended {
                "platform recommended"
            } else {
                "platform"
            };
            let badge = if recommended {
                "<span class=\"platform-badge\">推荐</span>"
            } else {
                ""
            };
            let hidden = if initial_target == Some(*target) {
                ""
            } else {
                " hidden"
            };
            cards.push_str(&format!(
                "<div class=\"address\" data-platform=\"{}\"{}><div class=\"{}\">{} {}</div><label class=\"address-label\">订阅地址</label><input aria-label=\"{} 订阅地址\" readonly spellcheck=\"false\" value=\"{}\"><div class=\"links\"><button type=\"button\" data-copy-address aria-label=\"复制 {} 订阅地址\">复制地址</button><a href=\"{}\">查看订阅<span class=\"arrow\" aria-hidden=\"true\">↗</span></a></div></div>",
                escape(target),
                hidden,
                platform_class,
                escape(platform_name),
                badge,
                escape(platform_name),
                escape(&url),
                escape(platform_name),
                escape(&url),
            ));
        }
        cards.push_str(&format!(
            "</div><details><summary>兼容性说明</summary>{}</details></article>",
            compatibility,
        ));
    }
    let mut node_section = String::new();
    if id.is_none() {
        node_section
            .push_str("<section class=\"formats node-formats\" aria-label=\"节点订阅格式\">");
        for (kind, category, title, description, compatibility) in [
            (
                "Base64",
                "通用订阅",
                "Base64 节点订阅",
                "适用于支持 Hysteria2 的 Shadowrocket、<br>v2rayN、v2rayNG 等客户端。",
                "仅包含节点链接，不包含完整路由配置。mTLS、ECH、端口跳跃、Gecko 和 Realm 节点会被过滤。",
            ),
            (
                "URI",
                "手动导入",
                "纯文本节点链接",
                "每行一个 hysteria2:// 链接。<br>适合手动导入或与其他工具配合使用。",
                "链接列表采用基础兼容配置。mTLS、ECH、端口跳跃、Gecko 和 Realm 节点会被过滤。",
            ),
        ] {
            let format = kind.to_ascii_lowercase();
            let url = format!("{}?format={format}", auto_subscription_url(token));
            node_section.push_str(&format!(
                "<article><div class=\"card-top\"><span class=\"file-kind\">{}</span><span class=\"category\">{}</span></div><h2>{}</h2><p class=\"description\">{}</p><div class=\"address\"><label class=\"address-label\">订阅地址</label><input aria-label=\"{} 订阅地址\" readonly spellcheck=\"false\" value=\"{}\"><div class=\"links\"><button type=\"button\" data-copy-address aria-label=\"复制 {} 订阅地址\">复制地址</button><a href=\"{}\">查看订阅<span class=\"arrow\" aria-hidden=\"true\">↗</span></a></div></div><details><summary>兼容性说明</summary><p>{}</p></details></article>",
                escape(kind),
                escape(category),
                escape(title),
                description,
                escape(kind),
                escape(&url),
                escape(kind),
                escape(&url),
                escape(compatibility),
            ));
        }
        node_section.push_str("</section>");
    }
    let html = include_str!("selection.html")
        .replace("{{PLATFORM_TABS}}", &platform_tabs)
        .replace("{{TEMPLATE_CARDS}}", &cards)
        .replace("{{NODE_SECTION}}", &node_section);
    Ok(([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
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
        "SELECT user_id FROM user_credentials WHERE kind='subscription' AND secret_hash = $1 AND revoked_at IS NULL",
    )
    .bind(token_hash)
    .fetch_optional(&mut *crate::read_snapshot::begin(&state.pool).await?)
    .await?
    .ok_or_else(|| ApiError::not_found("subscription"))?;
    let user_id: String = credential.get("user_id");
    validate_user(state, &user_id).await?;
    Ok(user_id)
}

async fn validate_user(state: &AppState, user_id: &str) -> Result<(), ApiError> {
    let user = sqlx::query(
        "SELECT enabled, expires_at, quota_bytes, usage_bytes FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&mut *crate::read_snapshot::begin(&state.pool).await?)
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

    Ok(())
}

async fn eligible_nodes(state: &AppState, user_id: &str) -> Result<Vec<String>, ApiError> {
    let rows=sqlx::query("SELECT n.id,p.config,coalesce(p.usage_bytes,0) usage_bytes,p.next_reset_at FROM node_assignments a JOIN nodes n ON n.id=a.node_id LEFT JOIN node_packages p ON p.node_id=n.id WHERE a.user_id=$1 AND n.state NOT IN ('deleting','delete_failed') AND n.mtls_state='ready' AND n.deployed_revision IS NOT NULL AND n.deployed_config_enc IS NOT NULL ORDER BY lower(n.name) COLLATE \"C\",n.name COLLATE \"C\",n.id")
        .bind(user_id).fetch_all(&mut *crate::read_snapshot::begin(&state.pool).await?).await?;
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
    Ok(eligible)
}

async fn cached_template(
    state: &AppState,
    user_id: &str,
    format: SubscriptionFormat,
    client: &ClientInfo,
    template: Option<&(String, i64, String, templates::document::Document)>,
) -> Result<Response, ApiError> {
    if let Some((_, _, _, document)) = template {
        document.effective(
            templates::platform_name(client.platform)
                .ok_or_else(|| ApiError::bad_request("select a target platform"))?,
        )?;
    }
    let eligible = eligible_nodes(state, user_id).await?;
    let now = Utc::now();
    let version = crate::cache_domains::version(
        &state.pool,
        &["node_config", "user_config", "credentials", "resources"],
    )
    .await?;
    let boundaries = sqlx::query("SELECT min(boundary) FILTER(WHERE boundary>$2) AS boundary,coalesce(jsonb_agg(jsonb_build_array(v.identity_id,v.credential_version,v.ca_version) ORDER BY v.identity_id,v.credential_version) FILTER(WHERE v.not_before<=$2 AND v.expires_at>$2),'[]'::jsonb) AS valid_versions FROM mtls_user_identities i JOIN mtls_user_version_publication v ON v.identity_id=i.id CROSS JOIN LATERAL (VALUES (v.not_before),(v.expires_at)) b(boundary) WHERE i.user_id=$1 AND NOT i.archived AND v.released AND NOT v.discarded")
        .bind(user_id).bind(now).fetch_one(&mut *crate::read_snapshot::begin(&state.pool).await?).await?;
    let boundary: Option<DateTime<Utc>> = boundaries.get("boundary");
    let key = format!(
        "subscription:v4-template:{user_id}:{version}:{format:?}:{}:{:?}:{:?}:{}:{}:{:?}",
        client.name,
        client.version,
        client.platform,
        token_digest(&eligible.join("|")),
        token_digest(&boundaries.get::<Value, _>("valid_versions").to_string()),
        template.map(|t| (&t.0, t.1))
    );
    if boundary.is_some_and(|at| at <= Utc::now()) {
        let nodes = load_subscription_nodes_eligible(state, user_id, Some(&eligible)).await?;
        return if let Some((id, _, format, document)) = template {
            templates::response_from_nodes(id, document, format, &nodes, client)
        } else {
            formats::render_subscription(&nodes, format, client)
        };
    }
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
                let response = if let Some((id, _, format, document)) = template {
                    templates::response_from_nodes(id, document, format, &nodes, client)
                } else {
                    formats::render_subscription(&nodes, format, client)
                }
                .map_err(IntoResponse::into_response)?;
                crate::cache::CachedResponse::capture(response).await
            },
        )
        .await;
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
    crate::read_snapshot::scope(
        &state.pool,
        load_subscription_nodes_snapshot(state, user_id, eligible),
    )
    .await
}
async fn load_subscription_nodes_snapshot(
    state: &AppState,
    user_id: &str,
    eligible: Option<&[String]>,
) -> Result<Vec<SubscriptionNode>, ApiError> {
    let rows = sqlx::query("SELECT n.id, n.name, n.public_host, n.public_port, n.listen_addr, n.tls_sni, n.tls_skip_verify, n.state, n.deployed_config_enc, n.published_connection, c.secret_enc AS credential_enc, a.user_id FROM node_assignments a JOIN user_credentials c ON c.user_id=a.user_id AND c.kind='connection' AND c.revoked_at IS NULL JOIN nodes n ON n.id = a.node_id WHERE hysteriax_authorized(a.user_id,a.node_id) AND a.user_id = $1 AND n.state NOT IN ('deleting', 'delete_failed') AND n.mtls_state='ready' AND n.deployed_revision IS NOT NULL AND n.deployed_config_enc IS NOT NULL AND ($2::text[] IS NULL OR n.id=ANY($2)) ORDER BY lower(n.name) COLLATE \"C\", n.name COLLATE \"C\", n.id")
        .bind(user_id).bind(eligible).fetch_all(&mut *crate::read_snapshot::begin(&state.pool).await?).await?;
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
    let mut mtls_targets = Vec::new();
    for row in &rows {
        let node_id: &str = row.get("id");
        if let Some(config) = configs.get(node_id)
            && config["mtls"]["mode"] == "managed"
        {
            let ca = config["mtls"]["ca_credential_id"]
                .as_str()
                .ok_or_else(ApiError::internal)?;
            mtls_targets.push((node_id.to_owned(), ca.to_owned()));
        }
    }
    let mtls_identities =
        crate::credentials::identities::for_nodes(&state.pool, &state.secrets, user_id, &mtls_targets)
            .await?;
    let mut seen_names = std::collections::HashSet::with_capacity(rows.len());
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
            let Some(material) = mtls_identities.get(&(node_id.clone(), ca.to_owned())).cloned()
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
        let mut node = build_node(
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
        );
        if !seen_names.insert(node.name.clone()) {
            node.name = format!("{name}:{node_id}");
            seen_names.insert(node.name.clone());
        }
        nodes.push(node);
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
    let mtls_ready = node.get::<String, _>("mtls_state") == "ready";
    if mtls_ready
        && !crate::node_limits::restricted(&state.pool, &node_id)
            .await
            .map_err(|_| ApiError::internal())?
    {
        let assignment = sqlx::query("SELECT u.id, u.enabled, u.expires_at, u.quota_bytes, u.usage_bytes FROM node_assignments a JOIN users u ON u.id = a.user_id JOIN user_credentials c ON c.user_id=u.id AND c.kind='connection' AND c.revoked_at IS NULL WHERE a.node_id = $1 AND c.secret_hash = $2 AND hysteriax_authorized(u.id,a.node_id)")
            .bind(&node_id).bind(&auth_hash).fetch_optional(&state.pool).await?;
        if let Some(assignment) = assignment {
            let enabled: bool = assignment.get("enabled");
            let expires_at: Option<chrono::DateTime<chrono::Utc>> = assignment.get("expires_at");
            let quota: Option<i64> = assignment.get("quota_bytes");
            let usage: i64 = assignment.get("usage_bytes");
            if !enabled || is_expired(expires_at) || quota.is_some_and(|limit| usage >= limit) {
                return Ok(Json(AuthResponse::denied()));
            }
            return Ok(Json(AuthResponse {
                ok: true,
                id: assignment.get("id"),
            }));
        }
    }
    let probe_hash: Option<String> = sqlx::query_scalar(
        "SELECT p.token_hash FROM deployment_probe_tokens p JOIN jobs j ON j.id=p.job_id JOIN nodes n ON n.id=p.node_id JOIN resource_leases nl ON nl.resource_key='node:'||p.node_id AND nl.work_id=j.id AND nl.owner=p.owner AND nl.generation=p.node_generation AND nl.epoch=p.epoch AND NOT nl.blocked AND nl.expires_at>clock_timestamp() JOIN resource_leases jl ON jl.resource_key='job:'||j.id AND jl.work_id=j.id AND jl.owner=p.owner AND jl.generation=p.job_generation AND jl.epoch=p.epoch AND NOT jl.blocked AND jl.expires_at>clock_timestamp() WHERE p.node_id=$1 AND p.epoch=hysteriax_execution_epoch() AND p.expires_at>clock_timestamp() AND j.status='running' AND j.node_id=p.node_id AND j.kind IN ('deploy','sync','rollback') AND (n.mtls_state='ready' OR EXISTS(SELECT 1 FROM mtls_activations a WHERE a.id=n.mtls_activation_id AND a.generation=n.mtls_generation AND a.job_id=j.id AND a.phase IN ('activating','verified','recovery_required')))",
    )
    .bind(&node_id)
    .fetch_optional(&state.pool)
    .await?;
    if probe_hash.is_some_and(|expected| {
        expected.len() == auth_hash.len()
            && bool::from(expected.as_bytes().ct_eq(auth_hash.as_bytes()))
    }) {
        return Ok(Json(AuthResponse {
            ok: true,
            id: crate::deployment::probe_tokens::identity(&node_id, &auth_hash),
        }));
    }
    if !mtls_ready {
        return Ok(Json(AuthResponse::denied()));
    }
    let monitor_hash: Option<String> = sqlx::query_scalar(
        "SELECT p.token_hash FROM monitoring_probe_tokens p JOIN background_work w ON w.id=p.work_id AND w.node_id=p.node_id AND w.kind='proxy' AND w.state='running' AND w.owner=p.owner AND w.generation=p.work_generation JOIN nodes n ON n.id=p.node_id AND n.deployed_revision=(w.payload->>'deployed_revision')::bigint AND n.mtls_state='ready' AND n.state NOT IN ('deleting','delete_failed') JOIN resource_leases nl ON nl.resource_key='node:'||p.node_id AND nl.work_id=w.id AND nl.owner=p.owner AND nl.generation=p.node_generation AND nl.epoch=p.epoch AND NOT nl.blocked AND nl.expires_at>clock_timestamp() JOIN resource_leases wl ON wl.resource_key='work:'||w.id AND wl.work_id=w.id AND wl.owner=p.owner AND wl.generation=p.work_generation AND wl.epoch=p.epoch AND NOT wl.blocked AND wl.expires_at>clock_timestamp() WHERE p.node_id=$1 AND p.epoch=hysteriax_execution_epoch() AND p.expires_at>clock_timestamp()",
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
            id: crate::proxy_monitor::identity(&node_id, &auth_hash),
        }));
    }
    Ok(Json(AuthResponse::denied()))
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
        id: id.to_owned(),
        display_name: name.to_owned(),
        name: format!("{name}:{}", id.chars().take(8).collect::<String>()),
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

pub(crate) fn auto_subscription_url(token: &str) -> String {
    let base =
        std::env::var("HYSTERIAX_PUBLIC_URL").unwrap_or_else(|_| "https://localhost".to_owned());
    format!("{}/sub/{token}", base.trim_end_matches('/'))
}

#[cfg(test)]
mod tests;
