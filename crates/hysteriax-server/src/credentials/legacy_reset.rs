//! One-time destructive protocol transition, inside the startup conversion transaction.
//! Never decrypt or validate client identities that this transition will discard.
use std::collections::BTreeSet;

use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::security::SecretBox;

const MARKER: &str = "managed_mtls_reset_v1";

fn server_config(value: &mut Value) -> &mut Value {
    if value.get("server_config").is_some() {
        value
            .get_mut("server_config")
            .expect("present server config")
    } else {
        value
    }
}

fn strip_legacy(value: &mut Value) -> (bool, bool) {
    let config = server_config(value);
    let client_ca = config.pointer("/tls/clientCA");
    let ca_enabled = client_ca
        .is_some_and(|ca| !ca.is_null() && ca.as_str().is_none_or(|text| !text.trim().is_empty()));
    let mode = config.pointer("/mtls/mode").and_then(Value::as_str);
    let legacy_mode = config.get("mtls").is_some() && !matches!(mode, Some("off" | "managed"));
    let enabled = ca_enabled || legacy_mode && !matches!(mode, Some("disabled") | None);
    let changed = client_ca.is_some() || legacy_mode;
    if changed {
        if let Some(tls) = config.get_mut("tls").and_then(Value::as_object_mut) {
            tls.remove("clientCA");
        }
        if legacy_mode || enabled {
            config["mtls"] = json!({"mode":"off"});
        }
    }
    (changed, enabled)
}

