pub mod migration;
#[cfg(test)]
mod tests;
pub mod worker;

use std::{collections::BTreeMap, io::Cursor};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{api::resources::ResourceFile, security::SecretBox};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Reference {
    pub id: String,
    pub version: i64,
    pub field: String,
}

impl Reference {
    pub fn parse(value: &str) -> Result<Self> {
        let rest = value
            .strip_prefix("credential://")
            .context("credential reference required")?;
        let parts: Vec<_> = rest.split('/').collect();
        if parts.len() != 3 || Uuid::parse_str(parts[0]).is_err() {
            bail!("invalid credential reference");
        }
        let version: i64 = parts[1].parse().context("invalid credential version")?;
        if version < 1
            || parts[2].is_empty()
            || !parts[2]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            bail!("invalid credential reference");
        }
        Ok(Self {
            id: parts[0].into(),
            version,
            field: parts[2].into(),
        })
    }

    pub fn uri(&self) -> String {
        format!("credential://{}/{}/{}", self.id, self.version, self.field)
    }
}

pub fn references(value: &Value) -> Result<Vec<(String, Reference)>> {
    fn visit(value: &Value, path: &str, output: &mut Vec<(String, Reference)>) -> Result<()> {
        match value {
            Value::String(s) if s.starts_with("credential://") => {
                output.push((path.into(), Reference::parse(s)?))
            }
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    visit(item, &format!("{path}/{i}"), output)?;
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    visit(
                        item,
                        &format!("{path}/{}", key.replace('~', "~0").replace('/', "~1")),
                        output,
                    )?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    let mut output = Vec::new();
    visit(value, "", &mut output)?;
    Ok(output)
}

pub fn replace_version(value: &mut Value, id: &str, version: i64) -> Result<bool> {
    let refs = references(value)?;
    let mut changed = false;
    for (path, mut reference) in refs {
        if reference.id == id {
            reference.version = version;
            *value
                .pointer_mut(&path)
                .context("missing credential path")? = json!(reference.uri());
            changed = true;
        }
    }
    Ok(changed)
}

fn text<'a>(payload: &'a Value, field: &str) -> Result<&'a str> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .context("credential field is missing or empty")
}

pub fn validate_payload(kind: &str, payload: &Value) -> Result<Value> {
    let object = payload
        .as_object()
        .context("credential payload must be an object")?;
    if payload.to_string().len() > 2 * 1024 * 1024 {
        bail!("credential payload exceeds 2 MiB");
    }
    let fields: &[&str] = match kind {
        "ssh_private_key" => &["secret", "passphrase"],
        "ssh_password" => &["secret"],
        "tls_identity" => &["certificate", "private_key"],
        "ca_certificate" | "ech_key" => &["content"],
        "dns" => &["provider", "config"],
        "api_token" => &["token"],
        _ => bail!("unsupported credential type"),
    };
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        bail!("unexpected credential field");
    }
    let mut metadata = json!({});
    match kind {
        "ssh_private_key" => {
            let passphrase = payload
                .get("passphrase")
                .map(|v| v.as_str().context("passphrase must be a string"))
                .transpose()?;
            let key = russh::keys::decode_secret_key(text(payload, "secret")?, passphrase)
                .map_err(|_| anyhow::anyhow!("invalid SSH private key or passphrase"))?;
            metadata = json!({"fingerprint": key.public_key().fingerprint(russh::keys::ssh_key::HashAlg::Sha256).to_string(), "has_passphrase": passphrase.is_some_and(|s| !s.is_empty())});
        }
        "ssh_password" => {
            text(payload, "secret")?;
        }
        "api_token" => {
            text(payload, "token")?;
        }
        "dns" => {
            let provider = text(payload, "provider")?;
            crate::config::validate_acme(
                &json!({"domains": ["example.invalid"], "type": "dns", "dns": {"name": provider, "config": payload.get("config").context("DNS config is required")?}}),
            )?;
            if payload["config"].as_object().is_none_or(|m| {
                m.keys().any(|k| {
                    k.is_empty() || !k.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                })
            }) {
                bail!("invalid DNS field name");
            }
            metadata = json!({"provider": provider, "fields": payload["config"].as_object().expect("validated map").keys().collect::<Vec<_>>()});
        }
        "tls_identity" => {
            let certificate = text(payload, "certificate")?;
            crate::api::resources::validate_text_resource("certificate", certificate.as_bytes())
                .map_err(|_| anyhow::anyhow!("invalid certificate PEM"))?;
            let certs = rustls_pemfile::certs(&mut Cursor::new(certificate.as_bytes()))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let key = rustls_pemfile::private_key(&mut Cursor::new(
                text(payload, "private_key")?.as_bytes(),
            ))?
            .context("private key required")?;
            rustls::sign::CertifiedKey::from_der(
                certs,
                key,
                &rustls::crypto::ring::default_provider(),
            )
            .map_err(|_| anyhow::anyhow!("certificate and private key do not match"))?;
            metadata = certificate_metadata(certificate)?;
        }
        "ca_certificate" | "ech_key" => {
            let content = text(payload, "content")?;
            let resource_kind = if kind == "ca_certificate" {
                "certificate"
            } else {
                kind
            };
            crate::api::resources::validate_text_resource(resource_kind, content.as_bytes())
                .map_err(|_| anyhow::anyhow!("invalid credential content"))?;
            if kind == "ca_certificate" {
                metadata = certificate_metadata(content)?;
                if kind == "ca_certificate" && metadata["is_ca"] != true {
                    bail!("CA certificate must have the CA basic constraint");
                }
            }
        }
        _ => unreachable!(),
    }
    Ok(metadata)
}

