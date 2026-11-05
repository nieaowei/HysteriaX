//! Public-endpoint probes run locally on the management host, never on the node.
use crate::{deployment, security::token_digest, state::AppState};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::Row;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::process::Command;
use uuid::Uuid;

#[cfg(test)]
pub(crate) mod token_tests;

fn probe_directory_prefix() -> String {
    let raw = std::env::var("DATABASE_URL").unwrap_or_default();
    let identity = url::Url::parse(&raw)
        .map(|mut url| {
            let _ = url.set_password(None);
            url.to_string()
        })
        .unwrap_or(raw);
    format!("hysteriax-monitor-{}-", &token_digest(&identity)[..16])
}

pub fn cleanup_probe_directories() -> Result<()> {
    cleanup_probe_directories_in(&std::env::temp_dir(), &probe_directory_prefix())
}

fn cleanup_probe_directories_in(root: &std::path::Path, prefix: &str) -> Result<()> {
    for item in std::fs::read_dir(root)? {
        let item = item?;
        let name = item.file_name();
        let name = name.to_string_lossy();
        let Some(suffix) = name.strip_prefix(prefix) else {
            continue;
        };
        let valid = suffix.is_ascii()
            && suffix.len() == 73
            && suffix.as_bytes()[36] == b'-'
            && Uuid::parse_str(&suffix[..36]).is_ok()
            && Uuid::parse_str(&suffix[37..]).is_ok();
        if !valid {
            continue;
        }
        let kind = match item.file_type() {
            Ok(kind) => kind,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !kind.is_dir() {
            continue;
        }

        // A live or paused process holds this OS lock for the directory's lifetime.
        // A missing lock belongs to the old format; only offline maintenance may
        // remove that material, since another old instance may still be using it.
        let lock = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(item.path().join("owner.lock"))
        {
            Ok(lock) => lock,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        match lock.try_lock() {
            Ok(()) => match std::fs::remove_dir_all(item.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            },
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
    Ok(())
}

struct WorkDir(PathBuf, Option<std::fs::File>);
impl WorkDir {
    fn create(root: &std::path::Path, prefix: &str, instance: &str) -> Result<Self> {
        let operation = Uuid::new_v4();
        // Publish only after holding the owner lock, so startup cleanup cannot
        // race with creation. Initializing paths are never cleanup candidates.
        let staging = root.join(format!("{prefix}{instance}-{operation}.initializing"));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&staging)?;
        let mut dir = Self(staging, None);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(dir.0.join("owner.lock"))?;
        lock.try_lock()?;
        dir.1 = Some(lock);
        let path = root.join(format!("{prefix}{instance}-{operation}"));
        std::fs::rename(&dir.0, &path)?;
        dir.0 = path;
        Ok(dir)
    }
}
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Clone, Default)]
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
    tokio::join!(
        crate::background::run_sampling(state.clone(), "proxy"),
        retention_loop(state)
    );
}
const RETENTION_TABLES: [&str; 3] = [
    "online_samples",
    "proxy_probe_samples",
    "monitoring_probe_tokens",
];
async fn retention_loop(state: AppState) {
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        timer.tick().await;
        if let Err(error) = retention_pass(&state).await {
            tracing::warn!(%error,"monitoring retention cleanup deferred");
        }
    }
}
async fn retention_pass(state: &AppState) -> Result<()> {
    use crate::background::maintenance;
    for table in RETENTION_TABLES {
        maintenance::ensure(
            &state.pool,
            &format!("monitoring-retention:{table}"),
            "monitoring-retention",
            json!({"table":table}),
        )
        .await?;
    }
    // Three independent categories, one bounded transaction for each pass.
    for _ in 0..3 {
        let Some(work) =
            maintenance::claim(&state.pool, "monitoring-retention", &state.instance_id).await?
        else {
            break;
        };
        crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = cleanup_work(state, &work).await {
                tracing::warn!(%error,"monitoring cleanup batch deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .transpose()?;
    }
    Ok(())
}
async fn cleanup_work(state: &AppState, work: &crate::background::maintenance::Work) -> Result<()> {
    let work = work.clone();
    let table = work.payload["table"]
        .as_str()
        .context("missing retention table")?;
    anyhow::ensure!(
        RETENTION_TABLES.contains(&table),
        "unsupported retention table"
    );
    let (key, at, age_days, interval) = if table == "monitoring_probe_tokens" {
        ("node_id", "expires_at", 0_i32, 60)
    } else {
        ("id", "sampled_at", 31_i32, 86400)
    };
    let delete = format!(
        "DELETE FROM {table} WHERE {key} IN (SELECT {key} FROM {table} WHERE {at}<$1 ORDER BY {at},{key} LIMIT 256 FOR UPDATE SKIP LOCKED)"
    );
    let remaining = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {at}<$1)");
    crate::db::write(&state.pool, move |tx| {
        let (work, delete, remaining) = (work.clone(), delete.clone(), remaining.clone());
        Box::pin(async move {
            work.guard_on(tx).await?;
            // Bind one DB-time cutoff so empty and nonempty scans can use
            // range indexes instead of filtering with a volatile expression.
            let cutoff: DateTime<Utc> =
                sqlx::query_scalar("SELECT clock_timestamp()-make_interval(days=>$1)")
                    .bind(age_days)
                    .fetch_one(&mut **tx)
                    .await?;
            sqlx::query(sqlx::AssertSqlSafe(delete.as_str()))
                .bind(cutoff)
                .execute(&mut **tx)
                .await?;
            let more: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(remaining.as_str()))
                .bind(cutoff)
                .fetch_one(&mut **tx)
                .await?;
            work.finish_on(tx, more, if more { 1 } else { interval }, None)
                .await
        })
    })
    .await
}
#[cfg(test)]
async fn cleanup(state: &AppState) -> Result<()> {
    retention_pass(state).await
}
pub(crate) async fn sample_work(state: &AppState, work: &crate::background::Work) -> Result<()> {
    let revision = work.payload["deployed_revision"]
        .as_i64()
        .context("missing proxy revision")?;
    let started: DateTime<Utc> = serde_json::from_value(work.payload["captured_at"].clone())?;
    let recorded: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM proxy_probe_samples WHERE node_id=$1 AND sampled_at=$2)",
    )
    .bind(&work.node)
    .bind(started)
    .fetch_one(&state.pool)
    .await?;
    if recorded {
        return Ok(());
    }
    crate::background::admit_read(&state.pool, work).await?;
    let row = sqlx::query("SELECT n.*,v.config_enc FROM nodes n JOIN config_versions v ON v.node_id=n.id AND v.revision=n.deployed_revision WHERE n.id=$1 AND n.deployed_revision=$2 AND n.state NOT IN ('deleting','delete_failed') AND n.mtls_state='ready'")
        .bind(&work.node).bind(revision).fetch_optional(&state.pool).await?;
    let Some(row) = row else {
        return Ok(());
    };
    let token = crate::api::generate_token();
    let timeout_seconds = crate::runtime_config::operation_timeout_seconds(15);
    let result = match tokio::time::timeout(
        Duration::from_secs(timeout_seconds),
        probe(state, work, &row, &token),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(error)) if error.is::<crate::background::SlotExpired>() => return Err(error),
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
            reason: Some(format!("探测超过 {timeout_seconds} 秒")),
            ..Default::default()
        },
    };
    save_result(state, work, result, &token_digest(&token)).await
}