pub async fn reset(tx: &mut Transaction<'_, Postgres>, secrets: &SecretBox) -> Result<()> {
    let done: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credential_migrations WHERE name=$1)")
            .bind(MARKER)
            .fetch_one(&mut **tx)
            .await?;
    if done {
        return Ok(());
    }
    let mut blocked = BTreeSet::new();
    let mut retained_credentials = BTreeSet::new();
    let mut retained_versions = BTreeSet::<(String, i64)>::new();
    let mut obsolete_histories = Vec::<String>::new();
    let nodes = sqlx::query(
        "SELECT id,desired_config_enc,deployed_config_enc,ssh_credential_id,ssh_credential_version FROM nodes",
    )
    .fetch_all(&mut **tx)
    .await?;
    for node in nodes {
        let id: String = node.get("id");
        if let Some(ssh) = node.get::<Option<String>, _>("ssh_credential_id") {
            if let Some(version) = node.get::<Option<i64>, _>("ssh_credential_version") {
                retained_versions.insert((ssh.clone(), version));
            }
            retained_credentials.insert(ssh);
        }
        for field in ["desired_config_enc", "deployed_config_enc"] {
            let Some(cipher) = node.get::<Option<String>, _>(field) else {
                continue;
            };
            let mut value: Value = serde_json::from_str(&secrets.decrypt(&cipher)?)?;
            let (changed, enabled) = strip_legacy(&mut value);
            if enabled {
                blocked.insert(id.clone());
            }
            if changed {
                let query = format!("UPDATE nodes SET {field}=$2 WHERE id=$1");
                sqlx::query(sqlx::AssertSqlSafe(query.as_str()))
                    .bind(&id)
                    .bind(secrets.encrypt(&value.to_string())?)
                    .execute(&mut **tx)
                    .await?;
            }
            for (_, reference) in super::references(&value)? {
                retained_versions.insert((reference.id.clone(), reference.version));
                retained_credentials.insert(reference.id);
            }
        }
    }
    for row in sqlx::query("SELECT id,config_enc FROM config_versions")
        .fetch_all(&mut **tx)
        .await?
    {
        let id: String = row.get("id");
        let mut value: Value =
            serde_json::from_str(&secrets.decrypt(&row.get::<String, _>("config_enc"))?)?;
        let (changed, enabled) = strip_legacy(&mut value);
        if changed {
            if enabled {
                obsolete_histories.push(id.clone());
                value["legacy_mtls_reset"] = json!(true);
            }
            let plain = value.to_string();
            sqlx::query("UPDATE config_versions SET config_enc=$2,content_sha256=$3,deployed_success=CASE WHEN $4 THEN FALSE ELSE deployed_success END WHERE id=$1").bind(id).bind(secrets.encrypt(&plain)?).bind(hex::encode(Sha256::digest(plain.as_bytes()))).bind(enabled).execute(&mut **tx).await?;
        }
        for (_, reference) in super::references(&value)? {
            retained_versions.insert((reference.id.clone(), reference.version));
            retained_credentials.insert(reference.id);
        }
    }
    let blocked: Vec<String> = blocked.into_iter().collect();
    sqlx::query("UPDATE nodes SET mtls_state='needs_configuration',mtls_recovery_configured=FALSE,mtls_error='Legacy mTLS was reset; explicitly configure off or a managed CA and deploy recovery' WHERE id=ANY($1)").bind(&blocked).execute(&mut **tx).await?;
    // Only user-owned client TLS identities belong to the removed protocol.
    // Keep identities still referenced by another retained configuration/feature.
    for (id,version) in sqlx::query_as::<_,(String,i64)>("SELECT credential_id,credential_version FROM dns_connections WHERE credential_id IS NOT NULL").fetch_all(&mut **tx).await? {
        retained_credentials.insert(id.clone());
        retained_versions.insert((id,version));
    }
    let old: Vec<String> = sqlx::query_scalar("SELECT c.id FROM credentials c WHERE c.kind='tls_identity' AND c.owner_user_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM mtls_user_identities i WHERE i.credential_id=c.id) AND NOT EXISTS(SELECT 1 FROM mtls_probe_versions p WHERE p.credential_id=c.id)").fetch_all(&mut **tx).await?;
    let discarded: Vec<String> = old
        .into_iter()
        .filter(|id| !retained_credentials.contains(id))
        .collect();
    let cleared_bindings = sqlx::query("UPDATE node_assignments SET mtls_credential_id=NULL,mtls_credential_version=NULL WHERE mtls_credential_id IS NOT NULL").execute(&mut **tx).await?.rows_affected();
    // Discard old inline user keys without ever trying to decrypt them. The format
    // migration later removes these columns after validating retained passwords.
    let inline: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='node_assignments' AND column_name='client_certificate_enc')").fetch_one(&mut **tx).await?;
    let mut discarded_inline = 0;
    if inline {
        discarded_inline = sqlx::query_scalar::<_,i64>("SELECT count(*) FROM node_assignments WHERE client_certificate_enc IS NOT NULL OR client_private_key_enc IS NOT NULL").fetch_one(&mut **tx).await?;
        sqlx::query(
            "UPDATE node_assignments SET client_certificate_enc=NULL,client_private_key_enc=NULL",
        )
        .execute(&mut **tx)
        .await?;
    }
    let jobs: Vec<String> = sqlx::query_scalar("SELECT id FROM jobs WHERE kind='credential-apply' AND (payload_json->>'mode'='mtls' OR payload_json->>'credential_id'=ANY($1)) OR kind IN ('deploy','sync','rollback') AND (node_id=ANY($2) OR EXISTS(SELECT 1 FROM config_versions v WHERE v.id=ANY($3) AND v.node_id=jobs.node_id AND v.revision=jobs.target_revision))").bind(&discarded).bind(&blocked).bind(&obsolete_histories).fetch_all(&mut **tx).await?;
    sqlx::query("UPDATE jobs SET status=CASE WHEN status IN ('queued','running','waiting_confirmation') THEN 'cancelled' ELSE status END,stage='legacy_mtls_reset',payload_json='{}',result_json=NULL,logs_json='[]',error_message='Legacy mTLS protocol was reset',updated_at=now(),finished_at=coalesce(finished_at,now()) WHERE id=ANY($1)").bind(&jobs).execute(&mut **tx).await?;
    sqlx::query("DELETE FROM job_events WHERE job_id=ANY($1)")
        .bind(&jobs)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM credential_batch_items WHERE job_id=ANY($1)")
        .bind(&jobs)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM credential_batches WHERE credential_id=ANY($1)")
        .bind(&discarded)
        .execute(&mut **tx)
        .await?;
    if !discarded.is_empty() {
        super::reference_index::flush_pending_on(tx, secrets).await?;
    }
    sqlx::query("DELETE FROM credentials WHERE id=ANY($1)")
        .bind(&discarded)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM deployment_probe_tokens WHERE node_id=ANY($1)")
        .bind(&blocked)
        .execute(&mut **tx)
        .await?;
    // Durable kicks survive offline nodes and resume through the existing trusted-SSH recovery path.
    let pairs: Vec<(String, String)> =
        sqlx::query_as("SELECT node_id,user_id FROM node_assignments WHERE node_id=ANY($1)")
            .bind(&blocked)
            .fetch_all(&mut **tx)
            .await?;
    for (node, user) in pairs {
        crate::api::enqueue_job_with_payload_in_tx(
            tx,
            "kick",
            Some(&node),
            None,
            json!({"user_id":user,"kick_reason":"legacy_mtls_reset"}),
        )
        .await
        .map_err(|error| anyhow::anyhow!("queue legacy mTLS disconnect: {}", error.message))?;
    }
    if !blocked.is_empty()
        || !discarded.is_empty()
        || !obsolete_histories.is_empty()
        || !jobs.is_empty()
        || discarded_inline > 0
        || cleared_bindings > 0
    {
        sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,'system:upgrade','mtls.legacy_reset','migration',$2,$3,now())").bind(Uuid::new_v4().to_string()).bind(MARKER).bind(json!({"blocked_node_ids":blocked,"discarded_user_identity_count":discarded.len(),"discarded_inline_identity_count":discarded_inline,"cleared_binding_count":cleared_bindings,"obsolete_history_count":obsolete_histories.len(),"cancelled_or_sanitized_job_count":jobs.len()})).execute(&mut **tx).await?;
    }
    // Validate retained exact versions, including an already-converted database.
    // Discarded user material has been removed without decryption at this point.
    for (id, version) in retained_versions {
        let row = sqlx::query("SELECT c.kind,v.payload_enc FROM credentials c JOIN credential_versions v ON v.credential_id=c.id WHERE c.id=$1 AND v.version=$2").bind(&id).bind(version).fetch_one(&mut **tx).await?;
        let payload: Value =
            serde_json::from_str(&secrets.decrypt(&row.get::<String, _>("payload_enc"))?)?;
        let kind: String = row.get("kind");
        match kind.as_str() {
            "certificate" | "private_key" => {
                crate::api::resources::validate_text_resource(
                    &kind,
                    super::text(&payload, "content")?.as_bytes(),
                )
                .map_err(|_| anyhow::anyhow!("retained legacy TLS resource is invalid"))?;
            }
            "ca_identity" => {
                super::ca::Authority::from_payload(&payload, chrono::Utc::now())?;
            }
            _ => {
                super::validate_payload(&kind, &payload)?;
            }
        }
    }
    sqlx::query("INSERT INTO credential_migrations(name,completed_at) VALUES($1,now())")
        .bind(MARKER)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests;
