use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use super::{RecordInput, hostname};
use crate::{
    api::{dns::zone_for_write, enqueue_job_in_tx, supersede_queued_syncs_in_tx},
    error::ApiError,
    security::SecretBox,
    state::AppState,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
    pub idempotency_key: String,
    pub zone_id: String,
    pub mode: String,
    pub prefix: Option<String>,
    pub hostname: Option<String>,
    pub ipv4: Option<String>,
    pub ipv6: Option<String>,
    #[serde(default)]
    pub record_ids: Vec<String>,
}
pub struct Prepared {
    pub hostname: String,
    pub records: Vec<RecordInput>,
    pub existing: Vec<String>,
}

pub async fn prepare(
    state: &AppState,
    node: &str,
    input: &Allocation,
) -> Result<Prepared, ApiError> {
    let zone=sqlx::query("SELECT z.*,c.credential_version,c.status FROM dns_zones z JOIN dns_connections c ON c.id=z.connection_id WHERE z.id=$1")
        .bind(&input.zone_id).fetch_optional(&state.pool).await?.ok_or_else(||ApiError::not_found("DNS zone"))?;
    if !zone.get::<bool, _>("enabled") || zone.get::<String, _>("status") != "verified" {
        return Err(ApiError::bad_request(
            "enable a zone from a verified connection",
        ));
    }
    let zone_name: String = zone.get("name");
    if input.mode == "existing" {
        if input.record_ids.is_empty() || input.record_ids.len() > 2 {
            return Err(ApiError::bad_request(
                "select one CNAME or one/two A/AAAA records",
            ));
        }
        let mut name = None::<String>;
        let mut types = std::collections::BTreeSet::new();
        for id in &input.record_ids {
            let record=sqlx::query("SELECT name,record_type,proxied,state FROM dns_records WHERE id=$1 AND zone_id=$2 AND deleted_at IS NULL")
                .bind(id).bind(&input.zone_id).fetch_optional(&state.pool).await?.ok_or_else(||ApiError::not_found("DNS record"))?;
            let host: String = record.get("name");
            let kind: String = record.get("record_type");
            if name.as_ref().is_some_and(|n| n != &host)
                || !types.insert(kind.clone())
                || !matches!(kind.as_str(), "A" | "AAAA" | "CNAME")
                || record.get::<bool, _>("proxied")
                || record.get::<String, _>("state") != "synced"
            {
                return Err(ApiError::bad_request(
                    "choose synced DNS-only records with the same hostname and distinct types",
                ));
            }
            name = Some(host);
        }
        if types.contains("CNAME") && types.len() != 1 {
            return Err(ApiError::bad_request(
                "CNAME cannot be combined with A/AAAA",
            ));
        }
        return Ok(Prepared {
            hostname: name.unwrap(),
            records: vec![],
            existing: input.record_ids.clone(),
        });
    }
    let host = if input.mode == "auto" {
        let prefix = input
            .prefix
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or("node");
        let prefix = hostname(prefix).map_err(|e| ApiError::bad_request(e.to_string()))?;
        if prefix.contains('.') || prefix.len() > 20 {
            return Err(ApiError::bad_request(
                "prefix must be a single DNS label of at most 20 characters",
            ));
        }
        let provider = super::provider(
            &state.pool,
            &state.secrets,
            &zone.get::<String, _>("connection_id"),
            zone.get("credential_version"),
        )
        .await
        .map_err(|_| ApiError::bad_request("DNS connection could not be loaded"))?;
        let remote = provider
            .records(&zone.get::<String, _>("provider_zone_id"))
            .await
            .map_err(|_| {
                ApiError::bad_request("could not check existing remote record names; retry")
            })?;
        let compact = node.replace('-', "");
        let mut available = None;
        for length in [8, 12, 16, 32] {
            let candidate = format!("{prefix}-{}.{}", &compact[..length], zone_name);
            let local:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dns_records WHERE zone_id=$1 AND name=$2 AND deleted_at IS NULL) OR EXISTS(SELECT 1 FROM dns_bindings WHERE hostname=$2)")
                .bind(&input.zone_id).bind(&candidate).fetch_one(&state.pool).await?;
            if !local
                && !remote
                    .iter()
                    .any(|r| r["name"].as_str() == Some(&candidate))
            {
                available = Some(candidate);
                break;
            }
        }
        available.ok_or_else(|| ApiError::conflict("could not reserve an automatic domain"))?
    } else if input.mode == "manual" {
        hostname(input.hostname.as_deref().unwrap_or(""))
            .map_err(|e| ApiError::bad_request(e.to_string()))?
    } else {
        return Err(ApiError::bad_request(
            "allocation mode must be auto, manual or existing",
        ));
    };
    let mut records = Vec::new();
    for (kind, ip) in [("A", &input.ipv4), ("AAAA", &input.ipv6)] {
        if let Some(ip) = ip.as_deref().filter(|s| !s.trim().is_empty()) {
            let record = RecordInput {
                name: host.clone(),
                record_type: kind.into(),
                content: ip.into(),
                ttl: 1,
                proxied: false,
            }
            .normalize(&zone_name)
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
            if !public_ip(&record.content) {
                return Err(ApiError::bad_request(
                    "node DNS targets must be public IP addresses",
                ));
            }
            records.push(record);
        }
    }
    if records.is_empty() {
        return Err(ApiError::bad_request(
            "enter at least one public IPv4 or IPv6 address",
        ));
    }
    Ok(Prepared {
        hostname: host,
        records,
        existing: vec![],
    })
}
fn public_ip(value: &str) -> bool {
    match value.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !ip.is_documentation()
                && ip.octets()[0] != 0
                && ip.octets()[0] < 240
                && !(ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1]))
                && !(ip.octets()[0] == 198 && (18..20).contains(&ip.octets()[1]))
                && !(ip.octets()[0] == 192 && ip.octets()[1] == 0 && ip.octets()[2] == 0)
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && (ip.segments()[0] & 0xe000) == 0x2000
                && (ip.segments()[0] & 0xfe00) != 0xfc00
                && (ip.segments()[0] & 0xffc0) != 0xfe80
                && !(ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8)
        }
        _ => false,
    }
}

