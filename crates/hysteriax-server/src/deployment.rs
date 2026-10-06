use crate::db;
use std::{env, time::Duration};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use url::{Host, Url};

use crate::{
    api::generate_token,
    api::resources::ResourceFile,
    config::{
        DEFAULT_TRAFFIC_STATS_PORT, listener_hop_ports, render_server_yaml_with_traffic_stats_port,
    },
    security::{SecretBox, redact_config_secrets, redact_secret_values, token_digest},
    ssh::{self, FingerprintResult, RemoteEnvironment, SshNode, SshSession},
};

const HYSTERIA_RELEASE_TAG: &str = "app/v2.12.3";
const LINUX_AMD64_SHA256: &str = "8c7a68a906998b747a0db87586e364f995fbfddb95693ae6e2fdb68a6e920d3e";
const LINUX_ARM64_SHA256: &str = "c8dc653c3ba0a28d29a26b8fa52d2086f27c0927afddce95c09965e7174e78b0";

#[derive(Clone, Debug)]
pub struct JobInput {
    pub id: String,
    pub kind: String,
    pub node_id: Option<String>,
    pub target_revision: Option<i64>,
    pub payload: Value,
    pub attempts: i64,
}

#[derive(Debug)]
pub struct JobOutput {
    pub stage: String,
    pub result: Value,
    pub node_state: Option<String>,
    pub deployed_revision: Option<i64>,
    pub deployed_config: Option<String>,
    pub deployed_sha256: Option<String>,
    pub delete_node: bool,
}

pub async fn run_job(pool: &PgPool, secrets: &SecretBox, job: &JobInput) -> Result<JobOutput> {
    if let (Some(id), Some(version)) = (
        job.payload.get("credential_id").and_then(Value::as_str),
        job.payload
            .get("credential_version")
            .and_then(Value::as_i64),
    ) {
        let latest: Option<i64> =
            sqlx::query_scalar("SELECT latest_version FROM credentials WHERE id=$1")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        if latest != Some(version) {
            bail!("credential deployment superseded; retry the latest credential batch");
        }
    }
    match job.kind.as_str() {
        "credential-apply" => crate::credentials::worker::apply(pool, secrets, job).await,
        "ssh-test" => ssh_test(pool, secrets, job).await,
        "deploy" | "sync" | "rollback" => deploy(pool, secrets, job).await,
        "kick" => kick(pool, secrets, job).await,
        "uninstall" => uninstall(pool, secrets, job).await,
        other => bail!("unsupported job type: {other}"),
    }
}

pub fn retryable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<ssh::SshError>().is_some_and(|error| {
            matches!(
                error,
                ssh::SshError::Transport(_) | ssh::SshError::ClientsStillOnline { .. }
            )
        })
    })
}

