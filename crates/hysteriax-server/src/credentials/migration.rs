//! One atomic conversion while the process owns the database instance lock.
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};

use super::{Reference, insert};
use crate::{db, security::SecretBox};

pub async fn migrate(pool: &PgPool, secrets: &SecretBox) -> Result<()> {
    let mut tx = db::begin_write(pool).await?;
    let done: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM credential_migrations WHERE name='credentials_v2')",
    )
    .fetch_one(&mut *tx)
    .await?;
    if done {
        tx.commit().await?;
        return migrate_tls_pairs(pool, secrets).await;
    }

    let resources = sqlx::query("SELECT * FROM config_resources WHERE resource_kind IN ('certificate','private_key','ech_key')").fetch_all(&mut *tx).await?;
    let mut mapping = BTreeMap::<(String, String), String>::new();
    for row in resources {
        let node: String = row.get("node_id");
        let resource: String = row.get("id");
        let kind: String = row.get("resource_kind");
        let bytes = secrets.decrypt_bytes(&row.get::<String, _>("content_enc"))?;
        let content =
            String::from_utf8(bytes).context("legacy credential resource is not UTF-8")?;
        crate::api::resources::validate_text_resource(&kind, content.as_bytes())
            .map_err(|_| anyhow::anyhow!("invalid legacy resource"))?;
        let metadata = if kind == "certificate" {
            super::certificate_metadata(&content)?
        } else {
            json!({})
        };
        let id = insert(
            &mut tx,
            secrets,
            &format!("{} · {}", row.get::<String, _>("name"), node),
            &kind,
            None,
            &json!({"content":content}),
            &metadata,
            &json!({"content":resource}),
        )
        .await?;
        mapping.insert(
            (node, resource),
            Reference {
                id,
                version: 1,
                field: "content".into(),
            }
            .uri(),
        );
    }

    let nodes = sqlx::query("SELECT * FROM nodes")
        .fetch_all(&mut *tx)
        .await?;
    for row in nodes {
        let node: String = row.get("id");
        let name: String = row.get("name");
        let secret = secrets.decrypt(&row.get::<String, _>("ssh_secret_enc"))?;
        let passphrase = row
            .get::<Option<String>, _>("ssh_passphrase_enc")
            .map(|s| secrets.decrypt(&s))
            .transpose()?;
        let kind = if row.get::<String, _>("ssh_auth_type") == "password" {
            "ssh_password"
        } else {
            "ssh_private_key"
        };
        let mut payload = json!({"secret":secret});
        if let Some(passphrase) = passphrase {
            payload["passphrase"] = json!(passphrase);
        }
        // Migration preserves existing authentication material, including invalid
        // credentials a user may already be repairing. New imports are validated.
        let metadata = super::validate_payload(kind, &payload)
            .unwrap_or_else(|_| json!({"needs_validation":true}));
        let id = insert(
            &mut tx,
            secrets,
            &format!("{name} · SSH"),
            kind,
            None,
            &payload,
            &metadata,
            &json!({}),
        )
        .await?;
        sqlx::query("UPDATE nodes SET ssh_credential_id=$1,ssh_credential_version=1 WHERE id=$2")
            .bind(&id)
            .bind(&node)
            .execute(&mut *tx)
            .await?;
        // Verify other encrypted node material before committing any conversion.
        for field in ["node_token_enc", "traffic_stats_secret_enc"] {
            secrets.decrypt(&row.get::<String, _>(field))?;
        }

        let mut dns = BTreeMap::new();
        let mut desired: Value =
            serde_json::from_str(&secrets.decrypt(&row.get::<String, _>("desired_config_enc"))?)?;
        convert_config(
            &mut tx,
            secrets,
            &node,
            &name,
            &mut desired,
            &mapping,
            &mut dns,
        )
        .await?;
        let deployed = if let Some(cipher) = row.get::<Option<String>, _>("deployed_config_enc") {
            let mut value: Value = serde_json::from_str(&secrets.decrypt(&cipher)?)?;
            convert_snapshot(
                &mut tx, secrets, &node, &name, &mut value, &mapping, &mut dns,
            )
            .await?;
            Some(secrets.encrypt(&value.to_string())?)
        } else {
            None
        };
        let versions = sqlx::query(
            "SELECT id,config_enc FROM config_versions WHERE node_id=$1 ORDER BY revision",
        )
        .bind(&node)
        .fetch_all(&mut *tx)
        .await?;
        for version in versions {
            let mut value: Value =
                serde_json::from_str(&secrets.decrypt(&version.get::<String, _>("config_enc"))?)?;
            convert_snapshot(
                &mut tx, secrets, &node, &name, &mut value, &mapping, &mut dns,
            )
            .await?;
            let plain = value.to_string();
            sqlx::query("UPDATE config_versions SET config_enc=$1,content_sha256=$2 WHERE id=$3")
                .bind(secrets.encrypt(&plain)?)
                .bind(hex::encode(Sha256::digest(plain.as_bytes())))
                .bind(version.get::<String, _>("id"))
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("UPDATE nodes SET desired_config_enc=$1,deployed_config_enc=$2 WHERE id=$3")
            .bind(secrets.encrypt(&desired.to_string())?)
            .bind(deployed)
            .bind(&node)
            .execute(&mut *tx)
            .await?;
    }

    let assignments = sqlx::query("SELECT * FROM node_assignments")
        .fetch_all(&mut *tx)
        .await?;
    for row in assignments {
        secrets.decrypt(&row.get::<String, _>("credential_enc"))?;
        match (
            row.get::<Option<String>, _>("client_certificate_enc"),
            row.get::<Option<String>, _>("client_private_key_enc"),
        ) {
            (Some(cert), Some(key)) => {
                let user: String = row.get("user_id");
                let node: String = row.get("node_id");
                let payload = json!({"certificate":secrets.decrypt(&cert)?,"private_key":secrets.decrypt(&key)?});
                let metadata = super::validate_payload("tls_identity", &payload)?;
                let id = insert(
                    &mut tx,
                    secrets,
                    &format!("{user} · {node} · mTLS"),
                    "tls_identity",
                    Some(&user),
                    &payload,
                    &metadata,
                    &json!({}),
                )
                .await?;
                sqlx::query("UPDATE node_assignments SET mtls_credential_id=$1,mtls_credential_version=1 WHERE user_id=$2 AND node_id=$3").bind(id).bind(user).bind(node).execute(&mut *tx).await?;
            }
            (None, None) => (),
            _ => anyhow::bail!("legacy mTLS credential pair is incomplete"),
        }
    }
    for cipher in sqlx::query_scalar::<_, String>("SELECT token_enc FROM subscription_credentials")
        .fetch_all(&mut *tx)
        .await?
    {
        secrets.decrypt(&cipher)?;
    }
    sqlx::query("DELETE FROM config_resources WHERE resource_kind IN ('certificate','private_key','ech_key')").execute(&mut *tx).await?;
    sqlx::query("ALTER TABLE nodes DROP COLUMN ssh_secret_enc, DROP COLUMN ssh_passphrase_enc, DROP COLUMN ssh_auth_type, ALTER COLUMN ssh_credential_id SET NOT NULL, ALTER COLUMN ssh_credential_version SET NOT NULL").execute(&mut *tx).await?;
    sqlx::query("ALTER TABLE node_assignments DROP COLUMN client_certificate_enc, DROP COLUMN client_private_key_enc").execute(&mut *tx).await?;
    sqlx::query("INSERT INTO credential_migrations(name,completed_at) VALUES('credentials_v2',$1)")
        .bind(Utc::now())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    migrate_tls_pairs(pool, secrets).await
}