pub async fn bind_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    input: &Allocation,
    prepared: &Prepared,
    request: &Value,
) -> Result<Value, ApiError> {
    let zone = zone_for_write(tx, &input.zone_id).await?;
    let used: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM dns_bindings WHERE hostname=$1 AND node_id<>$2)",
    )
    .bind(&prepared.hostname)
    .bind(node)
    .fetch_one(&mut **tx)
    .await?;
    if used {
        return Err(ApiError::conflict(
            "hostname is already bound to another node",
        ));
    }
    let mut record_ids = prepared.existing.clone();
    let mut jobs = Vec::new();
    for (index, record) in prepared.records.iter().enumerate() {
        let key = format!("{}:{index}", input.idempotency_key);
        let result = crate::api::dns::create_record_in_tx(
            tx,
            &zone,
            &key,
            &json!({"node_id":node,"record":record}),
            record,
            Some(node),
        )
        .await?;
        record_ids.push(
            result["resource_id"]
                .as_str()
                .ok_or_else(ApiError::internal)?
                .to_owned(),
        );
        jobs.push(result["job_id"].clone());
    }
    // Recheck selected records under the serialized write transaction after preflight.
    for id in &prepared.existing {
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dns_records WHERE id=$1 AND zone_id=$2 AND name=$3 AND state='synced' AND NOT proxied AND desired IS NULL AND deleted_at IS NULL)")
            .bind(id).bind(&input.zone_id).bind(&prepared.hostname).fetch_one(&mut **tx).await?;
        if !valid {
            return Err(ApiError::conflict("selected DNS record changed; reload"));
        }
    }
    sqlx::query("INSERT INTO dns_bindings(node_id,zone_id,hostname,record_ids) VALUES($1,$2,$3,$4) ON CONFLICT(node_id) DO UPDATE SET zone_id=excluded.zone_id,hostname=excluded.hostname,record_ids=excluded.record_ids,revision=dns_bindings.revision+1,updated_at=now()")
        .bind(node).bind(&input.zone_id).bind(&prepared.hostname).bind(json!(record_ids)).execute(&mut **tx).await?;
    let operation = Uuid::new_v4().to_string();
    let result =
        json!({"node_id":node,"hostname":prepared.hostname,"record_ids":record_ids,"job_ids":jobs});
    sqlx::query("INSERT INTO dns_operations(id,idempotency_key,request_sha256,connection_id,credential_version,resource_type,resource_id,action,payload,result,applied_at) VALUES($1,$2,$3,$4,$5,'node',$6,'allocation',$7,$8,now())")
        .bind(&operation).bind(&input.idempotency_key).bind(super::request_hash(request)).bind(zone.get::<String,_>("connection_id"))
        .bind(zone.get::<i64,_>("credential_version")).bind(node).bind(request).bind(&result).execute(&mut **tx).await?;
    super::audit(tx, "node.bound", node, result.clone()).await?;
    Ok(result)
}

