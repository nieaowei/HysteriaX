pub mod binding;
pub mod probe;
pub mod provider;
pub mod rotation;
pub mod worker;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::{
    api::enqueue_job_with_payload_in_tx, credentials, error::ApiError, security::SecretBox,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordInput {
    pub name: String,
    pub record_type: String,
    pub content: String,
    #[serde(default = "auto_ttl")]
    pub ttl: i64,
    #[serde(default)]
    pub proxied: bool,
}
fn auto_ttl() -> i64 {
    1
}

pub fn hostname(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('.').to_ascii_lowercase();
    if value.is_empty()
        || value.len() > 253
        || !value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        bail!("domain must contain valid DNS labels (use punycode for international domains)");
    }
    Ok(value)
}
impl RecordInput {
    pub fn normalize(mut self, zone: &str) -> Result<Self> {
        let zone = hostname(zone)?;
        self.name = if self.name.trim() == "@" {
            zone.clone()
        } else {
            hostname(&self.name)?
        };
        if self.name != zone && !self.name.ends_with(&format!(".{zone}")) {
            bail!("record name must belong to the selected zone");
        }
        self.record_type = self.record_type.to_ascii_uppercase();
        self.content = match self.record_type.as_str() {
            "A" => self
                .content
                .trim()
                .parse::<std::net::Ipv4Addr>()
                .context("invalid IPv4 address")?
                .to_string(),
            "AAAA" => self
                .content
                .trim()
                .parse::<std::net::Ipv6Addr>()
                .context("invalid IPv6 address")?
                .to_string(),
            "CNAME" => {
                let target = hostname(&self.content)?;
                if target == self.name {
                    bail!("CNAME cannot point to itself");
                }
                target
            }
            _ => bail!("only A, AAAA and CNAME records can be edited"),
        };
        if self.ttl != 1 && !(60..=86400).contains(&self.ttl) {
            bail!("TTL must be auto (1) or 60–86400 seconds");
        }
        Ok(self)
    }
    pub fn remote(&self) -> Value {
        json!({"name":self.name,"type":self.record_type,"content":self.content,"ttl":self.ttl,"proxied":self.proxied})
    }
}

pub async fn provider(
    pool: &PgPool,
    secrets: &SecretBox,
    connection: &str,
    version: i64,
) -> Result<provider::Cloudflare> {
    let credential: String =
        sqlx::query_scalar("SELECT credential_id FROM dns_connections WHERE id=$1")
            .bind(connection)
            .fetch_one(pool)
            .await?;
    cloudflare_credential(pool, secrets, &credential, version, true).await
}
pub async fn cloudflare_credential(
    pool: &PgPool,
    secrets: &SecretBox,
    id: &str,
    version: i64,
    allow_archived: bool,
) -> Result<provider::Cloudflare> {
    let (kind, owner, archived, payload, _) = credentials::load(pool, secrets, id, version).await?;
    if kind != "dns"
        || owner.is_some()
        || (archived && !allow_archived)
        || payload["provider"] != "cloudflare"
    {
        bail!("a Cloudflare DNS credential is required");
    }
    let token = payload["config"]["cloudflare_api_token"]
        .as_str()
        .context("Cloudflare token is missing")?;
    provider::Cloudflare::new(token.to_owned())
}

pub fn connection_json(row: &PgRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"name":row.get::<String,_>("name"),"provider":"cloudflare",
        "credential_id":row.get::<String,_>("credential_id"),"credential_version":row.get::<i64,_>("credential_version"),
        "revision":row.get::<i64,_>("revision"),"status":row.get::<String,_>("status"),
        "verified_at":row.get::<Option<DateTime<Utc>>,_>("verified_at"),
        "created_at":row.get::<DateTime<Utc>,_>("created_at"),"updated_at":row.get::<DateTime<Utc>,_>("updated_at")})
}
pub fn zone_json(row: &PgRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"connection_id":row.get::<String,_>("connection_id"),
        "provider_zone_id":row.get::<String,_>("provider_zone_id"),"name":row.get::<String,_>("name"),
        "enabled":row.get::<bool,_>("enabled"),"revision":row.get::<i64,_>("revision"),"synced_at":row.get::<Option<DateTime<Utc>>,_>("synced_at")})
}
pub fn record_json(row: &PgRow) -> Value {
    json!({"id":row.get::<String,_>("id"),"zone_id":row.get::<String,_>("zone_id"),
        "provider_record_id":row.get::<Option<String>,_>("provider_record_id"),"name":row.get::<String,_>("name"),
        "record_type":row.get::<String,_>("record_type"),"content":row.get::<String,_>("content"),
        "ttl":row.get::<i64,_>("ttl"),"proxied":row.get::<bool,_>("proxied"),"origin":row.get::<String,_>("origin"),
        "revision":row.get::<i64,_>("revision"),"remote_snapshot":row.get::<Option<Value>,_>("remote_snapshot"),
        "desired":row.get::<Option<Value>,_>("desired"),"state":row.get::<String,_>("state"),
        "resolution_status":row.get::<String,_>("resolution_status"),"resolution_detail":row.get::<Option<Value>,_>("resolution_detail"),
        "checked_at":row.get::<Option<DateTime<Utc>>,_>("checked_at"),"updated_at":row.get::<DateTime<Utc>,_>("updated_at")})
}