async fn convert_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    secrets: &SecretBox,
    node: &str,
    name: &str,
    value: &mut Value,
    mapping: &BTreeMap<(String, String), String>,
    dns: &mut BTreeMap<String, String>,
) -> Result<()> {
    let config = if value.get("server_config").is_some() {
        value.get_mut("server_config").expect("present config")
    } else {
        value
    };
    convert_config(tx, secrets, node, name, config, mapping, dns).await
}

async fn convert_config(
    tx: &mut Transaction<'_, Postgres>,
    secrets: &SecretBox,
    node: &str,
    name: &str,
    config: &mut Value,
    mapping: &BTreeMap<(String, String), String>,
    dns: &mut BTreeMap<String, String>,
) -> Result<()> {
    for path in ["/tls/cert", "/tls/key", "/tls/clientCA", "/ech/keyPath"] {
        if let Some(s) = config.pointer(path).and_then(Value::as_str)
            && let Some(resource) = s.strip_prefix("resource://")
        {
            let uri = mapping
                .get(&(node.to_owned(), resource.to_owned()))
                .context("legacy configuration credential resource is missing")?;
            *config.pointer_mut(path).expect("present path") = json!(uri);
        }
    }
    if let Some(settings) = config
        .pointer("/acme/dns/config")
        .and_then(Value::as_object)
        && !settings.is_empty()
        && !settings
            .values()
            .all(|v| v.as_str().is_some_and(|s| s.starts_with("credential://")))
    {
        let provider = config
            .pointer("/acme/dns/name")
            .and_then(Value::as_str)
            .context("legacy DNS provider missing")?
            .to_owned();
        let payload = json!({"provider":provider,"config":settings});
        let signature = payload.to_string();
        let id = if let Some(id) = dns.get(&signature) {
            id.clone()
        } else {
            let metadata = super::validate_payload("dns", &payload)?;
            let id = insert(
                tx,
                secrets,
                &format!("{name} · {provider}"),
                "dns",
                None,
                &payload,
                &metadata,
                &json!({}),
            )
            .await?;
            dns.insert(signature, id.clone());
            id
        };
        let replacement: serde_json::Map<String, Value> = settings
            .keys()
            .map(|key| {
                (
                    key.clone(),
                    json!(
                        Reference {
                            id: id.clone(),
                            version: 1,
                            field: key.clone()
                        }
                        .uri()
                    ),
                )
            })
            .collect();
        *config
            .pointer_mut("/acme/dns/config")
            .expect("present DNS config") = Value::Object(replacement);
    }
    Ok(())
}