async fn report_progress(pool: &PgPool, job: &JobInput, stage: &str, message: &str) -> Result<()> {
    let timestamp = now();
    let mut tx = db::begin_write(pool).await?;
    let Some(row) = sqlx::query("SELECT logs_json FROM jobs WHERE id = $1 AND status = 'running'")
        .bind(&job.id)
        .fetch_optional(&mut *tx)
        .await?
    else {
        tx.rollback().await?;
        return Ok(());
    };
    let mut logs: Vec<Value> = row.get::<sqlx::types::Json<Vec<Value>>, _>("logs_json").0;
    logs.push(json!({
        "created_at": timestamp,
        "stage": stage,
        "message": message,
        "attempt": job.attempts,
    }));
    if logs.len() > 100 {
        logs.drain(..logs.len() - 100);
    }
    let logs_json = logs;
    let updated = sqlx::query(
        "UPDATE jobs SET stage = $1, logs_json = $2, updated_at = $3 WHERE id = $4 AND status = 'running'",
    )
    .bind(stage)
    .bind(sqlx::types::Json(logs_json))
    .bind(timestamp)
    .bind(&job.id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        tx.rollback().await?;
        return Ok(());
    }
    let payload = json!({
        "id": job.id,
        "kind": job.kind,
        "node_id": job.node_id,
        "status": "running",
        "stage": stage,
        "message": message,
        "attempts": job.attempts,
    });
    sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ($1, 'job.progress', $2, $3)")
        .bind(&job.id)
        .bind(payload)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn ssh_test(pool: &PgPool, secrets: &SecretBox, job: &JobInput) -> Result<JobOutput> {
    let node_id = job
        .node_id
        .as_deref()
        .context("SSH test job is missing node_id")?;
    report_progress(
        pool,
        job,
        "loading_connection",
        "Loading the saved SSH connection.",
    )
    .await?;
    let node = load_ssh_node(pool, secrets, node_id).await?;
    report_progress(
        pool,
        job,
        "connecting",
        "Connecting to the node and checking its SSH host key.",
    )
    .await?;
    match ssh::connect(&node).await? {
        FingerprintResult::NeedsConfirmation { fingerprint } => {
            sqlx::query(
                "UPDATE nodes SET state = 'needs_fingerprint', updated_at = $1 WHERE id = $2",
            )
            .bind(now())
            .bind(node_id)
            .execute(pool)
            .await?;
            Ok(JobOutput {
                stage: "fingerprint_confirmation_required".to_owned(),
                result: json!({"status": "needs_confirmation", "fingerprint": fingerprint}),
                node_state: Some("needs_fingerprint".to_owned()),
                deployed_revision: None,
                deployed_config: None,
                deployed_sha256: None,
                delete_node: false,
            })
        }
        FingerprintResult::Changed { expected, observed } => {
            sqlx::query(
                "UPDATE nodes SET state = 'fingerprint_changed', updated_at = $1 WHERE id = $2",
            )
            .bind(now())
            .bind(node_id)
            .execute(pool)
            .await?;
            bail!("SSH host key changed: expected {expected}; observed {observed}")
        }
        FingerprintResult::Trusted(session) => {
            report_progress(
                pool,
                job,
                "checking_environment",
                "Checking the operating system, architecture, systemd, and sudo access.",
            )
            .await?;
            let environment = ssh::inspect_connected(&session).await?;
            remote_guard(&session, &environment, false, None).await?;
            sqlx::query("UPDATE nodes SET state = 'ready', ssh_host_fingerprint = $1, updated_at = $2 WHERE id = $3")
                .bind(&environment.fingerprint).bind(now()).bind(node_id).execute(pool).await?;
            Ok(JobOutput {
                stage: "environment_checked".to_owned(),
                result: environment_json(&environment),
                node_state: Some("ready".to_owned()),
                deployed_revision: None,
                deployed_config: None,
                deployed_sha256: None,
                delete_node: false,
            })
        }
    }
}

async fn deploy(pool: &PgPool, secrets: &SecretBox, job: &JobInput) -> Result<JobOutput> {
    let node_id = job
        .node_id
        .as_deref()
        .context("deployment job is missing node_id")?;
    let probe_token = generate_token();
    let result = deploy_with_probe(pool, secrets, job, &probe_token).await;
    if let Err(error) =
        sqlx::query("DELETE FROM deployment_probe_tokens WHERE node_id = $1 AND token_hash = $2")
            .bind(node_id)
            .bind(token_digest(&probe_token))
            .execute(pool)
            .await
    {
        tracing::warn!(node_id, %error, "failed to clear temporary deployment probe credential");
    }
    result
}

async fn deploy_with_probe(
    pool: &PgPool,
    secrets: &SecretBox,
    job: &JobInput,
    probe_token: &str,
) -> Result<JobOutput> {
    report_progress(
        pool,
        job,
        "loading_revision",
        "Loading the target configuration revision and encrypted resources.",
    )
    .await?;
    let node_id = job
        .node_id
        .as_deref()
        .context("deployment job is missing node_id")?;
    let revision = job
        .target_revision
        .context("deployment job is missing target_revision")?;
    let row = sqlx::query("SELECT * FROM nodes WHERE id = $1")
        .bind(node_id)
        .fetch_optional(pool)
        .await?
        .context("node was deleted before deployment")?;
    let snapshot_enc: Option<String> = sqlx::query_scalar(
        "SELECT config_enc FROM config_versions WHERE node_id = $1 AND revision = $2",
    )
    .bind(node_id)
    .bind(revision)
    .fetch_optional(pool)
    .await?;
    let snapshot_enc = snapshot_enc.context("target configuration version is missing")?;
    let snapshot: Value = serde_json::from_str(&secrets.decrypt(&snapshot_enc)?)
        .context("stored deployment snapshot is invalid")?;
    let options = snapshot
        .get("server_config")
        .cloned()
        .unwrap_or_else(|| json!({}));
    report_progress(
        pool,
        job,
        "resolving_resources",
        "Resolving and validating the configuration resource references.",
    )
    .await?;
    let (resolved_options, resource_files) =
        crate::api::resources::resolve_config_resources(pool, secrets, node_id, &options)
            .await
            .map_err(|error| anyhow::anyhow!(error.message))?;
    let listen_addr = snapshot
        .get("listen_addr")
        .and_then(Value::as_str)
        .context("deployment snapshot is missing listen_addr")?;
    let proxy_probe_url = snapshot
        .get("proxy_probe_url")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let traffic_stats_port = snapshot_traffic_stats_port(&snapshot)?;
    let name: String = row.get("name");
    let ssh_node = crate::credentials::ssh_node(pool, secrets, &row).await?;

    report_progress(
        pool,
        job,
        "connecting",
        "Connecting to the node and verifying its pinned SSH host key.",
    )
    .await?;
    let session = match ssh::connect(&ssh_node).await? {
        FingerprintResult::Trusted(session) => session,
        FingerprintResult::NeedsConfirmation { fingerprint } => {
            sqlx::query(
                "UPDATE nodes SET state = 'needs_fingerprint', updated_at = $1 WHERE id = $2",
            )
            .bind(now())
            .bind(node_id)
            .execute(pool)
            .await?;
            bail!("confirm this SSH host fingerprint before deploying: {fingerprint}")
        }
        FingerprintResult::Changed { expected, observed } => {
            sqlx::query(
                "UPDATE nodes SET state = 'fingerprint_changed', updated_at = $1 WHERE id = $2",
            )
            .bind(now())
            .bind(node_id)
            .execute(pool)
            .await?;
            bail!("SSH host key changed: expected {expected}; observed {observed}")
        }
    };
    report_progress(
        pool,
        job,
        "checking_environment",
        "Checking the operating system, architecture, systemd, sudo access, disk space, and listener requirements.",
    )
    .await?;
    let environment = ssh::inspect_connected(&session).await?;
    let port_hopping = listener_hop_ports(listen_addr).is_some();
    remote_guard(&session, &environment, port_hopping, Some(listen_addr)).await?;

    let deployed_hash: Option<String> = row.get("deployed_content_sha256");
    let force_sync = job
        .payload
        .get("force")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    report_progress(
        pool,
        job,
        "checking_drift",
        "Comparing the remote configuration with the last successfully deployed version.",
    )
    .await?;
    if job.kind != "rollback"
        && !force_sync
        && let Some(expected_hash) = deployed_hash
    {
        let remote_hash_output = session
            .execute_checked(&root_command(
                environment.privilege != "root",
                "sha256sum /etc/hysteriax/config.yaml | awk '{print $1}'",
            ))
            .await?;
        let remote_hash = remote_hash_output.trim();
        if remote_hash != expected_hash {
            sqlx::query("UPDATE nodes SET state = 'drift', updated_at = $1 WHERE id = $2")
                .bind(now())
                .bind(node_id)
                .execute(pool)
                .await?;
            bail!(
                "remote Hysteria configuration changed outside HysteriaX; inspect the difference, then start an explicit sync to apply the desired revision"
            )
        }
    }

    let (asset_name, expected_hash) = match environment.architecture.as_str() {
        "amd64" => ("hysteria-linux-amd64", LINUX_AMD64_SHA256),
        "arm64" => ("hysteria-linux-arm64", LINUX_ARM64_SHA256),
        _ => bail!("unsupported node architecture"),
    };
    report_progress(
        pool,
        job,
        "downloading_release",
        "Downloading and verifying the pinned Hysteria release asset.",
    )
    .await?;
    let artifact = download_hysteria(asset_name, expected_hash).await?;
    let node_token: String = secrets.decrypt(&row.get::<String, _>("node_token_enc"))?;
    let stats_secret: String =
        secrets.decrypt(&row.get::<String, _>("traffic_stats_secret_enc"))?;
    let management_url = env::var("HYSTERIAX_PUBLIC_URL")
        .context("HYSTERIAX_PUBLIC_URL must be configured before installing nodes")?;
    report_progress(
        pool,
        job,
        "rendering_configuration",
        "Rendering the server configuration and systemd unit.",
    )
    .await?;
    let server_yaml = render_server_yaml_with_traffic_stats_port(
        &resolved_options,
        listen_addr,
        node_id,
        &node_token,
        &stats_secret,
        &management_url,
        traffic_stats_port,
    )?;
    let deployed_sha256 = hex::encode(Sha256::digest(server_yaml.as_bytes()));
    let systemd_unit = systemd_unit(port_hopping);
    let remote_staging = format!("/tmp/hysteriax-{node_id}");
    report_progress(
        pool,
        job,
        "uploading_files",
        "Uploading the verified binary, configuration, unit, and referenced resources.",
    )
    .await?;
    session
        .execute_checked(&format!(
            "umask 077; mkdir -p {remote_staging}/resources; chmod 0700 {remote_staging} {remote_staging}/resources"
        ))
        .await?;
    session
        .upload(&format!("{remote_staging}/hysteria"), &artifact)
        .await?;
    session
        .upload(
            &format!("{remote_staging}/server.yaml"),
            server_yaml.as_bytes(),
        )
        .await?;
    session
        .upload(
            &format!("{remote_staging}/hysteriax.service"),
            systemd_unit.as_bytes(),
        )
        .await?;
    for resource in &resource_files {
        session
            .upload(
                &format!("{remote_staging}/resources/{}", resource.id),
                &resource.content,
            )
            .await?;
    }
    let probe_expires_at = chrono::Utc::now() + chrono::Duration::minutes(15);
    sqlx::query("INSERT INTO deployment_probe_tokens (node_id, token_hash, expires_at) VALUES ($1, $2, $3) ON CONFLICT(node_id) DO UPDATE SET token_hash = excluded.token_hash, expires_at = excluded.expires_at")
        .bind(node_id)
        .bind(token_digest(probe_token))
        .bind(probe_expires_at)
        .execute(pool)
        .await?;
    report_progress(
        pool,
        job,
        "installing_service",
        "Installing the managed systemd service and applying the new configuration.",
    )
    .await?;
    if let Err(error) = install_managed_node(&session, &environment, node_id, &remote_staging).await
    {
        let _ = session.execute(&format!("rm -rf {remote_staging}")).await;
        return Err(error);
    }

    let startup_timeout = startup_health_timeout(&resolved_options);
    report_progress(
        pool,
        job,
        "checking_health",
        &format!("Waiting up to {} seconds for the traffic and online statistics APIs, including certificate provisioning.", startup_timeout.as_secs()),
    )
    .await?;
    let health_check = wait_for_startup(startup_timeout, || async {
        let status = session.execute_checked(
            "systemctl show hysteriax.service --property=ActiveState --property=SubState --property=ExecMainStatus --property=NRestarts",
        ).await?;
        if let Some(reason) = startup_service_failure(&status) {
            return Ok(StartupCheck::Failed(reason));
        }
        let health = async {
            for path in ["/traffic", "/online"] {
                let body = session
                    .loopback_http_get(u32::from(traffic_stats_port), path, &stats_secret)
                    .await?;
                let value: Value = serde_json::from_slice(&body)
                    .with_context(|| format!("Hysteria {path} API returned invalid JSON"))?;
                if !value.is_object() {
                    bail!("Hysteria {path} API returned an invalid response shape")
                }
            }
            Ok::<(), anyhow::Error>(())
        }.await;
        Ok(match health {
            Ok(()) => StartupCheck::Ready,
            Err(error) => StartupCheck::Pending(error.to_string()),
        })
    }).await;
    let mut healthy = health_check.is_ok();
    let mut health_failure = health_check.err().map(|error| error.to_string());
    let mut proxy_probe_result = None;
    if healthy {
        let public_host: String = row.get("public_host");
        let tls_sni: Option<String> = row.get("tls_sni");
        report_progress(
            pool,
            job,
            "checking_proxy_traffic",
            "Running the pinned Hysteria client through the new server and forwarding a TCP request.",
        )
        .await?;
        match run_proxy_probe(
            pool,
            secrets,
            &session,
            ProxyProbe {
                node_id,
                token: probe_token,
                stats_secret: &stats_secret,
                traffic_stats_port,
                listen_addr,
                public_host: &public_host,
                tls_sni: tls_sni.as_deref(),
                options: &resolved_options,
                resources: &resource_files,
                target_url: proxy_probe_url.as_deref(),
            },
        )
        .await
        {
            Ok(result) => proxy_probe_result = Some(result),
            Err(error) => {
                healthy = false;
                health_failure = Some(format!("Hysteria client TCP proxy probe failed: {error}"));
            }
        }
    }
    if !healthy {
        let health_failure = health_failure
            .unwrap_or_else(|| "traffic and online endpoints did not become ready".to_owned());
        let diagnostics = session
            .execute(&root_command(
                environment.privilege != "root",
                "systemctl status hysteriax.service --no-pager 2>&1; journalctl -u hysteriax.service -n 30 --no-pager 2>&1; if command -v ss >/dev/null 2>&1; then ss -lnutp; fi",
            ))
            .await
            .map(|output| {
                output
                    .stdout
                    .chars()
                    .filter(|character| !character.is_control() || *character == '\n' || *character == '\t')
                    .collect::<String>()
            })
            .unwrap_or_else(|_| "remote service diagnostics unavailable".to_owned());
        let diagnostics: String = diagnostics
            .chars()
            .rev()
            .take(4_000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let health_detail = format!("{health_failure}; remote diagnostics: {diagnostics}");
        let has_previous = session
            .execute_checked(
                "if [ -f /opt/hysteriax/config.previous ]; then printf yes; else printf no; fi",
            )
            .await
            .map(|output| output.trim() == "yes")
            .ok();
        let rollback_message = match has_previous {
            Some(true) => {
                "The new service did not become healthy; restoring the previous configuration."
            }
            Some(false) => {
                "The new service did not become healthy; cleaning up the failed first installation."
            }
            None => "The new service did not become healthy; rolling back the installation.",
        };
        report_progress(pool, job, "rolling_back", rollback_message).await?;
        if rollback_managed_node(&session, &environment).await.is_err() {
            let _ = session.execute(&format!("rm -rf {remote_staging}")).await;
            return Err(ssh::SshError::Command {
                code: 54,
                stderr: format!(
                    "post-deployment health check failed ({health_detail}) and automatic rollback also failed"
                ),
            }
            .into());
        }
        let _ = session.execute(&format!("rm -rf {remote_staging}")).await;
        let rollback_result = match has_previous {
            Some(true) => "previous configuration was restored",
            Some(false) => "failed first installation was cleaned up",
            None => "installation was rolled back",
        };
        return Err(ssh::SshError::Command {
            code: 53,
            stderr: format!(
                "post-deployment health check failed ({health_detail}); {rollback_result}"
            ),
        }
        .into());
    }

    let deployed_config = secrets.encrypt(&options.to_string())?;
    let state = if job.kind == "rollback" {
        "rolled_back"
    } else if row.get::<i64, _>("desired_revision") > revision {
        "syncing"
    } else {
        "deployed"
    };
    Ok(JobOutput {
        stage: "health_checked".to_owned(),
        result: json!({"hysteria_version": HYSTERIA_RELEASE_TAG, "architecture": environment.architecture, "traffic_api": "ok", "online_api": "ok", "proxy_probe": proxy_probe_result, "name": name}),
        node_state: Some(state.to_owned()),
        deployed_revision: Some(revision),
        deployed_config: Some(deployed_config),
        deployed_sha256: Some(deployed_sha256),
        delete_node: false,
    })
}

fn startup_health_timeout(options: &Value) -> Duration {
    Duration::from_secs(if options.get("acme").is_some_and(Value::is_object) {
        180
    } else {
        30
    })
}

enum StartupCheck {
    Ready,
    Pending(String),
    Failed(String),
}

fn startup_service_failure(status: &str) -> Option<String> {
    let properties: std::collections::HashMap<_, _> = status
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let active = properties.get("ActiveState").copied().unwrap_or("unknown");
    let sub = properties.get("SubState").copied().unwrap_or("unknown");
    let exit = properties
        .get("ExecMainStatus")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let restarts = properties
        .get("NRestarts")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    if matches!(active, "failed" | "inactive" | "deactivating") || exit != 0 || restarts >= 3 {
        Some(format!(
            "Hysteria service failed during startup: ActiveState={active}, SubState={sub}, ExecMainStatus={exit}, NRestarts={restarts}"
        ))
    } else {
        None
    }
}

async fn wait_for_startup<F, Fut>(timeout: Duration, mut check: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<StartupCheck>>,
{
    let started = tokio::time::Instant::now();
    let deadline = started + timeout;
    let mut last_error = "statistics APIs have not become ready".to_owned();
    loop {
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "startup health check timed out after {:.1}s (budget {}s); last error: {last_error}",
                started.elapsed().as_secs_f64(),
                timeout.as_secs()
            );
        }
        // Bound each attempt as well as the entire wait, including SSH status requests.
        let attempt_deadline = deadline.min(tokio::time::Instant::now() + Duration::from_secs(12));
        match tokio::time::timeout_at(attempt_deadline, check()).await {
            Ok(Ok(StartupCheck::Ready)) => return Ok(()),
            Ok(Ok(StartupCheck::Failed(reason))) => {
                bail!("{reason}; waited {:.1}s", started.elapsed().as_secs_f64())
            }
            Ok(Ok(StartupCheck::Pending(reason))) => last_error = reason,
            Ok(Err(error)) => last_error = error.to_string(),
            Err(_) => last_error = format!("startup check timed out; last error: {last_error}"),
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "startup health check timed out after {:.1}s (budget {}s); last error: {last_error}",
                started.elapsed().as_secs_f64(),
                timeout.as_secs()
            );
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + Duration::from_secs(1)),
        )
        .await;
    }
}