pub(crate) fn identity(node: &str, hash: &str) -> String {
    format!("monitor-{node}-{hash}")
}

fn token_leases(
    work: &crate::background::Work,
) -> Result<(&crate::execution::Execution, &crate::execution::Execution)> {
    anyhow::ensure!(
        work.kind == "proxy",
        "monitoring credential requires proxy work"
    );
    let primary = work
        .leases
        .iter()
        .find(|l| {
            l.resource == format!("work:{}", work.id)
                && l.work == work.id
                && !l.owner.is_empty()
                && l.generation > 0
        })
        .context("missing monitoring work lease")?;
    let node = work
        .leases
        .iter()
        .find(|l| {
            l.resource == format!("node:{}", work.node)
                && l.work == work.id
                && l.owner == primary.owner
                && l.epoch == primary.epoch
                && l.generation > 0
        })
        .context("missing monitoring node lease")?;
    Ok((primary, node))
}

async fn guard_token_work(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    work: &crate::background::Work,
) -> Result<()> {
    let (primary, _) = token_leases(work)?;
    for lease in &work.leases {
        lease.guard(tx).await?;
    }
    let current: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_work WHERE id=$1 AND kind='proxy' AND node_id=$2 AND cycle_id=$3 AND payload=$4 AND state='running' AND owner=$5 AND generation=$6 FOR UPDATE)")
        .bind(&work.id).bind(&work.node).bind(work.cycle).bind(&work.payload)
        .bind(&primary.owner).bind(primary.generation).fetch_one(&mut **tx).await?;
    anyhow::ensure!(current, "monitoring work source changed or ended");
    Ok(())
}

