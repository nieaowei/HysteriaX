use crate::db;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    api::now,
    deployment,
    security::SecretBox,
    ssh::{self, FingerprintResult},
    state::AppState,
};

pub(crate) mod access;
#[cfg(test)]
mod batch_tests;
pub(crate) mod batches;
pub(crate) mod clear;
pub mod work;

pub async fn run(state: AppState) {
    // Pure DB maintenance keeps ticking independently of remote collection.
    tokio::join!(
        crate::background::run_sampling(state.clone(), "traffic"),
        maintenance_loop(state)
    );
}

async fn maintenance_loop(state: AppState) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if let Err(error) = maintenance_pass(&state).await {
            tracing::warn!(%error,"traffic maintenance deferred");
        }
    }
}
async fn maintenance_pass(state: &AppState) -> Result<()> {
    use crate::background::maintenance;
    for (key, operation) in [
        ("traffic-capture-expiry", "capture_expiry"),
        ("traffic-access-expiry", "access_expiry"),
    ] {
        maintenance::ensure(
            &state.pool,
            key,
            "traffic-maintenance",
            serde_json::json!({"operation":operation}),
        )
        .await?;
    }
    for _ in 0..2 {
        let Some(work) =
            maintenance::claim(&state.pool, "traffic-maintenance", &state.instance_id).await?
        else {
            break;
        };
        crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            let result = match work.payload["operation"].as_str() {
                Some("capture_expiry") => expire_work(state, &work).await,
                Some("access_expiry") => access::scan_work(&state.pool, &work).await,
                _ => Err(anyhow::anyhow!("unsupported traffic maintenance operation")),
            };
            if let Err(error) = result {
                tracing::warn!(%error,"traffic maintenance batch deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .transpose()?;
    }
    access::dispatch_pass(&state.pool, &state.instance_id).await
}

async fn expire_work(state: &AppState, work: &crate::background::maintenance::Work) -> Result<()> {
    let work = work.clone();
    db::write(&state.pool, move |tx| {
        let work = work.clone();
        Box::pin(async move {
            work.guard_on(tx).await?;
            batches::expire_captures_on(tx).await?;
            work.finish_on(tx, false, 1, None).await
        })
    })
    .await
}

pub async fn finish_captured_for_removal(pool: &PgPool, node: &str) -> Result<()> {
    let owned = node.to_owned();
    db::write(pool,move |tx| {
        let node=owned.clone();
        Box::pin(async move {
            sqlx::query("UPDATE traffic_sample_batches SET state='discarded',completed_at=clock_timestamp() WHERE node_id=$1 AND state='capturing' AND EXISTS(SELECT 1 FROM nodes WHERE id=$1 AND state IN ('deleting','delete_failed'))")
                .bind(&node).execute(&mut **tx).await?;
            Ok::<_,anyhow::Error>(())
        })
    }).await?;
    loop {
        let id:Option<String>=sqlx::query_scalar("SELECT sample_id FROM traffic_sample_batches WHERE node_id=$1 AND state='ready' ORDER BY sample_order LIMIT 1")
            .bind(node).fetch_optional(pool).await?;
        let Some(id) = id else {
            return Ok(());
        };
        if !batches::drain(pool, &id).await? {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

pub(crate) async fn sample_work(state: &AppState, work: &crate::background::Work) -> Result<()> {
    if batches::captured(&state.pool, &work.id).await? {
        batches::drain(&state.pool, &work.id).await?;
        return Ok(());
    }
    let requested: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM traffic_sample_batches WHERE sample_id=$1 AND clear_requested_at IS NOT NULL AND payload_digest IS NULL AND state='capturing')")
        .bind(&work.id).fetch_one(&state.pool).await?;
    if requested {
        clear::uncertain(&state.pool, work).await?;
        bail!("traffic clear response was not persisted");
    }
    if !crate::runtime_config::traffic_capture_enabled() {
        return Ok(());
    }
    crate::background::admit_read(&state.pool, work).await?;
    // Individual SSH and HTTP operations have bounded budgets. Do not wrap
    // the whole function in a timeout that could discard a received response
    // while its database transaction is retrying.
    let result = sample_node(&state.pool, &state.secrets, work).await;
    if let Err(error) = &result
        && !clear::uncertain(&state.pool, work).await?
    {
        record_failed_sample(
            &state.pool,
            &work.node,
            work.cycle,
            work.period_start,
            work.expected,
            error,
        )
        .await?;
    }
    result
}

async fn record_failed_sample(
    pool: &PgPool,
    node: &str,
    cycle: Uuid,
    started: chrono::DateTime<chrono::Utc>,
    expected: i64,
    error: &anyhow::Error,
) -> Result<()> {
    let (node, id, reason) = (
        node.to_owned(),
        format!("{cycle}:{node}"),
        classify_sample_error(error),
    );
    db::write(pool,move |tx| {
        let (node,id)=(node.clone(),id.clone());
        Box::pin(async move {
            let active:Option<bool>=sqlx::query_scalar("SELECT true FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
                .bind(&node).fetch_optional(&mut **tx).await?;
            if active.is_none() {return Ok(());}
            // A timeout may fire just after capture commit. Check after locking
            // the node, so sealing and failure admission cannot race.
            let captured:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM traffic_sample_batches WHERE sample_id=$1 AND payload_digest IS NOT NULL)")
                .bind(&id).fetch_one(&mut **tx).await?;
            if captured {return Ok(());}
            sqlx::query("UPDATE traffic_sample_batches SET state='discarded',completed_at=clock_timestamp() WHERE sample_id=$1 AND state='capturing'")
                .bind(&id).execute(&mut **tx).await?;
            crate::overview::record_online_on(tx,&node,cycle,started,expected,None).await?;
            sqlx::query("UPDATE online_samples SET traffic_status='failed' WHERE node_id=$1 AND cycle_id=$2")
                .bind(&node).bind(cycle).execute(&mut **tx).await?;
            record_gap_in_tx(tx,&node,reason,&now()).await?;
            sqlx::query("UPDATE nodes SET state=CASE WHEN state IN ('deployed','syncing') THEN 'unreachable' ELSE state END,updated_at=clock_timestamp() WHERE id=$1")
                .bind(&node).execute(&mut **tx).await?;
            Ok(())
        })
    }).await
}

#[cfg(test)]
async fn collect_nodes<F, Fut>(ids: Vec<String>, mut sample: F)
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut tasks = tokio::task::JoinSet::new();
    let mut ids = ids.into_iter();
    loop {
        while tasks.len() < 4 {
            let Some(id) = ids.next() else {
                break;
            };
            let future = sample(id.clone());
            tasks.spawn(async move { (id, future.await) });
        }
        match tasks.join_next().await {
            Some(Ok((node_id, Err(error)))) => {
                tracing::warn!(%node_id, %error, "traffic sample could not be saved")
            }
            Some(Err(error)) => tracing::error!(%error, "traffic sample task panicked"),
            Some(Ok(_)) => {}
            None => break,
        }
    }
}

async fn sample_node(
    pool: &PgPool,
    secrets: &SecretBox,
    work: &crate::background::Work,
) -> Result<()> {
    let node_id = &work.node;
    let cycle = work.cycle;
    let cycle_started = work.period_start;
    let expected_nodes = work.expected;
    let sample_id = format!("{cycle}:{node_id}");
    let state = batches::reserve_work(pool, work).await?;
    if state != "capturing" {
        batches::drain(pool, &sample_id).await?;
        return Ok(());
    }
    let node = deployment::load_ssh_node(pool, secrets, node_id).await?;
    let session = match ssh::connect(&node).await? {
        FingerprintResult::Trusted(session) => session,
        FingerprintResult::NeedsConfirmation { fingerprint } => {
            bail!("SSH host fingerprint needs confirmation: {fingerprint}")
        }
        FingerprintResult::Changed { expected, observed } => {
            bail!("SSH host fingerprint changed from {expected} to {observed}")
        }
    };
    if let Err(error) = tokio::time::timeout(
        Duration::from_secs(crate::runtime_config::operation_timeout_seconds(8)),
        crate::node_acme::sample(pool, secrets, node_id, &session),
    )
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("ACME status collection timed out")))
    {
        tracing::debug!(node_id, %error, "ACME observation unavailable");
    }
    crate::kick_requests::resume_node(pool, node_id).await?;
    let before_instance = systemd_instance(&session).await?;
    let stats_secret_enc: String =
        sqlx::query_scalar("SELECT traffic_stats_secret_enc FROM nodes WHERE id = $1")
            .bind(node_id)
            .fetch_one(pool)
            .await?;
    let stats_secret = secrets.decrypt(&stats_secret_enc)?;
    let traffic_stats_port =
        deployment::load_deployed_traffic_stats_port(pool, secrets, node_id).await?;
    // Online failures are isolated from the accounting transaction.
    let online = session
        .loopback_http_get(u32::from(traffic_stats_port), "/online", &stats_secret)
        .await;
    if let Err(error) = crate::overview::record_online(
        pool,
        node_id,
        cycle,
        cycle_started,
        expected_nodes,
        online.ok().as_deref(),
    )
    .await
    {
        tracing::warn!(node_id, %error, "online sample could not be saved");
    }
    match clear::prepare(pool, work).await? {
        clear::Admission::Send => {}
        clear::Admission::AlreadyRequested => bail!("traffic clear response was not persisted"),
        clear::Admission::Captured => {
            batches::drain(pool, &sample_id).await?;
            return Ok(());
        }
    };
    let body = session
        .loopback_traffic_clear(u32::from(traffic_stats_port), &stats_secret)
        .await?;
    let counters = parse_response(&body)?;
    capture_clear_response(
        pool,
        work,
        &before_instance,
        &counters,
        &body,
        systemd_instance(&session),
    )
    .await?;
    batches::drain(pool, &sample_id).await?;
    Ok(())
}

async fn capture_clear_response(
    pool: &PgPool,
    work: &crate::background::Work,
    before_instance: &str,
    counters: &Map<String, Value>,
    body: &[u8],
    observe_instance: impl std::future::Future<Output = Result<String>>,
) -> Result<()> {
    // The response is already destructive. Make the counters recoverable before
    // polling another network operation, including across a process crash.
    batches::seal_clear(pool, work, before_instance, counters, body, false).await?;
    match observe_instance.await {
        Ok(after_instance) if after_instance == before_instance => {
            batches::seal_clear(pool, work, before_instance, counters, body, true).await?;
        }
        Ok(_) => {
            tracing::warn!(node_id=%work.node, "hysteria restarted during clear request; captured counters retained")
        }
        Err(error) => {
            tracing::warn!(node_id=%work.node, %error, "clear instance verification failed; captured counters retained")
        }
    }
    Ok(())
}

async fn systemd_instance(session: &ssh::SshSession) -> Result<String> {
    let direct = session
        .execute_read_only("systemctl show hysteriax.service --property=InvocationID --value")
        .await?;
    let value = if direct.code == 0 {
        direct.stdout
    } else {
        session
            .execute_read_only_checked(
                "sudo -n systemctl show hysteriax.service --property=InvocationID --value",
            )
            .await?
    };
    let instance = value.trim();
    if instance.is_empty() {
        bail!("systemd did not report a Hysteria service instance id")
    }
    Ok(instance.to_owned())
}

#[cfg(test)]
async fn apply_sample(
    pool: &PgPool,
    node_id: &str,
    instance_id: &str,
    counters: &Map<String, Value>,
) -> Result<()> {
    parse_counters(counters)?;
    let id = Uuid::new_v4().to_string();
    batches::reserve(pool, &id, node_id, None).await?;
    batches::seal(pool, &id, instance_id, counters).await?;
    batches::drain(pool, &id).await?;
    Ok(())
}

fn parse_response(body: &[u8]) -> Result<Map<String, Value>> {
    use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
    #[derive(serde::Deserialize)]
    struct Counter {
        tx: u64,
        rx: u64,
    }
    struct Response(Map<String, Value>);
    impl<'de> Deserialize<'de> for Response {
        fn deserialize<D: Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            struct ResponseVisitor;
            impl<'de> Visitor<'de> for ResponseVisitor {
                type Value = Response;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a map of unique identities to traffic counters")
                }
                fn visit_map<A: MapAccess<'de>>(
                    self,
                    mut input: A,
                ) -> std::result::Result<Response, A::Error> {
                    let mut counters = Map::new();
                    while let Some((id, counter)) = input.next_entry::<String, Counter>()? {
                        if counter.tx > i64::MAX as u64 || counter.rx > i64::MAX as u64 {
                            return Err(serde::de::Error::custom(
                                "traffic counter exceeds supported range",
                            ));
                        }
                        if counters
                            .insert(id, serde_json::json!({"tx":counter.tx,"rx":counter.rx}))
                            .is_some()
                        {
                            return Err(serde::de::Error::custom("duplicate traffic identity"));
                        }
                    }
                    Ok(Response(counters))
                }
            }
            deserializer.deserialize_map(ResponseVisitor)
        }
    }
    let mut decoder = serde_json::Deserializer::from_slice(body);
    let response = Response::deserialize(&mut decoder)
        .context("Hysteria traffic API returned invalid counters")?;
    decoder
        .end()
        .context("Hysteria traffic API returned trailing data")?;
    Ok(response.0)
}