struct ProxyProbe<'a> {
    node_id: &'a str,
    token: &'a str,
    stats_secret: &'a str,
    traffic_stats_port: u16,
    listen_addr: &'a str,
    public_host: &'a str,
    tls_sni: Option<&'a str>,
    options: &'a Value,
    resources: &'a [ResourceFile],
    target_url: Option<&'a str>,
}

async fn run_proxy_probe(
    pool: &PgPool,
    secrets: &SecretBox,
    session: &SshSession,
    probe: ProxyProbe<'_>,
) -> Result<Value> {
    let ProxyProbe {
        node_id,
        token: probe_token,
        stats_secret,
        traffic_stats_port,
        listen_addr,
        public_host,
        tls_sni,
        options,
        resources: resource_files,
        target_url,
    } = probe;
    let (server_address, realm_options) = deployment_probe_server_config(options, listen_addr)?;
    let custom_target = target_url.map(parse_http_probe_target).transpose()?;
    let default_remote_target = format!("127.0.0.1:{traffic_stats_port}");
    let remote_target = custom_target
        .as_ref()
        .map(|target| target.remote_address.as_str())
        .unwrap_or(&default_remote_target);
    let probe_port = free_remote_tcp_port(session, probe_token).await?;
    let remote_dir = format!("/tmp/hysteriax-probe-{node_id}");
    let client_config_path = format!("{remote_dir}/client.yaml");
    let client_log_path = format!("{remote_dir}/client.log");
    let client_certificate_path = format!("{remote_dir}/client.crt");
    let client_key_path = format!("{remote_dir}/client.key");
    let probe_id = format!("probe-{node_id}");

    let mut tls = json!({
        "sni": tls_sni.filter(|value| !value.trim().is_empty()).unwrap_or(public_host),
        "insecure": true
    });
    if let Some(ech) = client_ech_config(options, resource_files)? {
        tls["ech"] = Value::String(ech);
    }
    let needs_client_certificate = options
        .pointer("/tls/clientCA")
        .and_then(Value::as_str)
        .is_some_and(|path| !path.trim().is_empty());
    let client_certificate = if needs_client_certificate {
        let row = sqlx::query("SELECT user_id, mtls_credential_id, mtls_credential_version FROM node_assignments WHERE node_id = $1 AND mtls_credential_id IS NOT NULL ORDER BY created_at LIMIT 1")
            .bind(node_id)
            .fetch_optional(pool)
            .await?
            .context("mTLS deployment probe requires an assigned client certificate and private key")?;
        let (certificate, private_key) =
            crate::credentials::assignment_identity(pool, secrets, &row)
                .await?
                .context("mTLS identity missing")?;
        tls["clientCertificate"] = Value::String(client_certificate_path.clone());
        tls["clientKey"] = Value::String(client_key_path.clone());
        Some((certificate, private_key))
    } else {
        None
    };

    let mut client_config = json!({
        "server": server_address,
        "auth": probe_token,
        "tls": tls,
        "tcpForwarding": [{
            "listen": format!("127.0.0.1:{probe_port}"),
            "remote": remote_target
        }]
    });
    if let Some(realm_options) = realm_options {
        client_config["realm"] = realm_options;
    }
    if let Some(obfs) = options.get("obfs") {
        client_config["obfs"] = obfs.clone();
    }
    let client_config_yaml = serde_yaml::to_string(&client_config)?;
    let restricted_egress = options.get("acl").is_some() || options.get("outbounds").is_some();
    session
        .execute_checked(&format!(
            "umask 077; mkdir -p {}; chmod 0700 {}",
            shell_quote(&remote_dir),
            shell_quote(&remote_dir)
        ))
        .await?;

    let mut client_pid = None;
    let probe_result = async {
        if let Some((certificate, private_key)) = &client_certificate {
            session
                .upload(&client_certificate_path, certificate.as_bytes())
                .await?;
            session
                .upload(&client_key_path, private_key.as_bytes())
                .await?;
        }
        session
            .upload(&client_config_path, client_config_yaml.as_bytes())
            .await?;
        let chmod_command = if client_certificate.is_some() {
            format!(
                "chmod 0600 {} {} {}",
                shell_quote(&client_config_path),
                shell_quote(&client_certificate_path),
                shell_quote(&client_key_path)
            )
        } else {
            format!("chmod 0600 {}", shell_quote(&client_config_path))
        };
        session.execute_checked(&chmod_command).await?;

        let start_command = format!(
            "set -eu; umask 077; HYSTERIA_DISABLE_UPDATE_CHECK=1 nohup /opt/hysteriax/hysteria client -c {} > {} 2>&1 </dev/null & printf '%s\\n' \"$!\"",
            shell_quote(&client_config_path),
            shell_quote(&client_log_path)
        );
        client_pid = Some(
            session
                .execute_checked(&start_command)
                .await?
                .trim()
                .parse::<u32>()
                .context("the Hysteria client probe did not start cleanly")?,
        );

        let mut last_probe_error = "the client did not return an online response".to_owned();
        for _ in 0..10 {
            let proxy_response = if let Some(target) = &custom_target {
                session
                    .loopback_http_get_target(
                        probe_port.into(),
                        &target.path,
                        &target.host_header,
                    )
                    .await
            } else {
                session
                    .loopback_http_get(probe_port.into(), "/online", stats_secret)
                    .await
            };
            match proxy_response {
                Ok(_) if custom_target.is_some() => {
                    let online_body = session
                        .loopback_http_get(u32::from(traffic_stats_port), "/online", stats_secret)
                        .await?;
                    let online: Value = serde_json::from_slice(&online_body)
                        .context("Hysteria online API returned invalid JSON during custom probe")?;
                    if online
                        .get(&probe_id)
                        .and_then(Value::as_u64)
                        .is_some_and(|count| count > 0)
                    {
                        return Ok::<&str, anyhow::Error>("custom_tcp_forwarding");
                    }
                    last_probe_error = "custom HTTP target returned successfully but the probe identity was not online".to_owned();
                }
                Ok(body) => match serde_json::from_slice::<Value>(&body) {
                    Ok(online)
                        if online
                            .get(&probe_id)
                            .and_then(Value::as_u64)
                            .is_some_and(|count| count > 0) =>
                    {
                        return Ok("tcp_forwarding");
                    }
                    Ok(_) => last_probe_error = "the proxy response omitted the probe client".to_owned(),
                    Err(error) => last_probe_error = format!("proxy response was invalid JSON: {error}"),
                },
                Err(error) => last_probe_error = error.to_string(),
            }

            if restricted_egress && custom_target.is_none() {
                let online_body = session
                    .loopback_http_get(u32::from(traffic_stats_port), "/online", stats_secret)
                    .await?;
                let online: Value = serde_json::from_slice(&online_body)
                    .context("Hysteria online API returned invalid JSON during proxy probe")?;
                if online
                    .get(&probe_id)
                    .and_then(Value::as_u64)
                    .is_some_and(|count| count > 0)
                {
                    return Ok("authenticated_session");
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        bail!("the pinned Hysteria client did not establish an authenticated proxy session: {last_probe_error}")
    }
    .await;

    let probe_result = match probe_result {
        Ok(route_check) => Ok(route_check),
        Err(error) => {
            let diagnostics = session
                .execute(&format!(
                    "tail -n 30 {} 2>/dev/null || true",
                    shell_quote(&client_log_path)
                ))
                .await
                .map(|output| output.stdout)
                .unwrap_or_default();
            let mut message = error.to_string();
            let mut secrets_to_scrub = vec![probe_token.to_owned(), stats_secret.to_owned()];
            if let Some((certificate, private_key)) = &client_certificate {
                secrets_to_scrub.push(certificate.clone());
                secrets_to_scrub.push(private_key.clone());
            }
            for path in ["/obfs/salamander/password", "/obfs/gecko/password"] {
                if let Some(password) = options.pointer(path).and_then(Value::as_str) {
                    secrets_to_scrub.push(password.to_owned());
                }
            }
            redact_secret_values(&mut message, secrets_to_scrub.iter().cloned());
            redact_config_secrets(&mut message, options);
            let diagnostics: String = diagnostics
                .chars()
                .filter(|character| {
                    !character.is_control() || *character == '\n' || *character == '\t'
                })
                .collect();
            let mut sanitized_log = diagnostics;
            redact_secret_values(&mut sanitized_log, secrets_to_scrub);
            redact_config_secrets(&mut sanitized_log, options);
            if !sanitized_log.trim().is_empty() {
                message.push_str("; client log: ");
                message.push_str(sanitized_log.trim());
            }
            Err(anyhow::anyhow!(message))
        }
    };

    let cleanup_command = format!(
        "{}rm -rf {}",
        client_pid
            .map(|pid| format!("kill {pid} 2>/dev/null || true; sleep 0.2; "))
            .unwrap_or_default(),
        shell_quote(&remote_dir)
    );
    let cleanup = session.execute_checked(&cleanup_command).await;
    let route_check = match (probe_result, cleanup) {
        (Ok(route_check), Ok(_)) => route_check,
        (Ok(_), Err(error)) => {
            return Err(error).context("failed to remove temporary Hysteria probe files");
        }
        (Err(error), Ok(_)) => return Err(error),
        (Err(probe_error), Err(cleanup_error)) => {
            return Err(probe_error.context(format!(
                "also failed to remove temporary Hysteria probe files: {cleanup_error}"
            )));
        }
    };

    Ok(json!({
        "status": "passed",
        "transport": "hysteria_client_tcp",
        "route_check": route_check,
    }))
}

pub(crate) fn client_ech_config(
    options: &Value,
    resource_files: &[ResourceFile],
) -> Result<Option<String>> {
    let Some(key_path) = options.pointer("/ech/keyPath").and_then(Value::as_str) else {
        return Ok(None);
    };
    let resource_id = key_path
        .rsplit('/')
        .next()
        .context("ECH key resource path has no resource id")?;
    let resource = resource_files
        .iter()
        .find(|resource| resource.id == resource_id)
        .context("ECH client probe could not find the uploaded key resource")?;
    let pem = std::str::from_utf8(&resource.content).context("ECH resource is not valid UTF-8")?;
    crate::api::subscriptions::extract_ech_config_list(pem)
        .map(Some)
        .map_err(|error| anyhow::anyhow!(error.message))
}

struct HttpProbeTarget {
    remote_address: String,
    host_header: String,
    path: String,
}

fn parse_http_probe_target(value: &str) -> Result<HttpProbeTarget> {
    let url = Url::parse(value).context("proxy_probe_url is not a valid HTTP URL")?;
    if url.scheme() != "http"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("proxy_probe_url must be HTTP without credentials, query, or fragment");
    }
    let host = match url.host().context("proxy_probe_url is missing a host")? {
        Host::Domain(host) => host.to_owned(),
        Host::Ipv4(address) => address.to_string(),
        Host::Ipv6(address) => format!("[{address}]"),
    };
    let port = url
        .port_or_known_default()
        .context("proxy_probe_url is missing a port")?;
    let host_header = if port == 80 {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    Ok(HttpProbeTarget {
        remote_address: format!("{host}:{port}"),
        host_header,
        path: if url.path().is_empty() {
            "/".to_owned()
        } else {
            url.path().to_owned()
        },
    })
}

fn loopback_server_address(listen_addr: &str) -> Result<String> {
    let (host, _) = listen_addr
        .rsplit_once(':')
        .context("listen_addr must contain a host and port")?;
    let port =
        primary_listener_port(listen_addr).context("listen_addr has an invalid primary port")?;
    let host = match host.trim() {
        "" | "0.0.0.0" => "127.0.0.1",
        "::" | "[::]" => "[::1]",
        host => host,
    };
    Ok(format!("{host}:{port}"))
}

pub(crate) fn deployment_probe_server_config(
    options: &Value,
    listen_addr: &str,
) -> Result<(String, Option<Value>)> {
    let Some(connection) = crate::config::realm_connection(options)? else {
        return Ok((loopback_server_address(listen_addr)?, None));
    };
    let mut realm = options
        .get("realm")
        .cloned()
        .context("Realm connection is missing its configuration")?;
    realm
        .as_object_mut()
        .context("realm must be an object")?
        .remove("connection");
    Ok((crate::config::realm_client_uri(&connection)?, Some(realm)))
}

async fn free_remote_tcp_port(session: &SshSession, seed: &str) -> Result<u16> {
    let seed = u16::from_str_radix(seed.get(3..7).unwrap_or("0000"), 16).unwrap_or_default();
    let base = 49_152 + seed % 16_000;
    for offset in 0..32_u16 {
        let port = 49_152 + (base - 49_152 + offset * 257) % 16_384;
        let port_hex = format!("{port:04X}");
        let command = format!(
            "if grep -qi ':{port_hex} ' /proc/net/tcp /proc/net/tcp6 2>/dev/null; then printf busy; else printf free; fi"
        );
        if session.execute_checked(&command).await?.trim() == "free" {
            return Ok(port);
        }
    }
    bail!("could not choose a free loopback port for the Hysteria probe")
}

pub(crate) async fn load_ssh_node(
    pool: &PgPool,
    secrets: &SecretBox,
    node_id: &str,
) -> Result<SshNode> {
    let row = sqlx::query("SELECT * FROM nodes WHERE id = $1")
        .bind(node_id)
        .fetch_optional(pool)
        .await?
        .context("node not found")?;
    crate::credentials::ssh_node(pool, secrets, &row).await
}

pub(crate) async fn load_deployed_traffic_stats_port(
    pool: &PgPool,
    secrets: &SecretBox,
    node_id: &str,
) -> Result<u16> {
    let row = sqlx::query("SELECT traffic_stats_port, deployed_revision FROM nodes WHERE id = $1")
        .bind(node_id)
        .fetch_optional(pool)
        .await?
        .context("node not found")?;
    let configured_port = u16::try_from(row.get::<i32, _>("traffic_stats_port"))
        .context("saved trafficStats port is invalid")?;
    if let Some(revision) = row.get::<Option<i64>, _>("deployed_revision") {
        let snapshot_enc: Option<String> = sqlx::query_scalar(
            "SELECT config_enc FROM config_versions WHERE node_id = $1 AND revision = $2",
        )
        .bind(node_id)
        .bind(revision)
        .fetch_optional(pool)
        .await?;
        if let Some(snapshot_enc) = snapshot_enc {
            let snapshot: Value = serde_json::from_str(&secrets.decrypt(&snapshot_enc)?)
                .context("stored deployed configuration snapshot is invalid")?;
            return snapshot_traffic_stats_port(&snapshot);
        }
    }
    Ok(configured_port)
}

fn snapshot_traffic_stats_port(snapshot: &Value) -> Result<u16> {
    let Some(value) = snapshot.get("traffic_stats_port") else {
        // Configuration snapshots created before this setting was introduced used 9780.
        return Ok(DEFAULT_TRAFFIC_STATS_PORT);
    };
    let raw = value
        .as_u64()
        .context("deployment snapshot traffic_stats_port must be an integer")?;
    let port = u16::try_from(raw).context("deployment snapshot traffic_stats_port is invalid")?;
    if port == 0 {
        bail!("deployment snapshot traffic_stats_port must be between 1 and 65535");
    }
    Ok(port)
}

async fn uninstall(pool: &PgPool, secrets: &SecretBox, job: &JobInput) -> Result<JobOutput> {
    report_progress(
        pool,
        job,
        "connecting",
        "Connecting to the node and verifying HysteriaX ownership before removal.",
    )
    .await?;
    let node_id = job
        .node_id
        .as_deref()
        .context("uninstall job is missing node_id")?;
    let node = load_ssh_node(pool, secrets, node_id).await?;
    let session = match ssh::connect(&node).await? {
        FingerprintResult::Trusted(session) => session,
        FingerprintResult::NeedsConfirmation { fingerprint } => {
            bail!("confirm this SSH host fingerprint before uninstalling: {fingerprint}")
        }
        FingerprintResult::Changed { expected, observed } => {
            bail!("SSH host key changed: expected {expected}; observed {observed}")
        }
    };
    report_progress(
        pool,
        job,
        "checking_environment",
        "Checking remote systemd and the managed-install marker.",
    )
    .await?;
    let environment = ssh::inspect_connected(&session).await?;
    report_progress(
        pool,
        job,
        "uninstalling_service",
        "Stopping the managed service and removing its files and service account.",
    )
    .await?;
    let script = "set -eu\nROOT=/opt/hysteriax\nETC=/etc/hysteriax\nif [ ! -f \"$ROOT/.hysteriax-managed\" ]; then\n  if [ -e \"$ROOT\" ] || [ -e \"$ETC\" ] || [ -e /etc/systemd/system/hysteriax.service ] || getent passwd hysteriax >/dev/null 2>&1; then echo 'HysteriaX ownership marker is missing; refusing to remove remote files' >&2; exit 55; fi\n  exit 0\nfi\nsystemctl stop hysteriax.service || true\nsystemctl disable hysteriax.service || true\nrm -f /etc/systemd/system/hysteriax.service\nsystemctl daemon-reload\nrm -rf \"$ROOT\" \"$ETC\" /var/lib/hysteriax\nuserdel hysteriax || true\ngroupdel hysteriax || true\n";
    session
        .execute_checked(&root_command(environment.privilege != "root", script))
        .await?;
    Ok(JobOutput {
        stage: "remote_uninstalled".to_owned(),
        result: json!({"node_id": node_id, "managed_service": "removed"}),
        node_state: None,
        deployed_revision: None,
        deployed_config: None,
        deployed_sha256: None,
        delete_node: true,
    })
}

async fn remote_guard(
    session: &SshSession,
    environment: &RemoteEnvironment,
    port_hopping: bool,
    listen_addr: Option<&str>,
) -> Result<()> {
    let firewall_tools = if port_hopping {
        "if ! command -v nft >/dev/null 2>&1 && ! command -v iptables >/dev/null 2>&1 && ! command -v ip6tables >/dev/null 2>&1; then echo 'port hopping requires nftables or iptables on the remote node' >&2; exit 45; fi;"
    } else {
        ""
    };
    let port_check = listen_addr
        .and_then(primary_listener_port)
        .map(|port| {
            let port_hex = format!("{port:04X}");
            format!(
                "if [ ! -f /opt/hysteriax/.hysteriax-managed ]; then for table in /proc/net/udp /proc/net/udp6; do if [ -r \"$table\" ] && awk -v port=\":{port_hex}\" 'NR > 1 && toupper($2) ~ (port \"$\") {{ found = 1 }} END {{ exit !found }}' \"$table\"; then echo 'configured UDP listener port is already occupied' >&2; exit 46; fi; done; fi;"
            )
        })
        .unwrap_or_default();
    let output = session
        .execute_checked(
            &format!("set -eu; {firewall_tools} {port_check} if [ -e /etc/systemd/system/hysteriax.service ] && [ ! -f /opt/hysteriax/.hysteriax-managed ]; then echo 'unmanaged systemd unit already exists' >&2; exit 41; fi; if [ -e /opt/hysteriax ] && [ ! -f /opt/hysteriax/.hysteriax-managed ]; then echo 'unmanaged directory already exists' >&2; exit 42; fi; if getent passwd hysteriax >/dev/null 2>&1 && [ ! -f /opt/hysteriax/.hysteriax-managed ]; then echo 'unmanaged hysteriax service account already exists' >&2; exit 43; fi; if command -v hysteria >/dev/null 2>&1 && [ ! -f /opt/hysteriax/.hysteriax-managed ]; then echo 'an existing Hysteria installation was found' >&2; exit 44; fi; df -Pk / | awk 'NR == 2 {{ if ($4 < 102400) exit 1 }}'"),
        )
        .await?;
    let _ = output;
    if !ssh::supported_release(&environment.distribution, &environment.version) {
        bail!("remote operating system is outside the supported deployment matrix")
    }
    Ok(())
}

async fn install_managed_node(
    session: &SshSession,
    environment: &RemoteEnvironment,
    node_id: &str,
    remote_staging: &str,
) -> Result<()> {
    let privileged = environment.privilege != "root";
    let command = |script: &str| root_command(privileged, script);
    let preflight = format!(
        "set -eu\nROOT=/opt/hysteriax\nETC=/etc/hysteriax\nSTAGE={remote_staging}\nif [ -e /etc/systemd/system/hysteriax.service ] && [ ! -f \"$ROOT/.hysteriax-managed\" ]; then echo 'unmanaged service conflict' >&2; exit 50; fi\nif [ -e \"$ROOT\" ] && [ ! -f \"$ROOT/.hysteriax-managed\" ]; then echo 'unmanaged install directory conflict' >&2; exit 51; fi\nif getent passwd hysteriax >/dev/null 2>&1 && [ ! -f \"$ROOT/.hysteriax-managed\" ]; then echo 'service account conflict' >&2; exit 52; fi\n"
    );
    session.execute_checked(&command(&preflight)).await?;

    let apply = format!(
        r#"set -eu
ROOT=/opt/hysteriax
ETC=/etc/hysteriax
STAGE={remote_staging}
if ! getent group hysteriax >/dev/null 2>&1; then groupadd --system hysteriax; fi
if ! getent passwd hysteriax >/dev/null 2>&1; then useradd --system --home-dir /var/lib/hysteriax --shell /usr/sbin/nologin --gid hysteriax hysteriax; fi
mkdir -p "$ROOT" "$ETC" "$ETC/resources" /var/lib/hysteriax
chown root:hysteriax "$ETC/resources"
chmod 0750 "$ETC/resources"
for resource in "$STAGE"/resources/*; do
  [ -f "$resource" ] || continue
  target="$ETC/resources/$(basename "$resource")"
  if [ -e "$target" ]; then
    if ! cmp -s "$resource" "$target"; then echo 'managed resource id conflicts with existing contents' >&2; exit 49; fi
  else
    install -o root -g hysteriax -m 0640 "$resource" "$target"
  fi
done
if [ -f "$ROOT/hysteria" ]; then cp -a "$ROOT/hysteria" "$ROOT/hysteria.previous"; fi
if [ -f "$ETC/config.yaml" ]; then cp -a "$ETC/config.yaml" "$ROOT/config.previous"; fi
if [ -f /etc/systemd/system/hysteriax.service ]; then cp -a /etc/systemd/system/hysteriax.service "$ROOT/hysteriax.service.previous"; fi
install -o root -g root -m 0755 "$STAGE/hysteria" "$ROOT/hysteria.new"
mv -f "$ROOT/hysteria.new" "$ROOT/hysteria"
install -o root -g hysteriax -m 0640 "$STAGE/server.yaml" "$ETC/config.yaml.new"
mv -f "$ETC/config.yaml.new" "$ETC/config.yaml"
install -o root -g root -m 0644 "$STAGE/hysteriax.service" /etc/systemd/system/hysteriax.service.new
mv -f /etc/systemd/system/hysteriax.service.new /etc/systemd/system/hysteriax.service
chown hysteriax:hysteriax /var/lib/hysteriax
systemctl daemon-reload
systemctl enable hysteriax.service >/dev/null
if systemctl restart hysteriax.service && systemctl is-active --quiet hysteriax.service; then
  touch "$ROOT/.hysteriax-managed"
  chown root:root "$ROOT/.hysteriax-managed"
  chmod 0600 "$ROOT/.hysteriax-managed"
  rm -rf "$STAGE"
  exit 0
fi
if [ -f "$ROOT/.hysteriax-managed" ]; then
  [ ! -f "$ROOT/hysteria.previous" ] || cp -a "$ROOT/hysteria.previous" "$ROOT/hysteria"
  [ ! -f "$ROOT/config.previous" ] || cp -a "$ROOT/config.previous" "$ETC/config.yaml"
  [ ! -f "$ROOT/hysteriax.service.previous" ] || cp -a "$ROOT/hysteriax.service.previous" /etc/systemd/system/hysteriax.service
  systemctl daemon-reload
  systemctl restart hysteriax.service || true
  echo 'Hysteria service did not become active; previous configuration was restored' >&2
else
  systemctl stop hysteriax.service || true
  systemctl disable hysteriax.service || true
  rm -f /etc/systemd/system/hysteriax.service "$ROOT/hysteria" "$ETC/config.yaml"
  userdel hysteriax || true
  groupdel hysteriax || true
  rm -rf "$ROOT" "$ETC" /var/lib/hysteriax
  echo 'Hysteria service did not become active; failed first installation was cleaned up' >&2
fi
exit 53
"#
    );
    session
        .execute_checked(&command(&apply))
        .await
        .with_context(|| {
            format!("install or restart managed Hysteria service for node {node_id}")
        })?;
    Ok(())
}

async fn rollback_managed_node(
    session: &SshSession,
    environment: &RemoteEnvironment,
) -> Result<()> {
    let script = "set -eu\nROOT=/opt/hysteriax\nETC=/etc/hysteriax\nif [ -f \"$ROOT/config.previous\" ]; then\n  cp -a \"$ROOT/config.previous\" \"$ETC/config.yaml\"\n  [ ! -f \"$ROOT/hysteria.previous\" ] || cp -a \"$ROOT/hysteria.previous\" \"$ROOT/hysteria\"\n  [ ! -f \"$ROOT/hysteriax.service.previous\" ] || cp -a \"$ROOT/hysteriax.service.previous\" /etc/systemd/system/hysteriax.service\n  systemctl daemon-reload\n  systemctl restart hysteriax.service\n  systemctl is-active --quiet hysteriax.service\nelse\n  systemctl stop hysteriax.service || true\n  systemctl disable hysteriax.service || true\n  rm -f /etc/systemd/system/hysteriax.service \"$ROOT/hysteria\" \"$ETC/config.yaml\" \"$ROOT/.hysteriax-managed\"\n  userdel hysteriax || true\n  groupdel hysteriax || true\n  rm -rf \"$ROOT\" \"$ETC\" /var/lib/hysteriax\nfi\n";
    session
        .execute_checked(&root_command(environment.privilege != "root", script))
        .await?;
    Ok(())
}

fn systemd_unit(port_hopping: bool) -> String {
    let extra_capability = if port_hopping { " CAP_NET_ADMIN" } else { "" };
    format!(
        "[Unit]\nDescription=HysteriaX managed Hysteria 2 server\nWants=network-online.target\nAfter=network-online.target\n\n[Service]\nType=simple\nUser=hysteriax\nGroup=hysteriax\nWorkingDirectory=/var/lib/hysteriax\nExecStart=/opt/hysteriax/hysteria server -c /etc/hysteriax/config.yaml\nRestart=on-failure\nRestartSec=5s\nNoNewPrivileges=true\nAmbientCapabilities=CAP_NET_BIND_SERVICE{extra_capability}\nCapabilityBoundingSet=CAP_NET_BIND_SERVICE{extra_capability}\nPrivateTmp=true\nProtectSystem=strict\nProtectHome=true\nReadWritePaths=/var/lib/hysteriax\n\n[Install]\nWantedBy=multi-user.target\n"
    )
}

async fn download_hysteria(asset: &str, expected_sha256: &str) -> Result<Vec<u8>> {
    let url = format!(
        "https://github.com/apernet/hysteria/releases/download/{HYSTERIA_RELEASE_TAG}/{asset}"
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let max_attempts = 3;
    for attempt in 1..=max_attempts {
        let response = match client.get(&url).send().await {
            Ok(response) => response,
            Err(error) if attempt < max_attempts && retryable_download_error(&error) => {
                tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if attempt < max_attempts
            && (response.status().is_server_error()
                || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS)
        {
            tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
            continue;
        }
        let response = response.error_for_status()?;
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(error) if attempt < max_attempts && retryable_download_error(&error) => {
                tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        verify_hysteria_asset(&bytes, expected_sha256)?;
        return Ok(bytes.to_vec());
    }
    unreachable!("bounded download retry loop always returns")
}

fn retryable_download_error(error: &reqwest::Error) -> bool {
    error.is_connect() || error.is_timeout() || error.is_body() || error.is_request()
}

fn verify_hysteria_asset(bytes: &[u8], expected_sha256: &str) -> Result<()> {
    if bytes.len() < 1_000_000 || bytes.len() > 100_000_000 {
        bail!("official Hysteria asset has an unexpected size")
    }
    let digest = hex::encode(Sha256::digest(bytes));
    if digest != expected_sha256 {
        bail!("official Hysteria asset SHA-256 did not match the pinned release digest")
    }
    Ok(())
}

fn environment_json(environment: &RemoteEnvironment) -> Value {
    json!({
        "distribution": environment.distribution,
        "version": environment.version,
        "architecture": environment.architecture,
        "privilege": environment.privilege,
        "systemd": environment.systemd,
        "fingerprint": environment.fingerprint,
    })
}

fn root_command(privileged: bool, script: &str) -> String {
    let command = format!("sh -c {}", shell_quote(script));
    if privileged {
        format!("sudo -n {command}")
    } else {
        command
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

fn primary_listener_port(listen_addr: &str) -> Option<u16> {
    let (_, ports) = listen_addr.rsplit_once(':')?;
    ports.split([',', '-']).next()?.parse().ok()
}

async fn kick(pool: &PgPool, secrets: &SecretBox, job: &JobInput) -> Result<JobOutput> {
    let node_id = job
        .node_id
        .as_deref()
        .context("kick job is missing node_id")?;
    let user_id = job
        .payload
        .get("user_id")
        .and_then(Value::as_str)
        .context("kick job is missing user_id")?;
    if !crate::kick_requests::still_required(pool, job).await? {
        return Ok(restriction_cleared_output(node_id, user_id));
    }
    let node = load_ssh_node(pool, secrets, node_id).await?;
    let session = match ssh::connect(&node).await? {
        FingerprintResult::Trusted(session) => session,
        FingerprintResult::NeedsConfirmation { fingerprint } => {
            bail!("confirm this SSH host fingerprint before kicking clients: {fingerprint}")
        }
        FingerprintResult::Changed { expected, observed } => {
            bail!("SSH host key changed: expected {expected}; observed {observed}")
        }
    };
    let stats_secret_enc: String =
        sqlx::query_scalar("SELECT traffic_stats_secret_enc FROM nodes WHERE id = $1")
            .bind(node_id)
            .fetch_one(pool)
            .await?;
    let stats_secret = secrets.decrypt(&stats_secret_enc)?;
    let traffic_stats_port = load_deployed_traffic_stats_port(pool, secrets, node_id).await?;
    let request_body = format!("[\"{user_id}\"]");
    report_progress(
        pool,
        job,
        "kicking_clients",
        "Requesting that the node disconnect the user's active client devices.",
    )
    .await?;
    let mut remaining = 0_u64;
    for attempt in 0..6 {
        if !crate::kick_requests::still_required(pool, job).await? {
            return Ok(restriction_cleared_output(node_id, user_id));
        }
        // Hysteria stores kick IDs until the next traffic callback. Check online first
        // so an already-offline user does not leave a stale kick marker for a later login.
        let online_before = session
            .loopback_http_get(u32::from(traffic_stats_port), "/online", &stats_secret)
            .await?;
        let online_before: Value = serde_json::from_slice(&online_before)
            .context("Hysteria online API returned invalid JSON")?;
        remaining = online_before
            .get(user_id)
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if remaining == 0 {
            report_progress(
                pool,
                job,
                "clients_offline",
                "The node reports no active client devices for this user.",
            )
            .await?;
            return Ok(clients_offline_output(node_id, user_id));
        }
        // The online query can block while a renewal or billing reset commits.
        if !crate::kick_requests::still_required(pool, job).await? {
            return Ok(restriction_cleared_output(node_id, user_id));
        }
        session
            .loopback_http_post(
                u32::from(traffic_stats_port),
                "/kick",
                &stats_secret,
                request_body.as_bytes(),
            )
            .await?;
        let online_body = session
            .loopback_http_get(u32::from(traffic_stats_port), "/online", &stats_secret)
            .await?;
        let online: Value = serde_json::from_slice(&online_body)
            .context("Hysteria online API returned invalid JSON")?;
        remaining = online
            .get(user_id)
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if remaining == 0 {
            report_progress(
                pool,
                job,
                "clients_offline",
                "The node reports no active client devices for this user.",
            )
            .await?;
            return Ok(clients_offline_output(node_id, user_id));
        }
        report_progress(
            pool,
            job,
            "checking_clients",
            &format!("{remaining} client device(s) remain online; requesting another kick."),
        )
        .await?;
        if attempt < 5 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    Err(ssh::SshError::ClientsStillOnline { remaining }.into())
}

fn restriction_cleared_output(node_id: &str, user_id: &str) -> JobOutput {
    let mut output = clients_offline_output(node_id, user_id);
    output.stage = "restriction_cleared".into();
    output.result = json!({"node_id": node_id, "user_id": user_id, "skipped": true, "reason": "restriction_cleared"});
    output
}

fn clients_offline_output(node_id: &str, user_id: &str) -> JobOutput {
    JobOutput {
        stage: "clients_offline".to_owned(),
        result: json!({"node_id": node_id, "user_id": user_id, "remaining_connections": 0}),
        node_state: None,
        deployed_revision: None,
        deployed_config: None,
        deployed_sha256: None,
        delete_node: false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use sqlx::{PgPool, Row};

    use super::{
        JobInput, deployment_probe_server_config, parse_http_probe_target, primary_listener_port,
        report_progress, systemd_unit, verify_hysteria_asset,
    };

    #[test]
    fn startup_budget_and_service_failures() {
        assert_eq!(
            super::startup_health_timeout(&json!({"acme": {"domains": ["sg1.conn.lol"]}}))
                .as_secs(),
            180
        );
        assert_eq!(
            super::startup_health_timeout(&json!({"tls": {"cert": "cert.pem", "key": "key.pem"}}))
                .as_secs(),
            30
        );
        for status in [
            "ActiveState=failed\nSubState=failed\nExecMainStatus=1\nNRestarts=0",
            "ActiveState=activating\nSubState=auto-restart\nExecMainStatus=1\nNRestarts=0",
            "ActiveState=active\nSubState=running\nExecMainStatus=0\nNRestarts=3",
            "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\nNRestarts=0",
        ] {
            assert!(super::startup_service_failure(status).is_some());
        }
        assert!(
            super::startup_service_failure(
                "ActiveState=active\nSubState=running\nExecMainStatus=0\nNRestarts=0"
            )
            .is_none()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn startup_wait_accepts_delayed_acme_readiness() {
        let started = tokio::time::Instant::now();
        super::wait_for_startup(
            super::startup_health_timeout(&json!({"acme": {}})),
            || async {
                Ok(if started.elapsed() >= std::time::Duration::from_secs(30) {
                    super::StartupCheck::Ready
                } else {
                    super::StartupCheck::Pending("ConnectFailed".to_owned())
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn startup_wait_fails_early_when_service_exits() {
        let started = tokio::time::Instant::now();
        let error = super::wait_for_startup(std::time::Duration::from_secs(180), || async {
            Ok(if started.elapsed() >= std::time::Duration::from_secs(2) {
                super::StartupCheck::Failed("ExecMainStatus=1".to_owned())
            } else {
                super::StartupCheck::Pending("ConnectFailed".to_owned())
            })
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("ExecMainStatus=1"));
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn startup_wait_expires_with_last_error() {
        let started = tokio::time::Instant::now();
        let error = super::wait_for_startup(std::time::Duration::from_secs(180), || async {
            Ok(super::StartupCheck::Pending("ConnectFailed".to_owned()))
        })
        .await
        .unwrap_err();
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(180));
        assert!(error.to_string().contains("budget 180s"));
        assert!(error.to_string().contains("ConnectFailed"));
    }

    #[tokio::test(start_paused = true)]
    async fn startup_wait_bounds_hung_ssh_checks() {
        let started = tokio::time::Instant::now();
        let error = super::wait_for_startup(std::time::Duration::from_secs(30), || async {
            std::future::pending::<anyhow::Result<super::StartupCheck>>().await
        })
        .await
        .unwrap_err();
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(30));
        assert!(error.to_string().contains("startup check timed out"));
    }

    #[test]
    fn realm_deployment_probe_uses_realm_uri_and_client_tuning() {
        let options = json!({
            "realm": {
                "connection": {
                    "serverURL": "http://127.0.0.1:10820",
                    "token": "realm-token",
                    "realmID": "node-realm-1"
                },
                "stunServers": ["127.0.0.1:3478"],
                "stunTimeout": "5s",
                "ipMode": "v4"
            }
        });
        let (server, realm) = deployment_probe_server_config(&options, ":443").unwrap();
        assert_eq!(
            server,
            "realm+http://realm-token@127.0.0.1:10820/node-realm-1"
        );
        let realm = realm.unwrap();
        assert!(realm.get("connection").is_none());
        assert_eq!(realm["stunServers"][0], "127.0.0.1:3478");
        assert_eq!(realm["stunTimeout"], "5s");
        assert_eq!(realm["ipMode"], "v4");
    }

    #[test]
    fn non_realm_deployment_probe_keeps_loopback_listener_address() {
        let (server, realm) = deployment_probe_server_config(&json!({}), ":443").unwrap();
        assert_eq!(server, "127.0.0.1:443");
        assert!(realm.is_none());
    }

    async fn test_pool() -> PgPool {
        crate::db::test_pool().await
    }

    #[test]
    fn release_asset_digests_are_sha256_hex() {
        assert_eq!(
            hex::encode(Sha256::digest(b"pinned-release-check")),
            "5939868cdcb89df1b4fed608f8db89b421c39c53c909ef9458c523f3fe1fc3d5"
        );
    }

    #[test]
    fn rejects_downloaded_asset_with_wrong_pinned_digest() {
        let bytes = vec![0x5a; 1_000_000];
        let error = verify_hysteria_asset(&bytes, &"0".repeat(64)).unwrap_err();
        assert!(error.to_string().contains("SHA-256 did not match"));
    }

    #[test]
    fn parses_http_probe_targets_for_tcp_forwarding() {
        let target = parse_http_probe_target("http://status.example.test/health").unwrap();
        assert_eq!(target.remote_address, "status.example.test:80");
        assert_eq!(target.host_header, "status.example.test");
        assert_eq!(target.path, "/health");

        let ipv6 = parse_http_probe_target("http://[::1]:8080/ready").unwrap();
        assert_eq!(ipv6.remote_address, "[::1]:8080");
        assert_eq!(ipv6.host_header, "[::1]:8080");
        assert!(parse_http_probe_target("https://status.example.test/health").is_err());
        assert!(parse_http_probe_target("http://status.example.test/health?token=secret").is_err());
    }

    #[tokio::test]
    async fn progress_updates_job_stage_logs_and_persisted_event_atomically() {
        let pool = test_pool().await;
        sqlx::query("INSERT INTO jobs (id, kind, status, stage, available_at, created_at, updated_at) VALUES ('progress-job', 'sync', 'running', 'starting', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        let job = JobInput {
            id: "progress-job".to_owned(),
            kind: "sync".to_owned(),
            node_id: None,
            target_revision: Some(1),
            payload: serde_json::json!({}),
            attempts: 1,
        };

        report_progress(
            &pool,
            &job,
            "downloading_release",
            "Verifying pinned release.",
        )
        .await
        .unwrap();

        let row = sqlx::query("SELECT stage, logs_json FROM jobs WHERE id = 'progress-job'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("stage"), "downloading_release");
        let logs: serde_json::Value = row
            .get::<sqlx::types::Json<serde_json::Value>, _>("logs_json")
            .0;
        assert_eq!(logs[0]["stage"], "downloading_release");
        assert_eq!(logs[0]["message"], "Verifying pinned release.");

        let event = sqlx::query(
            "SELECT event_type, payload_json FROM job_events WHERE job_id = 'progress-job'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event.get::<String, _>("event_type"), "job.progress");
        let payload: serde_json::Value = event.get("payload_json");
        assert_eq!(payload["stage"], "downloading_release");
        assert_eq!(payload["message"], "Verifying pinned release.");
    }

    #[test]
    fn net_admin_capability_is_limited_to_port_hopping_nodes() {
        assert!(!systemd_unit(false).contains("CAP_NET_ADMIN"));
        let unit = systemd_unit(true);
        assert!(unit.contains("AmbientCapabilities=CAP_NET_BIND_SERVICE CAP_NET_ADMIN"));
        assert!(unit.contains("CapabilityBoundingSet=CAP_NET_BIND_SERVICE CAP_NET_ADMIN"));
    }

    #[test]
    fn extracts_the_first_listener_port_for_preflight_checks() {
        assert_eq!(primary_listener_port(":443"), Some(443));
        assert_eq!(primary_listener_port("0.0.0.0:8443,9000-9010"), Some(8443));
        assert_eq!(primary_listener_port("[::]:12000-12010"), Some(12000));
        assert_eq!(primary_listener_port("not-a-listener"), None);
    }
}
