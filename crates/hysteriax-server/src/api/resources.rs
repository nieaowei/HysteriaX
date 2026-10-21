use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    path::Path,
};

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Digest;
use sqlx::Row;
use uuid::Uuid;

use crate::{api::now, db, error::ApiError, state::AppState};

const MAX_RESOURCE_BYTES: usize = 20 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct ResourceFile {
    pub id: String,
    pub content: Vec<u8>,
}

pub(crate) async fn resolve_config_resources(
    pool: &sqlx::PgPool,
    secrets: &crate::security::SecretBox,
    node_id: &str,
    config: &Value,
) -> Result<(Value, Vec<ResourceFile>), ApiError> {
    let (mut resolved, mut resources) =
        crate::credentials::resolve_config(pool, secrets, config, false)
            .await
            .map_err(|_| {
                ApiError::bad_request("invalid or unavailable configuration credential")
            })?;
    let ids = collect_resource_ids(&resolved);
    if !ids.is_empty() {
        let rows = sqlx::query(
            "SELECT id, resource_kind, content_enc FROM config_resources WHERE node_id=$1 AND id=ANY($2)",
        )
        .bind(node_id)
        .bind(ids.iter().cloned().collect::<Vec<_>>())
        .fetch_all(&mut *crate::read_snapshot::begin(pool).await?)
        .await?;
        let mut kinds = BTreeMap::new();
        for row in rows {
            let id: String = row.get("id");
            if ids.contains(&id) {
                kinds.insert(id.clone(), row.get::<String, _>("resource_kind"));
                resources.push(ResourceFile {
                    id,
                    content: secrets.decrypt_bytes(&row.get::<String, _>("content_enc"))?,
                });
            }
        }
        if ids.iter().any(|id| !kinds.contains_key(id)) {
            return Err(ApiError::bad_request(
                "configuration references a missing resource for this node",
            ));
        }
        validate_resource_kinds(&resolved, &kinds)?;
        validate_server_tls_resource_pair(&resolved, &resources).await?;
        rewrite_resource_paths(&mut resolved);
    }
    if let Some(tls) = resolved.get("tls").and_then(Value::as_object) {
        let file = |field: &str| {
            tls.get(field)
                .and_then(Value::as_str)
                .and_then(|p| p.strip_prefix("/etc/hysteriax/resources/"))
                .and_then(|id| resources.iter().find(|r| r.id == id))
        };
        if let (Some(cert), Some(key)) = (file("cert"), file("key")) {
            validate_pair_cached(&cert.content, &key.content).await?;
        }
    }
    Ok((resolved, resources))
}

async fn validate_server_tls_resource_pair(
    config: &Value,
    resources: &[ResourceFile],
) -> Result<(), ApiError> {
    let Some(tls) = config.get("tls").and_then(Value::as_object) else {
        return Ok(());
    };
    let Some(certificate_id) = tls
        .get("cert")
        .and_then(Value::as_str)
        .and_then(|path| path.strip_prefix("resource://"))
    else {
        return Ok(());
    };
    let Some(private_key_id) = tls
        .get("key")
        .and_then(Value::as_str)
        .and_then(|path| path.strip_prefix("resource://"))
    else {
        return Ok(());
    };
    let certificate = resources
        .iter()
        .find(|resource| resource.id == certificate_id)
        .ok_or_else(|| ApiError::bad_request("TLS certificate resource is missing"))?;
    let private_key = resources
        .iter()
        .find(|resource| resource.id == private_key_id)
        .ok_or_else(|| ApiError::bad_request("TLS private-key resource is missing"))?;
    validate_pair_cached(&certificate.content, &private_key.content).await?;
    Ok(())
}