pub fn certificate_metadata(pem: &str) -> Result<Value> {
    let cert = rustls_pemfile::certs(&mut Cursor::new(pem.as_bytes()))
        .next()
        .context("certificate required")??;
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref())
        .map_err(|_| anyhow::anyhow!("invalid X.509 certificate"))?;
    let expiry = DateTime::<Utc>::from_timestamp(parsed.validity().not_after.timestamp(), 0)
        .context("invalid certificate expiry")?;
    let start = DateTime::<Utc>::from_timestamp(parsed.validity().not_before.timestamp(), 0)
        .context("invalid certificate start")?;
    let domains = parsed
        .subject_alternative_name()
        .map_err(|_| anyhow::anyhow!("invalid subject alternative names"))?
        .map(|san| {
            san.value
                .general_names
                .iter()
                .filter_map(|n| match n {
                    x509_parser::extensions::GeneralName::DNSName(s) => Some(s.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(
        json!({"certificate": pem, "subject": parsed.subject().to_string(), "issuer": parsed.issuer().to_string(), "domains": domains, "fingerprint": hex::encode(Sha256::digest(cert.as_ref())), "not_before": start, "expires_at": expiry, "is_ca": parsed.is_ca()}),
    )
}

// Migration supplies metadata and immutable filenames independently of the
// validated import API; keep each persisted field explicit at this boundary.
#[allow(clippy::too_many_arguments)]
pub async fn insert(
    tx: &mut Transaction<'_, Postgres>,
    secrets: &SecretBox,
    name: &str,
    kind: &str,
    owner: Option<&str>,
    payload: &Value,
    metadata: &Value,
    artifacts: &Value,
) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let timestamp = Utc::now();
    sqlx::query("INSERT INTO credentials (id, name, kind, owner_user_id, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$5)").bind(&id).bind(name).bind(kind).bind(owner).bind(timestamp).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO credential_versions (credential_id,version,payload_enc,metadata,artifact_names,created_at) VALUES ($1,1,$2,$3,$4,$5)").bind(&id).bind(secrets.encrypt(&payload.to_string())?).bind(metadata).bind(artifacts).bind(timestamp).execute(&mut **tx).await?;
    Ok(id)
}

pub async fn load(
    pool: &PgPool,
    secrets: &SecretBox,
    id: &str,
    version: i64,
) -> Result<(String, Option<String>, bool, Value, Value)> {
    let row = sqlx::query("SELECT c.kind,c.owner_user_id,c.archived,v.payload_enc,v.artifact_names FROM credentials c JOIN credential_versions v ON v.credential_id=c.id WHERE c.id=$1 AND v.version=$2").bind(id).bind(version).fetch_optional(pool).await?.context("credential version not found")?;
    let payload = serde_json::from_str(&secrets.decrypt(&row.get::<String, _>("payload_enc"))?)?;
    Ok((
        row.get("kind"),
        row.get("owner_user_id"),
        row.get("archived"),
        payload,
        row.get("artifact_names"),
    ))
}

fn check_config_reference(
    path: &str,
    reference: &Reference,
    kind: &str,
    owner: Option<&str>,
) -> Result<bool> {
    if owner.is_some() {
        bail!("user-owned credentials cannot be used in node configuration");
    }
    let file = match path {
        "/tls/cert" => kind == "tls_identity" && reference.field == "certificate",
        "/tls/key" => kind == "tls_identity" && reference.field == "private_key",
        "/tls/clientCA" => kind == "ca_certificate" && reference.field == "content",
        "/ech/keyPath" => kind == "ech_key" && reference.field == "content",
        p if p.starts_with("/acme/dns/config/") => {
            if kind != "dns" || reference.field != p.trim_start_matches("/acme/dns/config/") {
                bail!("invalid DNS credential reference");
            }
            return Ok(false);
        }
        _ => false,
    };
    if !file {
        bail!("credential type or field is invalid for this configuration position");
    }
    Ok(true)
}

/// Resolve pinned versions into runtime values and immutable remote artifacts.
/// The public config and snapshot always keep references, never resolved secrets.
pub async fn resolve_config(
    pool: &PgPool,
    secrets: &SecretBox,
    config: &Value,
    new_binding: bool,
) -> Result<(Value, Vec<ResourceFile>)> {
    let dns_references: std::collections::BTreeSet<_> = references(config)?
        .into_iter()
        .filter(|(path, _)| path.starts_with("/acme/dns/config/"))
        .map(|(_, r)| (r.id, r.version))
        .collect();
    if dns_references.len() > 1 {
        bail!("DNS configuration must use one credential version");
    }
    let mut resolved = config.clone();
    let mut files = BTreeMap::<String, Vec<u8>>::new();
    for (path, reference) in references(config)? {
        let (kind, owner, archived, payload, artifacts) =
            load(pool, secrets, &reference.id, reference.version).await?;
        if new_binding && archived {
            bail!("credential is archived");
        }
        let file = check_config_reference(&path, &reference, &kind, owner.as_deref())?;
        let content = if kind == "dns" {
            if payload["provider"].as_str()
                != config.pointer("/acme/dns/name").and_then(Value::as_str)
            {
                bail!("DNS credential provider does not match node configuration");
            }
            text(&payload["config"], &reference.field)?
        } else {
            text(&payload, &reference.field)?
        };
        let replacement = if file {
            let filename = artifacts
                .get(&reference.field)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!("{}-{}-{}", reference.id, reference.version, reference.field)
                });
            if filename.is_empty()
                || filename
                    .bytes()
                    .any(|c| !(c.is_ascii_alphanumeric() || c == b'-' || c == b'_'))
            {
                bail!("invalid credential artifact name");
            }
            files.insert(filename.clone(), content.as_bytes().to_vec());
            format!("/etc/hysteriax/resources/{filename}")
        } else {
            content.to_owned()
        };
        *resolved
            .pointer_mut(&path)
            .context("missing credential path")? = json!(replacement);
    }
    Ok((
        resolved,
        files
            .into_iter()
            .map(|(id, content)| ResourceFile { id, content })
            .collect(),
    ))
}

pub fn require_managed_config(config: &Value) -> Result<()> {
    for path in ["/tls/cert", "/tls/key", "/tls/clientCA", "/ech/keyPath"] {
        if let Some(s) = config.pointer(path).and_then(Value::as_str)
            && !s.starts_with("credential://")
        {
            bail!("TLS and ECH must reference managed credentials");
        }
    }
    if let Some(fields) = config
        .pointer("/acme/dns/config")
        .and_then(Value::as_object)
        && fields
            .values()
            .any(|v| v.as_str().is_none_or(|s| !s.starts_with("credential://")))
    {
        bail!("DNS configuration must reference managed credentials");
    }
    if config.get("tls").is_some() {
        let cert = config
            .pointer("/tls/cert")
            .and_then(Value::as_str)
            .map(Reference::parse)
            .transpose()?
            .context("TLS certificate pair is required")?;
        let key = config
            .pointer("/tls/key")
            .and_then(Value::as_str)
            .map(Reference::parse)
            .transpose()?
            .context("TLS certificate pair is required")?;
        if cert.id != key.id
            || cert.version != key.version
            || cert.field != "certificate"
            || key.field != "private_key"
        {
            bail!("TLS must use the certificate and private key from one certificate-pair version");
        }
    }
    references(config)?;
    Ok(())
}

pub async fn check_bindings(
    tx: &mut Transaction<'_, Postgres>,
    config: &Value,
    previous: Option<&Value>,
    ssh: Option<(&str, i64, bool)>,
) -> Result<()> {
    let previous = previous.map(references).transpose()?.unwrap_or_default();
    for (path, r) in references(config)? {
        let existed = previous.iter().any(|(p, old)| p == &path && old == &r);
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials c JOIN credential_versions v ON v.credential_id=c.id WHERE c.id=$1 AND v.version=$2 AND c.owner_user_id IS NULL AND (c.archived=FALSE OR $3))").bind(&r.id).bind(r.version).bind(existed).fetch_one(&mut **tx).await?;
        if !valid {
            bail!(
                "configuration credential was deleted, archived, or is outside its allowed scope"
            );
        }
    }
    if let Some((id, version, existed)) = ssh {
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials c JOIN credential_versions v ON v.credential_id=c.id WHERE c.id=$1 AND v.version=$2 AND c.kind IN ('ssh_password','ssh_private_key') AND c.owner_user_id IS NULL AND (c.archived=FALSE OR $3))").bind(id).bind(version).bind(existed).fetch_one(&mut **tx).await?;
        if !valid {
            bail!("SSH credential was deleted, archived, or has the wrong type");
        }
    }
    Ok(())
}