/// Consolidate legacy TLS material without deploying or changing business revisions.
pub async fn migrate_tls_pairs(pool: &PgPool, secrets: &SecretBox) -> Result<()> {
    let mut tx = db::begin_write(pool).await?;
    let done: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM credential_migrations WHERE name='tls_pairs_v1')",
    )
    .fetch_one(&mut *tx)
    .await?;
    if done {
        tx.commit().await?;
        return Ok(());
    }
    let active: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind='credential-apply' AND status IN ('queued','running')")
        .fetch_one(&mut *tx).await?;
    anyhow::ensure!(
        active == 0,
        "finish credential application jobs before TLS-pair migration"
    );
    let mut converted = BTreeMap::<String, String>::new();
    let mut current = std::collections::BTreeSet::new();
    for row in sqlx::query("SELECT id,name,desired_config_enc,deployed_config_enc FROM nodes")
        .fetch_all(&mut *tx)
        .await?
    {
        let node: String = row.get("id");
        let name: String = row.get("name");
        for field in ["desired_config_enc", "deployed_config_enc"] {
            if let Some(cipher) = row.get::<Option<String>, _>(field) {
                let mut config: Value = serde_json::from_str(&secrets.decrypt(&cipher)?)?;
                consolidate_config(&mut tx, secrets, &name, &mut config, &mut converted).await?;
                if field == "desired_config_enc" {
                    current.extend(super::references(&config)?.into_iter().map(|(_, r)| r.id));
                }
                let query = format!("UPDATE nodes SET {field}=$1 WHERE id=$2");
                sqlx::query(&query)
                    .bind(secrets.encrypt(&config.to_string())?)
                    .bind(&node)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        for version in sqlx::query("SELECT id,config_enc FROM config_versions WHERE node_id=$1")
            .bind(&node)
            .fetch_all(&mut *tx)
            .await?
        {
            let mut value: Value =
                serde_json::from_str(&secrets.decrypt(&version.get::<String, _>("config_enc"))?)?;
            let config = if value.get("server_config").is_some() {
                value.get_mut("server_config").expect("present")
            } else {
                &mut value
            };
            consolidate_config(&mut tx, secrets, &name, config, &mut converted).await?;
            let plain = value.to_string();
            sqlx::query("UPDATE config_versions SET config_enc=$1,content_sha256=$2 WHERE id=$3")
                .bind(secrets.encrypt(&plain)?)
                .bind(hex::encode(Sha256::digest(plain.as_bytes())))
                .bind(version.get::<String, _>("id"))
                .execute(&mut *tx)
                .await?;
        }
    }
    for id in converted.values() {
        sqlx::query("UPDATE credentials SET archived=$1 WHERE id=$2")
            .bind(!current.contains(id))
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    // The original database backup is the rollback boundary for removed types.
    // Keep unused CA certificates; obsolete standalone private keys are removed.
    for row in sqlx::query("SELECT c.id,v.payload_enc FROM credentials c JOIN credential_versions v ON v.credential_id=c.id AND v.version=c.latest_version WHERE c.kind='certificate'").fetch_all(&mut *tx).await? {
        let payload: Value = serde_json::from_str(&secrets.decrypt(&row.get::<String,_>("payload_enc"))?)?;
        if super::validate_payload("ca_certificate", &payload).is_ok() {
            sqlx::query("UPDATE credentials SET kind='ca_certificate' WHERE id=$1").bind(row.get::<String,_>("id")).execute(&mut *tx).await?;
        }
    }
    sqlx::query("DELETE FROM credential_batches WHERE credential_id IN (SELECT id FROM credentials WHERE kind IN ('certificate','private_key'))").execute(&mut *tx).await?;
    sqlx::query("DELETE FROM credentials WHERE kind IN ('certificate','private_key')")
        .execute(&mut *tx)
        .await?;
    sqlx::query("ALTER TABLE credentials DROP CONSTRAINT credentials_kind_check")
        .execute(&mut *tx)
        .await?;
    sqlx::query("ALTER TABLE credentials ADD CONSTRAINT credentials_kind_check CHECK (kind IN ('ssh_private_key','ssh_password','tls_identity','ca_certificate','ech_key','dns','api_token'))").execute(&mut *tx).await?;
    sqlx::query("INSERT INTO credential_migrations(name,completed_at) VALUES('tls_pairs_v1',$1)")
        .bind(Utc::now())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn material_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    secrets: &SecretBox,
    uri: &str,
) -> Result<(Reference, String, Value, Value)> {
    let r = Reference::parse(uri)?;
    let row = sqlx::query("SELECT c.kind,c.owner_user_id,v.payload_enc,v.artifact_names FROM credentials c JOIN credential_versions v ON v.credential_id=c.id WHERE c.id=$1 AND v.version=$2")
        .bind(&r.id).bind(r.version).fetch_one(&mut **tx).await?;
    anyhow::ensure!(
        row.get::<Option<String>, _>("owner_user_id").is_none(),
        "node TLS cannot use a user-owned identity"
    );
    let payload = serde_json::from_str(&secrets.decrypt(&row.get::<String, _>("payload_enc"))?)?;
    Ok((r, row.get("kind"), payload, row.get("artifact_names")))
}

fn artifact_name(r: &Reference, artifacts: &Value) -> String {
    artifacts
        .get(&r.field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}-{}-{}", r.id, r.version, r.field))
}

async fn consolidate_config(
    tx: &mut Transaction<'_, Postgres>,
    secrets: &SecretBox,
    name: &str,
    config: &mut Value,
    converted: &mut BTreeMap<String, String>,
) -> Result<()> {
    if let (Some(cert_uri), Some(key_uri)) = (
        config.pointer("/tls/cert").and_then(Value::as_str),
        config.pointer("/tls/key").and_then(Value::as_str),
    ) {
        let (cert, cert_kind, cert_payload, cert_files) =
            material_in_tx(tx, secrets, cert_uri).await?;
        let (key, key_kind, key_payload, key_files) = material_in_tx(tx, secrets, key_uri).await?;
        if !(cert_kind == "tls_identity"
            && key_kind == "tls_identity"
            && cert.id == key.id
            && cert.version == key.version
            && cert.field == "certificate"
            && key.field == "private_key")
        {
            anyhow::ensure!(
                matches!(
                    cert_kind.as_str(),
                    "certificate" | "ca_certificate" | "tls_identity"
                ) && matches!(key_kind.as_str(), "private_key" | "tls_identity"),
                "invalid legacy TLS material kinds"
            );
            let signature = format!("pair:{cert_uri}|{key_uri}");
            let id = if let Some(id) = converted.get(&signature) {
                id.clone()
            } else {
                let payload = json!({"certificate":super::text(&cert_payload,&cert.field)?,"private_key":super::text(&key_payload,&key.field)?});
                let metadata = super::validate_payload("tls_identity", &payload)?;
                let id=insert(tx,secrets,&format!("{name} · TLS 证书对"),"tls_identity",None,&payload,&metadata,
                    &json!({"certificate":artifact_name(&cert,&cert_files),"private_key":artifact_name(&key,&key_files)})).await?;
                converted.insert(signature, id.clone());
                id
            };
            *config.pointer_mut("/tls/cert").expect("present") = json!(
                Reference {
                    id: id.clone(),
                    version: 1,
                    field: "certificate".into()
                }
                .uri()
            );
            *config.pointer_mut("/tls/key").expect("present") = json!(
                Reference {
                    id,
                    version: 1,
                    field: "private_key".into()
                }
                .uri()
            );
        }
    }
    if let Some(uri) = config.pointer("/tls/clientCA").and_then(Value::as_str) {
        let (r, kind, payload, artifacts) = material_in_tx(tx, secrets, uri).await?;
        if kind == "certificate" {
            super::validate_payload("ca_certificate", &payload)?;
            sqlx::query("UPDATE credentials SET kind='ca_certificate' WHERE id=$1")
                .bind(&r.id)
                .execute(&mut **tx)
                .await?;
        }
        let _ = artifacts;
    }
    Ok(())
}
