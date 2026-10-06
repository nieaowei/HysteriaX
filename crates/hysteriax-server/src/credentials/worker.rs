use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    api::enqueue_job_with_payload_in_tx,
    db,
    deployment::{JobInput, JobOutput},
    security::SecretBox,
    ssh::{self, FingerprintResult},
};

pub async fn apply(pool: &PgPool, secrets: &SecretBox, job: &JobInput) -> Result<JobOutput> {
    let batch = job.payload["batch_id"]
        .as_str()
        .context("missing credential batch")?;
    let id = job.payload["credential_id"]
        .as_str()
        .context("missing credential ID")?;
    let version = job.payload["version"]
        .as_i64()
        .context("missing credential version")?;
    let node = job.node_id.as_deref().context("missing node")?;
    let user = job.payload["user_id"].as_str().unwrap_or("");
    let item=sqlx::query("SELECT i.expected_revision,i.job_id,i.applied_at,i.apply_stage,c.latest_version,c.archived FROM credential_batch_items i JOIN credential_batches b ON b.id=i.batch_id JOIN credentials c ON c.id=b.credential_id WHERE i.batch_id=$1 AND i.node_id=$2 AND i.user_id=$3").bind(batch).bind(node).bind(user).fetch_optional(pool).await?.context("credential batch item is missing")?;
    if item
        .get::<Option<chrono::DateTime<Utc>>, _>("applied_at")
        .is_some()
    {
        return Ok(output(
            item.get::<String, _>("apply_stage").as_str(),
            json!({"credential_id":id,"version":version,"node_id":node,"already_applied":true,"followup_job_id":item.get::<String,_>("job_id")}),
        ));
    }
    if item.get::<String, _>("job_id") != job.id {
        bail!("credential batch item was replaced by a retry");
    }
    if item.get::<bool, _>("archived") || item.get::<i64, _>("latest_version") != version {
        bail!("credential update superseded or archived; this target was not applied");
    }
    let revision: i64 = item.get("expected_revision");
    let current = sqlx::query(
        "SELECT * FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed')",
    )
    .bind(node)
    .fetch_optional(pool)
    .await?
    .context("node is missing or being deleted")?;
    let (kind, owner, _, payload, _) = super::load(pool, secrets, id, version).await?;

    match job.payload["mode"].as_str() {
        Some("ssh") => {
            if current.get::<i64, _>("desired_revision") != revision
                || current
                    .get::<Option<String>, _>("ssh_credential_id")
                    .as_deref()
                    != Some(id)
            {
                bail!(
                    "node changed before SSH credential update; retry after reviewing current configuration"
                );
            }
            if owner.is_some() || !matches!(kind.as_str(), "ssh_private_key" | "ssh_password") {
                bail!("invalid SSH credential");
            }
            let mut candidate = super::ssh_node(pool, secrets, &current).await?;
            candidate.secret = payload["secret"]
                .as_str()
                .context("missing SSH secret")?
                .into();
            candidate.passphrase = payload["passphrase"].as_str().map(str::to_owned);
            match ssh::connect(&candidate).await.context("SSH verification failed; previous binding was retained, but its remote credential may already be invalid")? {
                FingerprintResult::Trusted(session) => {
                    ssh::inspect_connected(&session).await?;
                }
                FingerprintResult::NeedsConfirmation { .. } => {
                    bail!("confirm the node SSH host fingerprint before applying credentials")
                }
                FingerprintResult::Changed { .. } => {
                    bail!("SSH host key changed; credential was not applied")
                }
            }
            let mut tx = db::begin_write(pool).await?;
            verify_latest(&mut tx, id, version).await?;
            let changed=sqlx::query("UPDATE nodes SET ssh_credential_version=$1,desired_revision=desired_revision+1,updated_at=$2 WHERE id=$3 AND desired_revision=$4 AND ssh_credential_id=$5").bind(version).bind(Utc::now()).bind(node).bind(revision).bind(id).execute(&mut *tx).await?;
            if changed.rows_affected() != 1 {
                bail!("node changed during SSH verification; credential was not applied");
            }
            // SSH updates increment the optimistic revision but don't change the
            // proxy configuration; preserve a snapshot at the new revision.
            let snapshot = make_snapshot(secrets, &current, None)?;
            save_snapshot(&mut tx, secrets, node, revision + 1, &snapshot).await?;
            mark_applied(&mut tx, batch, node, user, "credential_applied").await?;
            tx.commit().await?;
            Ok(output(
                "credential_applied",
                json!({"credential_id":id,"version":version,"node_id":node,"ssh_verified":true}),
            ))
        }
        Some("config") => {
            if owner.is_some() {
                bail!("user-owned credentials cannot be deployed to nodes");
            }
            if current.get::<i64, _>("desired_revision") != revision {
                bail!(
                    "node changed before credential update; retry after reviewing current configuration"
                );
            }
            let mut config: Value = serde_json::from_str(
                &secrets.decrypt(&current.get::<String, _>("desired_config_enc"))?,
            )?;
            if !super::replace_version(&mut config, id, version)? {
                bail!("node no longer references this credential");
            }
            if kind == "dns" {
                let fields = payload["config"]
                    .as_object()
                    .context("missing DNS configuration")?;
                let replacement: serde_json::Map<String, Value> = fields
                    .keys()
                    .map(|field| {
                        (
                            field.clone(),
                            json!(
                                super::Reference {
                                    id: id.into(),
                                    version,
                                    field: field.clone()
                                }
                                .uri()
                            ),
                        )
                    })
                    .collect();
                *config
                    .pointer_mut("/acme/dns/config")
                    .context("node DNS reference is missing")? = Value::Object(replacement);
            }
            let (resolved, _) =
                crate::api::resources::resolve_config_resources(pool, secrets, node, &config)
                    .await
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "credential configuration could not be resolved or validated"
                        )
                    })?;
            crate::config::validate_server_options(&resolved).map_err(|_| {
                anyhow::anyhow!("new credential produces an invalid node configuration")
            })?;
            let snapshot = make_snapshot(secrets, &current, Some(&config))?;
            let mut tx = db::begin_write(pool).await?;
            verify_latest(&mut tx, id, version).await?;
            let changed=sqlx::query("UPDATE nodes SET desired_config_enc=$1,desired_revision=$2,updated_at=$3 WHERE id=$4 AND desired_revision=$5").bind(secrets.encrypt(&config.to_string())?).bind(revision+1).bind(Utc::now()).bind(node).bind(revision).execute(&mut *tx).await?;
            if changed.rows_affected() != 1 {
                bail!(
                    "node changed while applying credential; retry after reviewing current configuration"
                );
            }
            save_snapshot(&mut tx, secrets, node, revision + 1, &snapshot).await?;
            let sync=enqueue_job_with_payload_in_tx(&mut tx,"sync",Some(node),Some(revision+1),json!({"credential_batch_id":batch,"credential_id":id,"credential_version":version})).await.map_err(|_|anyhow::anyhow!("failed to enqueue credential follow-up job"))?;
            sqlx::query("UPDATE credential_batch_items SET job_id=$1,applied_at=now(),apply_stage='credential_deployment_queued' WHERE batch_id=$2 AND node_id=$3 AND user_id='' AND job_id=$4").bind(&sync).bind(batch).bind(node).bind(&job.id).execute(&mut *tx).await?;
            tx.commit().await?;
            Ok(output(
                "credential_deployment_queued",
                json!({"credential_id":id,"version":version,"sync_job_id":sync}),
            ))
        }
        Some("mtls") => {
            if kind != "tls_identity" || owner.as_deref() != Some(user) {
                bail!("mTLS credential is not owned by this user");
            }
            let mut tx = db::begin_write(pool).await?;
            verify_latest(&mut tx, id, version).await?;
            let revision:i64=sqlx::query_scalar("SELECT expected_revision FROM credential_batch_items WHERE batch_id=$1 AND node_id=$2 AND user_id=$3 AND job_id=$4").bind(batch).bind(node).bind(user).bind(&job.id).fetch_one(&mut *tx).await?;
            let changed = sqlx::query(
                "UPDATE users SET revision=revision+1,updated_at=$1 WHERE id=$2 AND revision=$3",
            )
            .bind(Utc::now())
            .bind(user)
            .bind(revision)
            .execute(&mut *tx)
            .await?;
            if changed.rows_affected() != 1 {
                bail!("user changed before mTLS credential update");
            }
            let changed=sqlx::query("UPDATE node_assignments SET mtls_credential_version=$1 WHERE user_id=$2 AND node_id=$3 AND mtls_credential_id=$4").bind(version).bind(user).bind(node).bind(id).execute(&mut *tx).await?;
            if changed.rows_affected() != 1 {
                bail!("user assignment no longer references this credential");
            }
            // Remaining assignments for the same user in this batch follow the
            // revision bump made here, but unrelated user edits still conflict.
            sqlx::query("UPDATE credential_batch_items i SET expected_revision=$1 FROM jobs j WHERE i.batch_id=$2 AND i.user_id=$3 AND i.job_id=j.id AND j.kind='credential-apply' AND j.status IN ('queued','running') AND i.expected_revision=$4").bind(revision+1).bind(batch).bind(user).bind(revision).execute(&mut *tx).await?;
            let kick = enqueue_job_with_payload_in_tx(
                &mut tx,
                "kick",
                Some(node),
                None,
                json!({"user_id":user}),
            )
            .await
            .map_err(|_| anyhow::anyhow!("failed to enqueue credential follow-up job"))?;
            sqlx::query("UPDATE credential_batch_items SET job_id=$1,applied_at=now(),apply_stage='credential_revocation_queued' WHERE batch_id=$2 AND node_id=$3 AND user_id=$4 AND job_id=$5").bind(&kick).bind(batch).bind(node).bind(user).bind(&job.id).execute(&mut *tx).await?;
            tx.commit().await?;
            Ok(output(
                "credential_revocation_queued",
                json!({"credential_id":id,"version":version,"kick_job_id":kick}),
            ))
        }
        _ => bail!("unknown credential update mode"),
    }
}