fn parse_counters(counters: &Map<String, Value>) -> Result<Vec<(String, u64, u64)>> {
    counters
        .iter()
        .map(|(user_id, value)| {
            let object = value
                .as_object()
                .context("Hysteria traffic API returned an invalid user counter")?;
            let tx = object
                .get("tx")
                .and_then(Value::as_u64)
                .context("Hysteria traffic API returned an invalid upload counter")?;
            let rx = object
                .get("rx")
                .and_then(Value::as_u64)
                .context("Hysteria traffic API returned an invalid download counter")?;
            if tx > i64::MAX as u64 || rx > i64::MAX as u64 {
                bail!("Hysteria traffic API counter exceeds the supported range");
            }
            Ok((user_id.clone(), tx, rx))
        })
        .collect()
}

#[cfg(test)]
async fn record_gap(pool: &PgPool, node_id: &str, reason: &str) -> Result<()> {
    let (node, reason) = (node_id.to_owned(), reason.to_owned());
    db::write(pool,move |tx| {
        let (node,reason)=(node.clone(),reason.clone());
        Box::pin(async move {
            let active:Option<bool>=sqlx::query_scalar("SELECT true FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
                .bind(&node).fetch_optional(&mut **tx).await?;
            if active.is_some() {record_gap_in_tx(tx,&node,&reason,&now()).await?;}
            Ok(())
        })
    }).await
}

async fn record_gap_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    node_id: &str,
    reason: &str,
    timestamp: &chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(node_id)
        .fetch_optional(&mut **tx)
        .await?;
    let open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM data_gaps WHERE node_id = $1 AND resolved_at IS NULL",
    )
    .bind(node_id)
    .fetch_one(&mut **tx)
    .await?;
    if open == 0 {
        sqlx::query(
            "INSERT INTO data_gaps (id, node_id, opened_at, reason) VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(node_id)
        .bind(timestamp)
        .bind(reason)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

fn classify_sample_error(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    if message.contains("fingerprint") {
        "ssh_host_key_unavailable"
    } else {
        "traffic_sample_failed"
    }
}

#[cfg(test)]
mod tests;
