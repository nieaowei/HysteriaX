use super::*;
use crate::kick_requests::{self, Request};

fn offline(requests: &[Request], online: &Value) -> Vec<Request> {
    requests
        .iter()
        .filter(|r| online.get(&r.user).and_then(Value::as_u64).unwrap_or(0) == 0)
        .cloned()
        .collect()
}
async fn refresh(pool: &PgPool, job: &JobInput, requests: &[Request]) -> Result<Vec<Request>> {
    let users: Vec<_> = requests.iter().map(|r| r.user.clone()).collect();
    let current = kick_requests::batch(pool, job, Some(&users)).await?;
    let cleared: Vec<_> = current.iter().filter(|r| !r.required).cloned().collect();
    kick_requests::acknowledge(pool, job, &cleared, "cancelled").await?;
    Ok(current.into_iter().filter(|r| r.required).collect())
}

pub(super) async fn kick(pool: &PgPool, secrets: &SecretBox, job: &JobInput) -> Result<JobOutput> {
    anyhow::ensure!(
        job.payload == json!({"scope":"node"}),
        "kick job must use node scope"
    );
    let node_id = job
        .node_id
        .as_deref()
        .context("kick job is missing node_id")?;
    let mut connection = None;
    for _ in 0..kick_requests::BATCHES_PER_PASS {
        let requests = kick_requests::batch(pool, job, None).await?;
        if requests.is_empty() {
            break;
        }
        let pending = refresh(pool, job, &requests).await?;
        if pending.is_empty() {
            continue;
        }
        if connection.is_none() {
            let node = load_ssh_node(pool, secrets, node_id).await?;
            let session = match ssh::connect(&node).await? {
                FingerprintResult::Trusted(s) => s,
                FingerprintResult::NeedsConfirmation { fingerprint } => {
                    bail!("confirm this SSH host fingerprint before kicking clients: {fingerprint}")
                }
                FingerprintResult::Changed { expected, observed } => {
                    bail!("SSH host key changed: expected {expected}; observed {observed}")
                }
            };
            let cipher: String =
                sqlx::query_scalar("SELECT traffic_stats_secret_enc FROM nodes WHERE id=$1")
                    .bind(node_id)
                    .fetch_one(pool)
                    .await?;
            connection = Some((
                session,
                secrets.decrypt(&cipher)?,
                load_deployed_traffic_stats_port(pool, secrets, node_id).await?,
            ));
        }
        let (session, secret, port) = connection.as_ref().unwrap();
        disconnect_batch(
            pool,
            job,
            pending,
            &NodeTransport {
                session,
                secret,
                port: *port,
            },
        )
        .await?;
        report_progress(
            pool,
            job,
            "kicking_clients",
            "A batch of at most 200 users has been checked and checkpointed.",
        )
        .await?;
    }
    let (completed,pending):(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE state IN ('completed','cancelled')),count(*) FILTER(WHERE state='active') FROM kick_requests WHERE node_id=$1 AND latest_job_id=$2").bind(node_id).bind(&job.id).fetch_one(pool).await?;
    let mut result = json!({"node_id":node_id,"processed_users":completed,"pending_users":pending});
    if pending == 0 {
        result["remaining_connections"] = json!(0);
    }
    Ok(JobOutput {
        stage: if pending > 0 {
            "batch_continue"
        } else {
            "clients_offline"
        }
        .into(),
        result,
        node_state: None,
        deployed_revision: None,
        deployed_config: None,
        deployed_sha256: None,
        published_connection: None,
        delete_node: false,
    })
}

trait Transport: Sync {
    fn online(&self) -> impl std::future::Future<Output = Result<Value>> + Send;
    fn disconnect(&self, users: &[String]) -> impl std::future::Future<Output = Result<()>> + Send;
}
struct NodeTransport<'a> {
    session: &'a ssh::SshSession,
    secret: &'a str,
    port: u16,
}
impl Transport for NodeTransport<'_> {
    async fn online(&self) -> Result<Value> {
        serde_json::from_slice(
            &self
                .session
                .loopback_http_get(u32::from(self.port), "/online", self.secret)
                .await?,
        )
        .context("Hysteria online API returned invalid JSON")
    }
    async fn disconnect(&self, users: &[String]) -> Result<()> {
        self.session
            .loopback_http_post(
                u32::from(self.port),
                "/kick",
                self.secret,
                &serde_json::to_vec(users)?,
            )
            .await?;
        Ok(())
    }
}
async fn disconnect_batch(
    pool: &PgPool,
    job: &JobInput,
    mut pending: Vec<Request>,
    transport: &impl Transport,
) -> Result<()> {
    let mut remaining = 0;
    for attempt in 0..6 {
        // Query online before issuing any kick, avoiding stale markers for offline users.
        let online = transport.online().await?;
        // The network read may span a regrant/renewal. Read fresh obligations before dispatch.
        pending = refresh(pool, job, &pending).await?;
        let done = offline(&pending, &online);
        kick_requests::acknowledge(pool, job, &done, "completed").await?;
        pending.retain(|r| !done.iter().any(|d| d.user == r.user));
        if pending.is_empty() {
            break;
        }
        let users: Vec<String> = pending.iter().map(|r| r.user.clone()).collect();
        transport.disconnect(&users).await?;
        let online = transport.online().await?;
        let done = offline(&pending, &online);
        // Only the exact generations that were checked/sent may be acknowledged.
        kick_requests::acknowledge(pool, job, &done, "completed").await?;
        pending.retain(|r| !done.iter().any(|d| d.user == r.user));
        if pending.is_empty() {
            break;
        }
        remaining = pending
            .iter()
            .map(|r| online.get(&r.user).and_then(Value::as_u64).unwrap_or(0))
            .sum();
        if attempt < 5 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    if !pending.is_empty() {
        return Err(ssh::SshError::ClientsStillOnline { remaining }.into());
    }
    Ok(())
}
#[cfg(test)]
mod tests;