async fn mark_applied(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    batch: &str,
    node: &str,
    user: &str,
    stage: &str,
) -> Result<()> {
    sqlx::query("UPDATE credential_batch_items SET applied_at=now(),apply_stage=$1 WHERE batch_id=$2 AND node_id=$3 AND user_id=$4").bind(stage).bind(batch).bind(node).bind(user).execute(&mut **tx).await?;
    Ok(())
}

async fn verify_latest(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
    version: i64,
) -> Result<()> {
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials WHERE id=$1 AND latest_version=$2 AND archived=FALSE)").bind(id).bind(version).fetch_one(&mut **tx).await?;
    if !valid {
        bail!("credential update superseded or archived");
    }
    Ok(())
}
fn make_snapshot(
    secrets: &SecretBox,
    row: &sqlx::postgres::PgRow,
    config: Option<&Value>,
) -> Result<Value> {
    let config = match config {
        Some(c) => c.clone(),
        None => {
            serde_json::from_str(&secrets.decrypt(&row.get::<String, _>("desired_config_enc"))?)?
        }
    };
    Ok(
        json!({"server_config":config,"listen_addr":row.get::<String,_>("listen_addr"),"traffic_stats_port":row.get::<i32,_>("traffic_stats_port"),"proxy_probe_url":row.get::<Option<String>,_>("proxy_probe_url"),"public_host":row.get::<String,_>("public_host"),"public_port":row.get::<i32,_>("public_port"),"tls_sni":row.get::<Option<String>,_>("tls_sni"),"tls_skip_verify":row.get::<bool,_>("tls_skip_verify")}),
    )
}
async fn save_snapshot(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    secrets: &SecretBox,
    node: &str,
    revision: i64,
    snapshot: &Value,
) -> Result<()> {
    let plain = snapshot.to_string();
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES($1,$2,$3,$4,$5,$6)").bind(Uuid::new_v4().to_string()).bind(node).bind(revision).bind(secrets.encrypt(&plain)?).bind(hex::encode(Sha256::digest(plain.as_bytes()))).bind(Utc::now()).execute(&mut **tx).await?;
    Ok(())
}
fn output(stage: &str, result: Value) -> JobOutput {
    JobOutput {
        stage: stage.into(),
        result,
        node_state: None,
        deployed_revision: None,
        deployed_config: None,
        deployed_sha256: None,
        published_connection: None,
        delete_node: false,
    }
}