pub async fn change_hostname_in_tx(
    state: &AppState,
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    revision: i64,
    host: &str,
) -> Result<(), ApiError> {
    let current =
        sqlx::query("SELECT desired_revision,deployed_revision,state FROM nodes WHERE id=$1")
            .bind(node)
            .fetch_one(&mut **tx)
            .await?;
    if current.get::<i64, _>("desired_revision") != revision
        || matches!(
            current.get::<String, _>("state").as_str(),
            "deleting" | "delete_failed"
        )
    {
        return Err(ApiError::conflict("node changed or is being deleted"));
    }
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM jobs WHERE node_id=$1 AND status='running')",
    )
    .bind(node)
    .fetch_one(&mut **tx)
    .await?;
    if active {
        return Err(ApiError::conflict(
            "wait for the running node task before changing its domain",
        ));
    }
    let cipher: String = sqlx::query_scalar(
        "SELECT config_enc FROM config_versions WHERE node_id=$1 AND revision=$2",
    )
    .bind(node)
    .bind(revision)
    .fetch_one(&mut **tx)
    .await?;
    let mut snapshot: Value =
        serde_json::from_str(&state.secrets.decrypt(&cipher)?).map_err(|_| ApiError::internal())?;
    snapshot["public_host"] = json!(host);
    let next = revision + 1;
    sqlx::query("UPDATE nodes SET public_host=$1,desired_revision=$2,updated_at=now() WHERE id=$3")
        .bind(host)
        .bind(next)
        .bind(node)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES($1,$2,$3,$4,$5,now())")
        .bind(Uuid::new_v4().to_string()).bind(node).bind(next).bind(state.secrets.encrypt(&snapshot.to_string())?).bind(super::request_hash(&snapshot)).execute(&mut **tx).await?;
    Ok(())
}

pub async fn ensure_ready(
    pool: &PgPool,
    _secrets: &SecretBox,
    node: &str,
    host: &str,
    historical: bool,
) -> Result<()> {
    // Historical domains may have been unbound; if known, validate their retained records too.
    let rows=sqlx::query("SELECT r.id,r.state,r.proxied,r.record_type FROM dns_records r WHERE r.name=$1 AND r.deleted_at IS NULL AND r.record_type IN ('A','AAAA','CNAME') AND ($3 OR EXISTS(SELECT 1 FROM dns_bindings b WHERE b.node_id=$2 AND b.hostname=$1) OR r.origin='hysteriax')")
        .bind(host).bind(node).bind(historical).fetch_all(pool).await?;
    let managed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM dns_bindings WHERE node_id=$1 AND hostname=$2)",
    )
    .bind(node)
    .bind(host)
    .fetch_one(pool)
    .await?;
    let known_history = historical && sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM dns_records WHERE name=$1 AND record_type IN ('A','AAAA','CNAME'))")
        .bind(host).fetch_one(pool).await?;
    if (managed || known_history) && rows.is_empty() {
        bail!("node DNS records are missing; refresh and repair before deployment");
    }
    let mut types = std::collections::BTreeSet::new();
    for row in &rows {
        if !types.insert(row.get::<String, _>("record_type")) {
            bail!(
                "node hostname has multiple records of the same address type; resolve the ambiguity before deployment"
            );
        }
    }
    if types.contains("CNAME") && types.len() > 1 {
        bail!("node hostname has conflicting CNAME and address records");
    }
    for row in rows {
        if row.get::<String, _>("state") != "synced" || row.get::<bool, _>("proxied") {
            bail!("node DNS records are pending or proxied; repair before deployment");
        }
        let detail = super::probe::check(pool, &row.get::<String, _>("id")).await?;
        if detail["status"] != "verified" {
            bail!("node DNS resolution is not verified; retry after propagation");
        }
    }
    Ok(())
}

pub async fn queue_sync(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<(), ApiError> {
    let row = sqlx::query("SELECT desired_revision,deployed_revision FROM nodes WHERE id=$1")
        .bind(node)
        .fetch_one(&mut **tx)
        .await?;
    if row.get::<Option<i64>, _>("deployed_revision").is_some() {
        supersede_queued_syncs_in_tx(tx, node).await?;
        enqueue_job_in_tx(tx, "sync", Some(node), Some(row.get("desired_revision"))).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocation_targets_reject_local_and_documentation_addresses() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "192.0.2.1",
            "100.64.0.1",
            "198.18.0.1",
            "192.0.0.1",
            "::ffff:8.8.8.8",
            "fec0::1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(!public_ip(value));
        }
        assert!(public_ip("8.8.8.8"));
        assert!(public_ip("2606:4700:4700::1111"));
    }
}
