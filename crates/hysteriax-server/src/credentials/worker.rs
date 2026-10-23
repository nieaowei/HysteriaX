mod persistence;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

use crate::{
    deployment::{JobInput, JobOutput},
    security::SecretBox,
    ssh::{self, FingerprintResult},
};

pub async fn apply(pool: &PgPool, secrets: &SecretBox, job: &JobInput) -> Result<JobOutput> {
    let id = job.payload["credential_id"]
        .as_str()
        .context("missing credential ID")?;
    let version = job.payload["version"]
        .as_i64()
        .context("missing credential version")?;
    let node = job.node_id.as_deref().context("missing node")?;
    let (source, current) = match persistence::prepare(pool, job).await? {
        persistence::Prepared::Applied(receipt) => return Ok(receipt),
        persistence::Prepared::Ready(source, current) => (source, current),
    };
    let revision = source.revision();
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
            let snapshot = make_snapshot(secrets, &current, None)?;
            persistence::commit(pool, secrets, &source, &snapshot, None).await
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
            persistence::commit(pool, secrets, &source, &snapshot, Some(&config)).await
        }
        _ => bail!("unknown credential update mode"),
    }
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

#[cfg(test)]
pub(super) async fn commit_ssh_for_test(
    pool: &PgPool,
    secrets: &SecretBox,
    job: &JobInput,
) -> Result<JobOutput> {
    match persistence::prepare(pool, job).await? {
        persistence::Prepared::Applied(receipt) => Ok(receipt),
        persistence::Prepared::Ready(source, current) => {
            let snapshot = make_snapshot(secrets, &current, None)?;
            persistence::commit(pool, secrets, &source, &snapshot, None).await
        }
    }
}