async fn install_token(state: &AppState, work: &crate::background::Work, hash: &str) -> Result<()> {
    token_leases(work)?;
    let (work, hash) = (work.clone(), hash.to_owned());
    crate::db::write(&state.pool,move |tx| {
        let (work,hash)=(work.clone(),hash.clone());
        let node=work.node.clone();
        Box::pin(async move {
            guard_token_work(tx,&work).await?;
            let (primary,node_lease)=token_leases(&work)?;
            let revision=work.payload["deployed_revision"].as_i64().context("missing proxy revision")?;
            sqlx::query("SELECT id FROM nodes WHERE id=$1 AND deployed_revision=$2 AND mtls_state='ready' AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
                .bind(&node).bind(revision).fetch_optional(&mut **tx).await?.context("monitoring node source changed")?;
            // Allow the whole probe budget after token admission. Ownership and
            // generation checks still fence every use and result write.
            let token_lifetime = crate::runtime_config::operation_timeout_seconds(15) * 2;
            sqlx::query("INSERT INTO monitoring_probe_tokens(node_id,token_hash,expires_at,work_id,owner,node_generation,work_generation,epoch) VALUES($1,$2,clock_timestamp()+make_interval(secs=>$8::double precision),$3,$4,$5,$6,$7) ON CONFLICT(node_id) DO UPDATE SET token_hash=excluded.token_hash,expires_at=excluded.expires_at,work_id=excluded.work_id,owner=excluded.owner,node_generation=excluded.node_generation,work_generation=excluded.work_generation,epoch=excluded.epoch")
                .bind(node).bind(hash).bind(&work.id).bind(&primary.owner).bind(node_lease.generation).bind(primary.generation).bind(&primary.epoch).bind(token_lifetime as f64).execute(&mut **tx).await?;
            Ok(())
        })
    }).await
}

async fn save_result(
    state: &AppState,
    work: &crate::background::Work,
    result: ProbeResult,
    token_hash: &str,
) -> Result<()> {
    token_leases(work)?;
    let (work, token_hash) = (work.clone(), token_hash.to_owned());
    crate::db::write(&state.pool,move |tx| {
        let (work,result,token_hash)=(work.clone(),result.clone(),token_hash.clone());
        Box::pin(async move {
            guard_token_work(tx,&work).await?;
            let (primary,node_lease)=token_leases(&work)?;
            let id=&work.node;
            let revision=work.payload["deployed_revision"].as_i64().context("missing proxy revision")?;
            let started:DateTime<Utc>=serde_json::from_value(work.payload["captured_at"].clone())?;
            let current:Option<bool>=sqlx::query_scalar("SELECT deployed_revision=$2 AND mtls_state='ready' AND state NOT IN ('deleting','delete_failed') FROM nodes WHERE id=$1 FOR UPDATE")
                .bind(id).bind(revision).fetch_optional(&mut **tx).await?;
            if current==Some(true) {
                let recorded:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM proxy_probe_samples WHERE node_id=$1 AND sampled_at=$2)").bind(id).bind(started).fetch_one(&mut **tx).await?;
                if !recorded {
                    let previous=sqlx::query("SELECT revision,health,failures,successes,sampled_at FROM monitoring_probe_state WHERE node_id=$1 FOR UPDATE").bind(id).fetch_optional(&mut **tx).await?;
                    let prior=previous.as_ref().filter(|r|r.get::<i64,_>("revision")==revision && started-r.get::<DateTime<Utc>,_>("sampled_at")<chrono::Duration::seconds(180));
                    let effective=if result.external.as_deref()==Some("failed") {"failed"}else{&result.status};
                    let health=crate::overview::next_probe_health(prior.map(|r|r.get::<String,_>("health")).as_deref().unwrap_or("checking"),prior.map(|r|r.get("failures")).unwrap_or(0),prior.map(|r|r.get("successes")).unwrap_or(0),effective);
                    sqlx::query("INSERT INTO proxy_probe_samples(node_id,revision,sampled_at,status,external_status,connection_ms,latency_ms,reason) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
                        .bind(id).bind(revision).bind(started).bind(&result.status).bind(&result.external).bind(result.connection_ms).bind(result.latency_ms).bind(&result.reason).execute(&mut **tx).await?;
                    sqlx::query("INSERT INTO monitoring_probe_state(node_id,revision,health,failures,successes,sampled_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(node_id) DO UPDATE SET revision=$2,health=$3,failures=$4,successes=$5,sampled_at=$6 WHERE monitoring_probe_state.sampled_at<$6")
                        .bind(id).bind(revision).bind(health.0).bind(health.1).bind(health.2).bind(started).execute(&mut **tx).await?;
                }
            }
            sqlx::query("DELETE FROM monitoring_probe_tokens WHERE node_id=$1 AND token_hash=$2 AND work_id=$3 AND owner=$4 AND node_generation=$5 AND work_generation=$6 AND epoch=$7")
                .bind(id).bind(token_hash).bind(&work.id).bind(&primary.owner).bind(node_lease.generation).bind(primary.generation).bind(&primary.epoch).execute(&mut **tx).await?;
            Ok(())
        })
    }).await
}
fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

async fn probe(
    state: &AppState,
    work: &crate::background::Work,
    row: &sqlx::postgres::PgRow,
    token: &str,
) -> Result<ProbeResult> {
    let binary = std::env::var("HYSTERIAX_PROBE_BINARY")
        .unwrap_or_else(|_| "/usr/local/bin/hysteria".into());
    probe_using(state, work, row, &binary, token).await
}
async fn probe_using(
    state: &AppState,
    work: &crate::background::Work,
    row: &sqlx::postgres::PgRow,
    binary: &str,
    token: &str,
) -> Result<ProbeResult> {
    let id = &work.node;
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
    let dir = WorkDir::create(
        &std::env::temp_dir(),
        &probe_directory_prefix(),
        &state.instance_id,
    )?;
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
    let policy = crate::credentials::node_mtls::Settings::from_config(&options)?;
    if let Some((ca, _)) = policy.authority() {
        let Some((certificate, private_key)) = crate::credentials::node_mtls::probe_material(
            &state.pool,
            &state.secrets,
            id,
            ca,
            None,
        )
        .await?
        else {
            return Ok(ProbeResult {
                status: "unconfigured".into(),
                reason: Some("托管 mTLS 探测证书不可用".into()),
                ..Default::default()
            });
        };
        let cp = dir.0.join("client.crt");
        let kp = dir.0.join("client.key");
        std::fs::write(&cp, certificate)?;
        std::fs::write(&kp, private_key)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&kp, std::fs::Permissions::from_mode(0o600))?;
        }
        tls["clientCertificate"] = json!(cp);
        tls["clientKey"] = json!(kp);
    }
    install_token(state, work, &token_digest(token)).await?;
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
    crate::background::admit_read(&state.pool, work).await?;
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
        .timeout(Duration::from_secs(
            crate::runtime_config::operation_timeout_seconds(3),
        ))
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
        crate::execution::check_current().await?;
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
                .and_then(|v| {
                    v.get(identity(id, &token_digest(token)))
                        .and_then(Value::as_u64)
                })
                .is_some_and(|n| n > 0)
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let connection_ms = start.elapsed().as_secs_f64() * 1000.0;
    crate::execution::check_current().await?;
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
        crate::execution::check_current().await?;
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
mod tests;

#[cfg(test)]
mod temporary_material_tests;
