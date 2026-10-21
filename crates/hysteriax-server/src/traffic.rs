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

#[cfg(test)]
mod batch_tests;
pub(crate) mod batches;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CounterDelta {
    pub tx: u64,
    pub rx: u64,
    pub gap_reason: Option<&'static str>,
}

#[cfg(test)]
#[path = "traffic/optimization_benchmark.rs"]
mod optimization_benchmark;

pub fn counter_delta(
    previous: Option<(&str, u64, u64)>,
    current_instance: &str,
    current_tx: u64,
    current_rx: u64,
) -> CounterDelta {
    match previous {
        None => CounterDelta {
            tx: current_tx,
            rx: current_rx,
            gap_reason: None,
        },
        Some((instance, _, _)) if instance != current_instance => CounterDelta {
            tx: current_tx,
            rx: current_rx,
            gap_reason: Some("node_restarted_between_samples"),
        },
        Some((_, old_tx, old_rx)) if current_tx < old_tx || current_rx < old_rx => CounterDelta {
            tx: 0,
            rx: 0,
            gap_reason: Some("counter_decreased_without_instance_change"),
        },
        Some((_, old_tx, old_rx)) => CounterDelta {
            tx: current_tx - old_tx,
            rx: current_rx - old_rx,
            gap_reason: None,
        },
    }
}

pub async fn run(state: AppState) {
    // Expiry enforcement must keep ticking even when SSH sampling is slow.
    tokio::join!(
        sampling_loop(&state),
        expiration_loop(&state.pool),
        billing_loop(&state.pool)
    );
}

async fn sampling_loop(state: &AppState) {
    crate::background::run_sampling(state.clone(), "traffic").await;
}

async fn expiration_loop(pool: &PgPool) {
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if let Err(error) = schedule_expirations(pool).await {
            tracing::error!(%error, "failed to schedule expired-user revocations");
        }
    }
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

async fn billing_loop(pool: &PgPool) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if let Err(error) = batches::expire_captures(pool).await {
            tracing::warn!(%error,"expired traffic captures could not be closed");
        }
        for _ in 0..16 {
            match batches::process_next(pool).await {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    tracing::warn!(%error,"captured traffic billing deferred");
                    break;
                }
            }
        }
    }
}