fn validate_resource_kinds(
    value: &Value,
    kinds: &BTreeMap<String, String>,
) -> Result<(), ApiError> {
    match value {
        Value::Object(values) => {
            for (key, child) in values {
                if let Value::String(reference) = child
                    && let Some(id) = reference.strip_prefix("resource://")
                {
                    let expected = match key.as_str() {
                        "cert" | "clientCA" => Some("certificate"),
                        "key" => Some("private_key"),
                        "keyPath" => Some("ech_key"),
                        "file" => Some("acl"),
                        "geoip" => Some("geoip"),
                        "geosite" => Some("geosite"),
                        _ => None,
                    };
                    let Some(expected) = expected else {
                        return Err(ApiError::bad_request(format!(
                            "resource references are not supported for field {key}"
                        )));
                    };
                    if kinds.get(id).map(String::as_str) != Some(expected) {
                        return Err(ApiError::bad_request(format!(
                            "resource kind for {key} must be {expected}"
                        )));
                    }
                }
                validate_resource_kinds(child, kinds)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                validate_resource_kinds(child, kinds)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn collect_resource_ids(value: &Value) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    match value {
        Value::String(value) => {
            if let Some(id) = value.strip_prefix("resource://")
                && !id.is_empty()
                && !id.contains('/')
            {
                result.insert(id.to_owned());
            }
        }
        Value::Array(values) => values
            .iter()
            .for_each(|value| result.extend(collect_resource_ids(value))),
        Value::Object(values) => values
            .values()
            .for_each(|value| result.extend(collect_resource_ids(value))),
        _ => {}
    }
    result
}

fn rewrite_resource_paths(value: &mut Value) {
    match value {
        Value::String(path) => {
            if let Some(id) = path.strip_prefix("resource://") {
                *path = format!("/etc/hysteriax/resources/{id}");
            }
        }
        Value::Array(values) => values.iter_mut().for_each(rewrite_resource_paths),
        Value::Object(values) => values.values_mut().for_each(rewrite_resource_paths),
        _ => {}
    }
}

#[derive(Deserialize)]
pub struct CreateResource {
    name: String,
    resource_kind: String,
    content_base64: String,
}

pub async fn list(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
) -> Result<Json<Vec<Value>>, ApiError> {
    ensure_node(&state, &node_id).await?;
    let rows = sqlx::query("SELECT id, name, resource_kind, content_sha256, size_bytes, created_at FROM config_resources WHERE node_id = $1 ORDER BY lower(name) COLLATE \"C\", name COLLATE \"C\", id")
        .bind(&node_id).fetch_all(&mut *crate::read_snapshot::begin(&state.pool).await?).await?;
    Ok(Json(rows.iter().map(|row| json!({
        "id": row.get::<String, _>("id"), "name": row.get::<String, _>("name"),
        "resource_kind": row.get::<String, _>("resource_kind"), "content_sha256": row.get::<String, _>("content_sha256"),
        "size_bytes": row.get::<i64, _>("size_bytes"), "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        "reference": format!("resource://{}", row.get::<String, _>("id"))
    })).collect()))
}

pub async fn create(
    State(state): State<AppState>,
    AxumPath(node_id): AxumPath<String>,
    Json(input): Json<CreateResource>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if !safe_name(&input.name) {
        return Err(ApiError::bad_request(
            "name must be a single filename without path separators",
        ));
    }
    if !matches!(input.resource_kind.as_str(), "acl" | "geoip" | "geosite") {
        return Err(ApiError::bad_request("unsupported config resource kind"));
    }
    let content = STANDARD
        .decode(input.content_base64.as_bytes())
        .map_err(|_| ApiError::bad_request("content_base64 is invalid"))?;
    if content.is_empty() || content.len() > MAX_RESOURCE_BYTES {
        return Err(ApiError::bad_request(
            "resource must be between 1 byte and 20 MiB",
        ));
    }
    validate_text_resource(&input.resource_kind, &content)?;
    let id = Uuid::new_v4().to_string();
    let digest = hex::encode(sha2::Sha256::digest(&content));
    let timestamp = now();
    let content_enc = state.secrets.encrypt_bytes(&content)?;
    let size = content.len() as i64;
    let name = input.name.trim().to_owned();
    let kind = input.resource_kind;
    let response = json!({"id":id,"name":name,"resource_kind":kind,"content_sha256":digest,"size_bytes":size,"reference":format!("resource://{id}")});
    let actor = super::audit_actor();
    db::serializable::<_, ApiError, _>(&state.pool, move |tx| {
        let (id,node_id,name,kind,content_enc,digest,actor) = (id.clone(),node_id.clone(),name.clone(),kind.clone(),content_enc.clone(),digest.clone(),actor.clone());
        Box::pin(async move {
            editable_node_on(tx, &node_id).await?;
            let inserted = sqlx::query("INSERT INTO config_resources(id,node_id,name,resource_kind,content_enc,content_sha256,size_bytes,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(node_id,name) DO NOTHING")
                .bind(&id).bind(&node_id).bind(&name).bind(&kind).bind(content_enc).bind(&digest).bind(size).bind(timestamp).execute(&mut **tx).await?;
            if inserted.rows_affected()!=1 {
                return Err(ApiError::conflict("a resource with this name already exists on the node"));
            }
            super::audit_on(tx,&actor,"node.resource_uploaded","node",&node_id,json!({"resource_id":id,"name":name,"kind":kind,"sha256":digest,"size_bytes":size})).await
        })
    }).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

pub async fn delete(
    State(state): State<AppState>,
    AxumPath((node_id, resource_id)): AxumPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let actor = super::audit_actor();
    db::serializable::<_, ApiError, _>(&state.pool, move |tx| {
        let (node_id,resource_id,actor) = (node_id.clone(),resource_id.clone(),actor.clone());
        Box::pin(async move {
            editable_node_on(tx, &node_id).await?;
            crate::credentials::reference_index::assert_clean_on(tx).await?;
            let name: String = sqlx::query_scalar("SELECT name FROM config_resources WHERE node_id=$1 AND id=$2 FOR UPDATE")
                .bind(&node_id).bind(&resource_id).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::not_found("config resource"))?;
            let referenced: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM config_resource_references WHERE node_id=$1 AND resource_id=$2)")
                .bind(&node_id).bind(&resource_id).fetch_one(&mut **tx).await?;
            if referenced { return Err(ApiError::conflict("resource is referenced by a current, deployed or saved node configuration")); }
            sqlx::query("DELETE FROM config_resources WHERE node_id=$1 AND id=$2")
                .bind(&node_id).bind(&resource_id).execute(&mut **tx).await?;
            super::audit_on(tx,&actor,"node.resource_deleted","node",&node_id,json!({"resource_id":resource_id,"name":name})).await
        })
    }).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn editable_node_on(connection: &mut sqlx::PgConnection, node: &str) -> Result<(), ApiError> {
    let state: String = sqlx::query_scalar("SELECT state FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(node)
        .fetch_optional(connection)
        .await?
        .ok_or_else(|| ApiError::not_found("node"))?;
    if matches!(state.as_str(), "deleting" | "delete_failed") {
        return Err(ApiError::conflict("node removal is in progress"));
    }
    Ok(())
}

async fn ensure_node(state: &AppState, node_id: &str) -> Result<(), ApiError> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM nodes WHERE id = $1")
        .bind(node_id)
        .fetch_one(&mut *crate::read_snapshot::begin(&state.pool).await?)
        .await?;
    if exists == 0 {
        return Err(ApiError::not_found("node"));
    }
    Ok(())
}

fn safe_name(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 180
        && Path::new(trimmed)
            .file_name()
            .and_then(|name| name.to_str())
            == Some(trimmed)
        && !trimmed.contains('/')
        && !trimmed.contains('\\')
        && !trimmed.chars().any(char::is_control)
}

pub(crate) fn validate_text_resource(kind: &str, content: &[u8]) -> Result<(), ApiError> {
    if matches!(kind, "geoip" | "geosite") {
        return Ok(());
    }
    let text = std::str::from_utf8(content)
        .map_err(|_| ApiError::bad_request("this resource kind must contain UTF-8 text"))?;
    let valid = match kind {
        "certificate" => valid_certificate_pem(text),
        "private_key" => valid_private_key_pem(text),
        "ech_key" => {
            valid_base64_pem_block(text, "ECH KEYS") && valid_base64_pem_block(text, "ECH CONFIGS")
        }
        "acl" => true,
        _ => false,
    };
    if !valid {
        return Err(ApiError::bad_request(
            "resource content does not match its declared kind",
        ));
    }
    Ok(())
}

fn valid_certificate_pem(text: &str) -> bool {
    let Ok(certificates) =
        rustls_pemfile::certs(&mut Cursor::new(text.as_bytes())).collect::<Result<Vec<_>, _>>()
    else {
        return false;
    };
    if certificates.is_empty() {
        return false;
    }
    certificates.iter().all(|certificate| {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate.clone()).is_ok()
    })
}

fn valid_private_key_pem(text: &str) -> bool {
    let Ok(Some(private_key)) = rustls_pemfile::private_key(&mut Cursor::new(text.as_bytes()))
    else {
        return false;
    };
    rustls::crypto::ring::default_provider()
        .key_provider
        .load_private_key(private_key)
        .is_ok()
}

fn valid_base64_pem_block(text: &str, name: &str) -> bool {
    let begin = format!("-----BEGIN {name}-----");
    let end = format!("-----END {name}-----");
    let Some(body) = text
        .split_once(&begin)
        .and_then(|(_, tail)| tail.split_once(&end).map(|(body, _)| body))
    else {
        return false;
    };
    let encoded: String = body
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    !encoded.is_empty() && STANDARD.decode(encoded.as_bytes()).is_ok()
}

async fn validate_pair_cached(certificate: &[u8], private_key: &[u8]) -> Result<(), ApiError> {
    use sha2::{Digest, Sha256};
    static VALIDATED: std::sync::OnceLock<crate::cache::Cache<bool>> = std::sync::OnceLock::new();
    let mut hash = Sha256::new();
    hash.update(certificate.len().to_be_bytes());
    hash.update(certificate);
    hash.update(private_key);
    let key = hex::encode(hash.finalize());
    VALIDATED
        .get_or_init(|| crate::cache::Cache::new(128, 128, 1, |_| 1))
        .get(key, std::time::Duration::from_secs(1800), false, || async {
            let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| ApiError::bad_request("invalid TLS certificate"))?;
            if certificates.is_empty() {
                return Err(ApiError::bad_request(
                    "TLS certificate must contain a PEM certificate",
                ));
            }
            let private_key = rustls_pemfile::private_key(&mut Cursor::new(private_key))
                .map_err(|_| ApiError::bad_request("invalid TLS key"))?
                .ok_or_else(|| ApiError::bad_request("TLS key is missing"))?;
            rustls::sign::CertifiedKey::from_der(
                certificates,
                private_key,
                &rustls::crypto::ring::default_provider(),
            )
            .map_err(|_| ApiError::bad_request("TLS certificate and private key do not match"))?;
            Ok::<_, ApiError>(true)
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{validate_resource_kinds, validate_text_resource};

    #[test]
    fn validates_resource_types_before_deployment() {
        let kinds = BTreeMap::from([
            ("cert-id".to_owned(), "certificate".to_owned()),
            ("key-id".to_owned(), "private_key".to_owned()),
            ("ech-id".to_owned(), "ech_key".to_owned()),
        ]);
        assert!(
            validate_resource_kinds(
                &json!({"tls": {"cert": "resource://cert-id", "key": "resource://key-id"}}),
                &kinds
            )
            .is_ok()
        );
        assert!(
            validate_resource_kinds(&json!({"tls": {"cert": "resource://key-id"}}), &kinds)
                .is_err()
        );
        assert!(
            validate_resource_kinds(&json!({"tls": {"clientCA": "resource://cert-id"}}), &kinds)
                .is_ok()
        );
        assert!(
            validate_resource_kinds(&json!({"tls": {"clientCA": "resource://key-id"}}), &kinds)
                .is_err()
        );
        assert!(
            validate_resource_kinds(&json!({"ech": {"keyPath": "resource://ech-id"}}), &kinds)
                .is_ok()
        );
        assert!(
            validate_resource_kinds(&json!({"ech": {"keyPath": "resource://key-id"}}), &kinds)
                .is_err()
        );
    }

    #[test]
    fn ech_key_resources_require_both_base64_pem_blocks() {
        let content = b"-----BEGIN ECH KEYS-----\nS0VZ\n-----END ECH KEYS-----\n-----BEGIN ECH CONFIGS-----\nQ09ORklH\n-----END ECH CONFIGS-----\n";
        assert!(validate_text_resource("ech_key", content).is_ok());
        assert!(
            validate_text_resource(
                "ech_key",
                b"-----BEGIN ECH KEYS-----\nS0VZ\n-----END ECH KEYS-----"
            )
            .is_err()
        );
        assert!(validate_text_resource("ech_key", b"-----BEGIN ECH KEYS-----\ninvalid!\n-----END ECH KEYS-----\n-----BEGIN ECH CONFIGS-----\nQ09ORklH\n-----END ECH CONFIGS-----").is_err());
    }

    #[test]
    fn server_certificate_and_key_resources_must_parse_as_pem() {
        assert!(
            validate_text_resource(
                "certificate",
                b"-----BEGIN CERTIFICATE-----\n%%%\n-----END CERTIFICATE-----"
            )
            .is_err()
        );
        assert!(
            validate_text_resource(
                "private_key",
                b"-----BEGIN PRIVATE KEY-----\n%%%\n-----END PRIVATE KEY-----"
            )
            .is_err()
        );
        assert!(
            validate_text_resource(
                "certificate",
                b"-----BEGIN CERTIFICATE-----\nS0VZ\n-----END CERTIFICATE-----"
            )
            .is_err()
        );
        assert!(
            validate_text_resource(
                "private_key",
                b"-----BEGIN PRIVATE KEY-----\nS0VZ\n-----END PRIVATE KEY-----"
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod transaction_tests;