pub fn request_hash(payload: &Value) -> String {
    hex::encode(Sha256::digest(payload.to_string().as_bytes()))
}

pub async fn existing_operation(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    payload: &Value,
) -> Result<Option<Value>, ApiError> {
    if key.trim().is_empty() || key.len() > 200 {
        return Err(ApiError::bad_request(
            "idempotency_key must contain 1–200 characters",
        ));
    }
    let row = sqlx::query(
        "SELECT id,job_id,resource_id,request_sha256 FROM dns_operations WHERE idempotency_key=$1",
    )
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(row) = row {
        if row.get::<String, _>("request_sha256") != request_hash(payload) {
            return Err(ApiError::conflict(
                "idempotency key was already used for a different request",
            ));
        }
        return Ok(Some(
            json!({"operation_id":row.get::<String,_>("id"),"job_id":row.get::<Option<String>,_>("job_id"),"resource_id":row.get::<String,_>("resource_id")}),
        ));
    }
    Ok(None)
}

pub struct Operation<'a> {
    pub key: &'a str,
    pub request: &'a Value,
    pub connection: &'a str,
    pub version: i64,
    pub resource_type: &'a str,
    pub resource: &'a str,
    pub resource_name: &'a str,
    pub resource_key: String,
    pub action: &'a str,
    pub payload: Value,
    pub node: Option<&'a str>,
}
pub async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    op: Operation<'_>,
) -> Result<Value, ApiError> {
    if let Some(previous) = existing_operation(tx, op.key, op.request).await? {
        return Ok(previous);
    }
    let id = Uuid::new_v4().to_string();
    let kind = format!("dns-{}", op.action);
    let job =
        enqueue_job_with_payload_in_tx(tx, &kind, op.node, None, json!({"dns_operation_id":id}))
            .await?;
    sqlx::query("UPDATE jobs SET resource_key=$1,resource_type=$2,resource_id=$3,resource_name=$4 WHERE id=$5")
        .bind(&op.resource_key).bind(op.resource_type).bind(op.resource).bind(op.resource_name).bind(&job).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO dns_operations(id,idempotency_key,request_sha256,job_id,connection_id,credential_version,resource_type,resource_id,action,payload) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
        .bind(&id).bind(op.key).bind(request_hash(op.request)).bind(&job).bind(op.connection).bind(op.version).bind(op.resource_type).bind(op.resource).bind(op.action).bind(op.payload).execute(&mut **tx).await?;
    Ok(json!({"operation_id":id,"job_id":job,"resource_id":op.resource}))
}

pub async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    action: &str,
    id: &str,
    detail: Value,
) -> Result<()> {
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,'admin',$2,'dns',$3,$4,now())")
        .bind(Uuid::new_v4().to_string()).bind(format!("dns.{action}")).bind(id).bind(detail).execute(&mut **tx).await?;
    Ok(())
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    #[test]
    fn record_validation_is_zone_scoped_and_normalizes_addresses() {
        let input = RecordInput {
            name: "HK.Example.COM.".into(),
            record_type: "aaaa".into(),
            content: "2001:0db8::1".into(),
            ttl: 300,
            proxied: false,
        };
        let normalized = input.normalize("example.com").unwrap();
        assert_eq!(normalized.name, "hk.example.com");
        assert_eq!(normalized.content, "2001:db8::1");
        assert!(
            RecordInput {
                name: "evil-example.com".into(),
                ..normalized.clone()
            }
            .normalize("example.com")
            .is_err()
        );
        assert!(
            RecordInput {
                record_type: "TXT".into(),
                ..normalized
            }
            .normalize("example.com")
            .is_err()
        );
        assert!(hostname("a..example.com").is_err());
    }
}

#[cfg(test)]
pub(crate) mod tests;