pub(crate) async fn sample_work(state: &AppState, work: &crate::background::Work) -> Result<()> {
    let result = tokio::time::timeout(
        Duration::from_secs(45),
        sample_node(
            &state.pool,
            &state.secrets,
            &work.node,
            work.cycle,
            work.period_start,
            work.expected,
        ),
    )
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("traffic sample timed out")));
    if let Err(error) = &result {
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
            crate::overview::record_online_on(&mut **tx,&node,cycle,started,expected,None).await?;
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
    node_id: &str,
    cycle: Uuid,
    cycle_started: chrono::DateTime<chrono::Utc>,
    expected_nodes: i64,
) -> Result<()> {
    let sample_id = format!("{cycle}:{node_id}");
    let state = batches::reserve(pool, &sample_id, node_id, Some(cycle)).await?;
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
        Duration::from_secs(8),
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
    let body = session
        .loopback_http_get(u32::from(traffic_stats_port), "/traffic", &stats_secret)
        .await?;
    let traffic: Value =
        serde_json::from_slice(&body).context("Hysteria traffic API returned invalid JSON")?;
    let after_instance = systemd_instance(&session).await?;
    if before_instance != after_instance {
        batches::discard_capture(pool, &sample_id).await?;
        record_gap(pool, node_id, "hysteria_restarted_during_sample").await?;
        crate::overview::finish_traffic_sample(pool, node_id, cycle, "failed").await;
        return Ok(());
    }
    let counters = traffic
        .as_object()
        .context("Hysteria traffic API returned an invalid response shape")?;
    batches::seal(pool, &sample_id, &before_instance, counters).await?;
    batches::drain(pool, &sample_id).await?;
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

async fn schedule_expirations(pool: &PgPool) -> Result<()> {
    let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM users WHERE access_kick_enqueued_at IS NULL AND (NOT enabled OR expires_at<=clock_timestamp() OR usage_bytes>=quota_bytes) ORDER BY id")
        .fetch_all(pool).await?;
    for id in ids {
        db::write(pool, move |tx| {
            let id = id.clone();
            Box::pin(async move {
                sqlx::query("SELECT id FROM users WHERE id=$1 FOR NO KEY UPDATE")
                    .bind(&id)
                    .fetch_optional(&mut **tx)
                    .await?;
                batches::enforce_user(tx, &id).await
            })
        })
        .await?;
    }
    Ok(())
}

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
mod tests {
    use serde_json::{Map, Value, json};
    use sqlx::PgPool;

    use super::{apply_sample, counter_delta, record_gap};

    async fn sample_pool(user_id: &str) -> PgPool {
        let pool = crate::db::test_pool().await;
        let timestamp = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        sqlx::query(
            "INSERT INTO users (id, name, created_at, updated_at) VALUES ($1, 'Sample user', $2, $3)",
        )
        .bind(user_id)
        .bind(timestamp)
        .bind(timestamp)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO nodes (id, name, ssh_host, ssh_port, ssh_username, ssh_auth_type, ssh_secret_enc, public_host, public_port, listen_addr, node_token_hash, node_token_enc, traffic_stats_secret_enc, desired_config_enc, created_at, updated_at) VALUES ('node-one', 'Node one', '127.0.0.1', 22, 'root', 'private_key', 'ssh-secret', 'node.example.test', 443, ':443', 'node-hash', 'node-token', 'stats-secret', '{}', $1, $2)")
            .bind(timestamp)
            .bind(timestamp)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO node_assignments (user_id, node_id, credential_hash, credential_enc, created_at) VALUES ($1, 'node-one', 'credential-hash', 'credential', $2)")
            .bind(user_id)
            .bind(timestamp)
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    fn counters(user_id: &str, tx: u64, rx: u64) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::from_iter([(user_id.to_owned(), json!({"tx": tx, "rx": rx}))])
    }

    #[tokio::test]
    async fn slow_and_failed_nodes_do_not_block_peers_and_concurrency_is_bounded() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let slow = Arc::new(tokio::sync::Notify::new());
        let peers = Arc::new(tokio::sync::Notify::new());
        let worker = {
            let (active, peak, completed, slow, peers) = (
                active.clone(),
                peak.clone(),
                completed.clone(),
                slow.clone(),
                peers.clone(),
            );
            tokio::spawn(async move {
                super::collect_nodes((0..12).map(|i| i.to_string()).collect(), |id| {
                    let (active, peak, completed, slow, peers) = (
                        active.clone(),
                        peak.clone(),
                        completed.clone(),
                        slow.clone(),
                        peers.clone(),
                    );
                    async move {
                        let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(count, Ordering::SeqCst);
                        if id == "0" {
                            slow.notified().await;
                        } else {
                            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                        }
                        active.fetch_sub(1, Ordering::SeqCst);
                        if completed.fetch_add(1, Ordering::SeqCst) == 10 {
                            peers.notify_one();
                        }
                        if id == "1" {
                            anyhow::bail!("injected sample failure");
                        }
                        Ok(())
                    }
                })
                .await;
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), peers.notified())
            .await
            .unwrap();
        assert_eq!(completed.load(Ordering::SeqCst), 11);
        assert_eq!(peak.load(Ordering::SeqCst), 4);
        slow.notify_one();
        worker.await.unwrap();
        assert_eq!(completed.load(Ordering::SeqCst), 12);
    }

    #[test]
    fn repeated_samples_add_only_new_counter_bytes() {
        assert_eq!(counter_delta(Some(("a", 100, 200)), "a", 140, 260).tx, 40);
        assert_eq!(counter_delta(Some(("a", 100, 200)), "a", 140, 260).rx, 60);
    }

    #[test]
    fn a_restart_starts_a_new_counter_baseline_and_records_a_gap() {
        let delta = counter_delta(
            Some(("old-instance", 10_000, 20_000)),
            "new-instance",
            120,
            250,
        );
        assert_eq!(delta.tx, 120);
        assert_eq!(delta.rx, 250);
        assert_eq!(delta.gap_reason, Some("node_restarted_between_samples"));
    }

    #[test]
    fn a_decreasing_counter_never_creates_negative_usage() {
        let delta = counter_delta(Some(("instance", 1_000, 2_000)), "instance", 10, 25);
        assert_eq!(delta.tx, 0);
        assert_eq!(delta.rx, 0);
        assert!(delta.gap_reason.is_some());
    }

    #[tokio::test]
    async fn malformed_sample_is_rejected_before_any_user_is_charged() {
        let pool = sample_pool("user-a").await;
        let counters = Map::<String, Value>::from_iter([
            ("user-a".to_owned(), json!({"tx": 100, "rx": 50})),
            ("user-z".to_owned(), json!({"tx": "invalid", "rx": 25})),
        ]);

        assert!(
            apply_sample(&pool, "node-one", "instance-one", &counters)
                .await
                .is_err()
        );
        record_gap(&pool, "node-one", "traffic_sample_failed")
            .await
            .unwrap();

        let usage: i64 = sqlx::query_scalar("SELECT usage_bytes FROM users WHERE id = 'user-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let baselines: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM traffic_baselines WHERE node_id = 'node-one'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let records: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM traffic_records WHERE node_id = 'node-one'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let gaps: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM data_gaps WHERE node_id = 'node-one' AND resolved_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(usage, 0);
        assert_eq!(baselines, 0);
        assert_eq!(records, 0);
        assert_eq!(gaps, 1);
    }

    #[tokio::test]
    async fn a_sample_applied_after_quota_reset_is_charged_to_the_new_period() {
        let pool = sample_pool("late-user").await;
        sqlx::query("UPDATE users SET usage_bytes = 450 WHERE id = 'late-user'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO traffic_baselines (node_id, user_id, instance_id, tx_total, rx_total, sampled_at) VALUES ('node-one', 'late-user', 'instance-one', 100, 100, '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();

        // The remote counter snapshot was taken before reset but persisted after it.
        sqlx::query("UPDATE users SET usage_bytes = 0, quota_reset_at = '2026-01-01T00:00:10Z' WHERE id = 'late-user'")
            .execute(&pool)
            .await
            .unwrap();
        apply_sample(
            &pool,
            "node-one",
            "instance-one",
            &counters("late-user", 140, 130),
        )
        .await
        .unwrap();

        let usage: i64 = sqlx::query_scalar("SELECT usage_bytes FROM users WHERE id = 'late-user'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let deltas: (i64, i64) = sqlx::query_as(
            "SELECT delta_tx, delta_rx FROM traffic_records WHERE user_id = 'late-user'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(usage, 70);
        assert_eq!(deltas, (40, 30));
    }

    #[tokio::test]
    async fn same_user_usage_aggregates_across_nodes_and_enqueues_one_kick_per_node() {
        let pool = crate::db::test_pool().await;

        let user_id = "quota-user";
        let timestamp = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        sqlx::query("INSERT INTO users (id, name, enabled, quota_bytes, usage_bytes, revision, created_at, updated_at) VALUES ($1, $2, TRUE, 500, 0, 1, $3, $4)")
            .bind(user_id).bind("Shared user").bind(timestamp).bind(timestamp)
            .execute(&pool).await.unwrap();

        for (index, node_id) in ["node-one", "node-two"].into_iter().enumerate() {
            sqlx::query("INSERT INTO nodes (id, name, ssh_host, ssh_port, ssh_username, ssh_auth_type, ssh_secret_enc, public_host, public_port, listen_addr, node_token_hash, node_token_enc, traffic_stats_secret_enc, desired_config_enc, created_at, updated_at) VALUES ($1, $2, $3, 22, 'root', 'private_key', 'ssh-secret', $4, 443, ':443', $5, 'node-token', 'stats-secret', '{}', $6, $7)")
                .bind(node_id).bind(node_id).bind("127.0.0.1").bind("node.example.test")
                .bind(format!("node-hash-{index}" )).bind(timestamp).bind(timestamp)
                .execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO node_assignments (user_id, node_id, credential_hash, credential_enc, created_at) VALUES ($1, $2, $3, 'credential', $4)")
                .bind(user_id).bind(node_id).bind(format!("credential-hash-{index}" )).bind(timestamp)
                .execute(&pool).await.unwrap();
        }

        apply_sample(
            &pool,
            "node-one",
            "instance-one",
            &counters(user_id, 100, 50),
        )
        .await
        .unwrap();
        apply_sample(
            &pool,
            "node-two",
            "instance-two",
            &counters(user_id, 200, 75),
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            425
        );

        apply_sample(
            &pool,
            "node-one",
            "instance-one",
            &counters(user_id, 120, 60),
        )
        .await
        .unwrap();
        apply_sample(
            &pool,
            "node-two",
            "instance-two",
            &counters(user_id, 250, 90),
        )
        .await
        .unwrap();

        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            520
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM jobs WHERE kind = 'kick'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );

        apply_sample(
            &pool,
            "node-one",
            "instance-one",
            &counters(user_id, 120, 60),
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            520
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM jobs WHERE kind = 'kick'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
    }
}
