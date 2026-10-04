//! Public-endpoint probes run locally on the management host, never on the node.
use crate::{deployment, security::token_digest, state::AppState};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::Row;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::{process::Command, task::JoinSet};
use uuid::Uuid;

struct WorkDir(PathBuf);
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct ProbeResult {
    status: String,
    external: Option<String>,
    connection_ms: Option<f64>,
    latency_ms: Option<f64>,
    reason: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct ProbeFailure(&'static str);

fn client_failure(log: &str) -> &'static str {
    let log = log.to_ascii_lowercase();
    if log.contains("address already in use") {
        "本地转发端口已被占用"
    } else if log.contains("authentication") || log.contains("auth error") {
        "代理身份验证失败"
    } else if log.contains("certificate") || log.contains("tls") || log.contains("handshake") {
        "TLS 或代理握手失败"
    } else if log.contains("timeout") || log.contains("deadline") {
        "公网代理连接超时"
    } else if log.contains("connection refused") || log.contains("dial") {
        "公网代理连接失败"
    } else {
        "探测客户端已退出"
    }
}

fn deployed_sni(snapshot: &Value, fallback: Option<String>) -> Option<String> {
    match snapshot.get("tls_sni") {
        Some(value) => value.as_str().map(str::to_owned),
        None => fallback,
    }
}

pub async fn run(state: AppState) {
    let mut timer = tokio::time::interval(Duration::from_secs(60));
    let mut last_cleanup = Instant::now() - Duration::from_secs(86400);
    loop {
        timer.tick().await;
        if let Err(error) = collect(&state).await {
            tracing::warn!(%error,"proxy monitoring pass failed");
        }
        if last_cleanup.elapsed() >= Duration::from_secs(86400) {
            if let Err(error) = cleanup(&state).await {
                tracing::warn!(%error,"monitoring retention cleanup failed");
            } else {
                last_cleanup = Instant::now();
            }
        }
    }
}
async fn cleanup(state: &AppState) -> Result<()> {
    for table in ["online_samples", "proxy_probe_samples"] {
        loop {
            let affected=sqlx::query(&format!("DELETE FROM {table} WHERE id IN (SELECT id FROM {table} WHERE sampled_at < now()-interval '30 days' LIMIT 1000)" )).execute(&state.pool).await?.rows_affected();
            if affected < 1000 {
                break;
            }
            tokio::task::yield_now().await;
        }
    }
    sqlx::query("DELETE FROM monitoring_probe_tokens WHERE expires_at < now()")
        .execute(&state.pool)
        .await?;
    Ok(())
}
async fn collect(state: &AppState) -> Result<()> {
    let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM nodes WHERE deployed_revision IS NOT NULL AND state NOT IN ('deleting','delete_failed') ORDER BY id").fetch_all(&state.pool).await?;
    let mut tasks = JoinSet::new();
    for id in ids {
        while tasks.len() >= 4 {
            let _ = tasks.join_next().await;
        }
        let state = state.clone();
        tasks.spawn(async move {
            if let Err(error) = sample(&state, &id).await {
                tracing::warn!(node_id=%id,%error,"proxy sample could not be saved");
            }
        });
    }
    while tasks.join_next().await.is_some() {}
    Ok(())
}
async fn acquire_lease(pool: &sqlx::PgPool, id: &str, owner: Uuid) -> Result<Option<Uuid>> {
    Ok(sqlx::query_scalar("INSERT INTO monitoring_leases(node_id,owner,expires_at) VALUES($1,$2,now()+interval '55 seconds') ON CONFLICT(node_id) DO UPDATE SET owner=$2,expires_at=now()+interval '55 seconds' WHERE monitoring_leases.expires_at < now() RETURNING owner").bind(id).bind(owner).fetch_optional(pool).await?)
}

async fn sample(state: &AppState, id: &str) -> Result<()> {
    let owner = Uuid::new_v4();
    let acquired = acquire_lease(&state.pool, id, owner).await?;
    if acquired.is_none() {
        return Ok(());
    }
    let row=sqlx::query("SELECT n.*,v.config_enc FROM nodes n JOIN config_versions v ON v.node_id=n.id AND v.revision=n.deployed_revision WHERE n.id=$1 AND n.state NOT IN ('deleting','delete_failed')").bind(id).fetch_optional(&state.pool).await?;
    if let Some(row) = row {
        let revision: i64 = row.get("deployed_revision");
        let started = Utc::now();
        let result =
            match tokio::time::timeout(Duration::from_secs(15), probe(state, id, &row)).await {
                Ok(Ok(r)) => r,
                Ok(Err(error)) => ProbeResult {
                    status: "failed".into(),
                    reason: Some(
                        error
                            .downcast_ref::<ProbeFailure>()
                            .map(|e| e.0)
                            .unwrap_or("代理连接或目标请求失败")
                            .into(),
                    ),
                    ..Default::default()
                },
                Err(_) => ProbeResult {
                    status: "failed".into(),
                    reason: Some("探测超过 15 秒".into()),
                    ..Default::default()
                },
            };
        // A concurrently deployed revision invalidates this result.
        let mut tx = crate::db::begin_write(&state.pool).await?;
        let previous=sqlx::query("SELECT revision,health,failures,successes,sampled_at FROM monitoring_probe_state WHERE node_id=$1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
        let prior = previous.as_ref().filter(|r| {
            r.get::<i64, _>("revision") == revision
                && Utc::now() - r.get::<chrono::DateTime<Utc>, _>("sampled_at")
                    < chrono::Duration::seconds(180)
        });
        let effective = if result.external.as_deref() == Some("failed") {
            "failed"
        } else {
            &result.status
        };
        let health = crate::overview::next_probe_health(
            prior
                .map(|r| r.get::<String, _>("health"))
                .as_deref()
                .unwrap_or("checking"),
            prior.map(|r| r.get("failures")).unwrap_or(0),
            prior.map(|r| r.get("successes")).unwrap_or(0),
            effective,
        );
        sqlx::query("INSERT INTO proxy_probe_samples(node_id,revision,sampled_at,status,external_status,connection_ms,latency_ms,reason) SELECT id,$2,$3,$4,$5,$6,$7,$8 FROM nodes WHERE id=$1 AND deployed_revision=$2 AND state NOT IN ('deleting','delete_failed')")
            .bind(id).bind(revision).bind(started).bind(&result.status).bind(&result.external).bind(result.connection_ms).bind(result.latency_ms).bind(result.reason).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO monitoring_probe_state(node_id,revision,health,failures,successes,sampled_at) SELECT id,$2,$3,$4,$5,$6 FROM nodes WHERE id=$1 AND deployed_revision=$2 AND state NOT IN ('deleting','delete_failed') ON CONFLICT(node_id) DO UPDATE SET revision=$2,health=$3,failures=$4,successes=$5,sampled_at=$6").bind(id).bind(revision).bind(health.0).bind(health.1).bind(health.2).bind(started).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    sqlx::query("DELETE FROM monitoring_probe_tokens WHERE node_id=$1")
        .bind(id)
        .execute(&state.pool)
        .await?;
    // Keep the lease until its due time, preventing a second process from immediately repeating this probe.
    Ok(())
}
fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

async fn probe(state: &AppState, id: &str, row: &sqlx::postgres::PgRow) -> Result<ProbeResult> {
    let binary = std::env::var("HYSTERIAX_PROBE_BINARY")
        .unwrap_or_else(|_| "/usr/local/bin/hysteria".into());
    probe_using(state, id, row, &binary).await
}
async fn probe_using(
    state: &AppState,
    id: &str,
    row: &sqlx::postgres::PgRow,
    binary: &str,
) -> Result<ProbeResult> {
    if !std::path::Path::new(&binary).is_file() {
        return Ok(ProbeResult {
            status: "unconfigured".into(),
            reason: Some("管理服务器缺少 Hysteria 探测客户端".into()),
            ..Default::default()
        });
    }
    let snapshot: Value =
        serde_json::from_str(&state.secrets.decrypt(&row.get::<String, _>("config_enc"))?)?;
    let options = snapshot.get("server_config").cloned().unwrap_or(json!({}));
    let (options, resources) =
        crate::api::resources::resolve_config_resources(&state.pool, &state.secrets, id, &options)
            .await
            .map_err(|_| anyhow::anyhow!("probe resources unavailable"))?;
    let dir = WorkDir(std::env::temp_dir().join(format!("hysteriax-monitor-{}", Uuid::new_v4())));
    std::fs::create_dir(&dir.0)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o700))?;
    }
    let host: String = snapshot["public_host"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| row.get("public_host"));
    let port: i32 = snapshot["public_port"]
        .as_i64()
        .map(|p| p as i32)
        .unwrap_or_else(|| row.get("public_port"));
    let sni = deployed_sni(&snapshot, row.get("tls_sni"));
    let mut tls = json!({"sni":sni.as_deref().filter(|s|!s.is_empty()).unwrap_or(&host),"insecure":snapshot["tls_skip_verify"].as_bool().unwrap_or_else(||row.get::<bool,_>("tls_skip_verify"))});
    if let Some(ech) = deployment::client_ech_config(&options, &resources)? {
        tls["ech"] = json!(ech);
    }
    if options
        .pointer("/tls/clientCA")
        .and_then(Value::as_str)
        .is_some_and(|v| !v.is_empty())
    {
        let cert=sqlx::query("SELECT client_certificate_enc,client_private_key_enc FROM node_assignments WHERE node_id=$1 AND client_certificate_enc IS NOT NULL AND client_private_key_enc IS NOT NULL ORDER BY created_at LIMIT 1").bind(id).fetch_optional(&state.pool).await?;
        let Some(cert) = cert else {
            return Ok(ProbeResult {
                status: "unconfigured".into(),
                reason: Some("mTLS 缺少客户端证书".into()),
                ..Default::default()
            });
        };
        let cp = dir.0.join("client.crt");
        let kp = dir.0.join("client.key");
        std::fs::write(
            &cp,
            state
                .secrets
                .decrypt(&cert.get::<String, _>("client_certificate_enc"))?,
        )?;
        std::fs::write(
            &kp,
            state
                .secrets
                .decrypt(&cert.get::<String, _>("client_private_key_enc"))?,
        )?;
        tls["clientCertificate"] = json!(cp);
        tls["clientKey"] = json!(kp);
    }
    let token = crate::api::generate_token();
    sqlx::query("INSERT INTO monitoring_probe_tokens(node_id,token_hash,expires_at) VALUES($1,$2,now()+interval '30 seconds') ON CONFLICT(node_id) DO UPDATE SET token_hash=$2,expires_at=excluded.expires_at").bind(id).bind(token_digest(&token)).execute(&state.pool).await?;
    let stats =
        deployment::load_deployed_traffic_stats_port(&state.pool, &state.secrets, id).await?;
    let local = free_port()?;
    let mut forwards =
        vec![json!({"listen":format!("127.0.0.1:{local}"),"remote":format!("127.0.0.1:{stats}")})];
    let target = snapshot
        .get("proxy_probe_url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(url::Url::parse)
        .transpose()?;
    let mut external_local = None;
    if let Some(target) = &target {
        if target.scheme() != "http" {
            bail!("unsupported probe target");
        }
        let lp = free_port()?;
        let remote_host = target.host_str().context("missing target host")?;
        let remote_port = target
            .port_or_known_default()
            .context("missing target port")?;
        forwards.push(json!({"listen":format!("127.0.0.1:{lp}"),"remote":address(remote_host,remote_port as i32)}));
        external_local = Some(lp);
    }
    let mut client =
        json!({"server":address(&host,port),"auth":token,"tls":tls,"tcpForwarding":forwards});
    let (_, realm) = deployment::deployment_probe_server_config(
        &options,
        snapshot["listen_addr"].as_str().unwrap_or(":443"),
    )?;
    if let Some(realm) = realm {
        let (uri, _) = deployment::deployment_probe_server_config(&options, ":443")?;
        client["server"] = json!(uri);
        client["realm"] = realm;
    }
    if let Some(obfs) = options.get("obfs") {
        client["obfs"] = obfs.clone();
    }
    let config = dir.0.join("client.yaml");
    std::fs::write(&config, serde_yaml::to_string(&client)?)?;
    let client_log = dir.0.join("client.log");
    let log_file = std::fs::File::create(&client_log)?;
    let mut process = Command::new(binary)
        .args(["--disable-update-check", "client", "-c"])
        .arg(&config)
        .env("HYSTERIA_DISABLE_UPDATE_CHECK", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(log_file))
        .kill_on_drop(true)
        .spawn()?;
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let stats_secret = state
        .secrets
        .decrypt(&row.get::<String, _>("traffic_stats_secret_enc"))?;
    let stats_url = format!("http://127.0.0.1:{local}/online");
    let start = Instant::now();
    // Establish and verify an authenticated proxy session first, then time a separate request.
    loop {
        if process.try_wait()?.is_some() {
            let log = std::fs::read_to_string(&client_log).unwrap_or_default();
            return Err(ProbeFailure(client_failure(&log)).into());
        }
        if let Ok(response) = http
            .get(&stats_url)
            .header("Authorization", &stats_secret)
            .send()
            .await
            && response.status().is_success()
        {
            let body = response.bytes().await?;
            if serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| v.get(format!("monitor-{id}")).and_then(Value::as_u64))
                .is_some_and(|n| n > 0)
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let connection_ms = start.elapsed().as_secs_f64() * 1000.0;
    let timing = Instant::now();
    let response = http
        .get(&stats_url)
        .header("Authorization", &stats_secret)
        .send()
        .await?;
    response.error_for_status()?.bytes().await?;
    let mut latency_ms = timing.elapsed().as_secs_f64() * 1000.0;
    let external = if let (Some(target), Some(lp)) = (target, external_local) {
        let host_header = target[url::Position::BeforeHost..url::Position::AfterPort].to_string();
        let url = format!("http://127.0.0.1:{lp}{}", target.path());
        let external_timing = Instant::now();
        let external_status =
            if let Ok(response) = http.get(url).header("Host", host_header).send().await {
                if response.status().is_success() {
                    "ok"
                } else {
                    "failed"
                }
            } else {
                "failed"
            };
        if external_status == "ok" {
            latency_ms = external_timing.elapsed().as_secs_f64() * 1000.0;
        }
        Some(external_status.into())
    } else {
        None
    };
    process.kill().await?;
    process.wait().await?;
    Ok(ProbeResult {
        status: "ok".into(),
        external: external.clone(),
        connection_ms: Some(connection_ms),
        latency_ms: if external.as_deref() == Some("failed") {
            None
        } else {
            Some(latency_ms)
        },
        reason: if external.as_deref() == Some("failed") {
            Some("外部目标请求失败".into())
        } else {
            None
        },
    })
}
fn address(host: &str, port: i32) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    async fn fixture() -> (AppState, String) {
        let pool = crate::db::test_pool_with_max_connections(3).await;
        let key = base64::engine::general_purpose::STANDARD_NO_PAD.encode([8_u8; 32]);
        let state = AppState::new(pool, crate::security::SecretBox::from_base64(&key).unwrap());
        let (_,axum::Json(created))=crate::api::nodes::create(axum::extract::State(state.clone()),axum::Json(serde_json::from_value(json!({"name":"Probe test","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"unused","public_host":"example.test","public_port":443,"listen_addr":":443"})).unwrap())).await.unwrap();
        (state, created["node"]["id"].as_str().unwrap().into())
    }
    #[tokio::test]
    async fn lease_prevents_duplicate_workers_and_can_be_reclaimed() {
        let (state, node) = fixture().await;
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let (one, two) = tokio::join!(
            acquire_lease(&state.pool, &node, a),
            acquire_lease(&state.pool, &node, b)
        );
        assert_ne!(one.unwrap().is_some(), two.unwrap().is_some());
        sqlx::query("UPDATE monitoring_leases SET expires_at=now()-interval '1 second'")
            .execute(&state.pool)
            .await
            .unwrap();
        assert_eq!(acquire_lease(&state.pool, &node, b).await.unwrap(), Some(b));
    }
    #[tokio::test]
    async fn retention_preserves_accounting_and_removes_only_old_monitoring() {
        let (state, node) = fixture().await;
        for at in ["now()-interval '31 days'", "now()"] {
            sqlx::query(&format!("INSERT INTO online_samples(cycle_id,node_id,sampled_at,status) VALUES($1,$2,{at},'ok')")).bind(Uuid::new_v4()).bind(&node).execute(&state.pool).await.unwrap();
            sqlx::query(&format!("INSERT INTO proxy_probe_samples(node_id,revision,sampled_at,status) VALUES($1,1,{at},'ok')")).bind(&node).execute(&state.pool).await.unwrap();
        }
        sqlx::query("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,sampled_at) VALUES($1,'billing',10,20,now()-interval '31 days')").bind(&node).execute(&state.pool).await.unwrap();
        cleanup(&state).await.unwrap();
        for table in [
            "online_samples",
            "proxy_probe_samples",
            "node_network_samples",
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(count, 1);
        }
    }
    #[tokio::test]
    async fn missing_mtls_certificate_is_unconfigured_not_an_outage() {
        let (state, node) = fixture().await;
        let encrypted = state
            .secrets
            .encrypt(
                &json!({"server_config":{"tls":{"clientCA":"/test/ca.pem"}},"listen_addr":":443"})
                    .to_string(),
            )
            .unwrap();
        sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id=$1")
            .bind(&node)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE config_versions SET config_enc=$2 WHERE node_id=$1")
            .bind(&node)
            .bind(encrypted)
            .execute(&state.pool)
            .await
            .unwrap();
        let row=sqlx::query("SELECT n.*,v.config_enc FROM nodes n JOIN config_versions v ON v.node_id=n.id WHERE n.id=$1").bind(&node).fetch_one(&state.pool).await.unwrap();
        assert_eq!(
            probe_using(&state, &node, &row, "/usr/bin/true")
                .await
                .unwrap()
                .status,
            "unconfigured"
        );
    }
    #[test]
    fn deployed_null_sni_does_not_use_an_undeployed_edit() {
        assert_eq!(
            deployed_sni(&json!({"tls_sni":null}), Some("new.example".into())),
            None
        );
        assert_eq!(
            deployed_sni(
                &json!({"tls_sni":"old.example"}),
                Some("new.example".into())
            ),
            Some("old.example".into())
        );
        assert_eq!(
            deployed_sni(&json!({}), Some("legacy.example".into())),
            Some("legacy.example".into())
        );
    }

    #[test]
    fn client_error_classification_never_returns_log_credentials() {
        assert_eq!(
            client_failure("authentication failed auth=hx_private_test_value"),
            "代理身份验证失败"
        );
        assert_eq!(
            client_failure("FATAL connect error: timeout: no recent network activity"),
            "公网代理连接超时"
        );
        assert_eq!(
            client_failure("bind: address already in use; auth=hx_private_test_value"),
            "本地转发端口已被占用"
        );
    }

    #[test]
    fn temporary_material_is_removed_on_drop() {
        let path = std::env::temp_dir().join(format!("hysteriax-monitor-test-{}", Uuid::new_v4()));
        {
            let dir = WorkDir(path.clone());
            std::fs::create_dir(&dir.0).unwrap();
            std::fs::write(dir.0.join("client.key"), "test-only").unwrap();
        }
        assert!(!path.exists());
    }
}