pub async fn ssh_node(
    pool: &PgPool,
    secrets: &SecretBox,
    row: &sqlx::postgres::PgRow,
) -> Result<crate::ssh::SshNode> {
    let id: String = row.get("ssh_credential_id");
    let version: i64 = row.get("ssh_credential_version");
    let (kind, owner, _, payload, _) = load(pool, secrets, &id, version).await?;
    if owner.is_some() || !matches!(kind.as_str(), "ssh_password" | "ssh_private_key") {
        bail!("invalid SSH credential type");
    }
    Ok(crate::ssh::SshNode {
        host: row.get("ssh_host"),
        port: row.get::<i32, _>("ssh_port") as u16,
        username: row.get("ssh_username"),
        auth_type: if kind == "ssh_password" {
            "password"
        } else {
            "private_key"
        }
        .into(),
        secret: text(&payload, "secret")?.into(),
        passphrase: payload
            .get("passphrase")
            .and_then(Value::as_str)
            .map(str::to_owned),
        host_fingerprint: row.get("ssh_host_fingerprint"),
    })
}

pub async fn assignment_identity(
    pool: &PgPool,
    secrets: &SecretBox,
    row: &sqlx::postgres::PgRow,
) -> Result<Option<(String, String)>> {
    let Some(id) = row.get::<Option<String>, _>("mtls_credential_id") else {
        return Ok(None);
    };
    let version: i64 = row.get("mtls_credential_version");
    let (kind, owner, _, payload, _) = load(pool, secrets, &id, version).await?;
    if kind != "tls_identity" || owner.as_deref() != Some(row.get::<String, _>("user_id").as_str())
    {
        bail!("invalid user-owned mTLS credential");
    }
    Ok(Some((
        text(&payload, "certificate")?.into(),
        text(&payload, "private_key")?.into(),
    )))
}

#[cfg(test)]
pub async fn test_node_request(
    state: &crate::state::AppState,
    mut input: Value,
) -> crate::api::nodes::CreateNode {
    migration::migrate(&state.pool, &state.secrets)
        .await
        .unwrap();
    let kind = if input["ssh_auth_type"] == "password" {
        "ssh_password"
    } else {
        "ssh_private_key"
    };
    let payload = json!({"secret":input["ssh_secret"].take()});
    let mut tx = crate::db::begin_write(&state.pool).await.unwrap();
    let id = insert(
        &mut tx,
        &state.secrets,
        "Fixture SSH",
        kind,
        None,
        &payload,
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let object = input.as_object_mut().unwrap();
    for field in ["ssh_secret", "ssh_auth_type", "ssh_passphrase"] {
        object.remove(field);
    }
    object.insert("ssh_credential_id".into(), json!(id));
    object.insert("ssh_credential_version".into(), json!(1));
    serde_json::from_value(input).unwrap()
}
