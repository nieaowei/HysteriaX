use crate::db;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    api::{enqueue_job_with_payload_in_tx, now},
    deployment,
    security::SecretBox,
    ssh::{self, FingerprintResult},
    state::AppState,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CounterDelta {
    pub tx: u64,
    pub rx: u64,
    pub gap_reason: Option<&'static str>,
}

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
    loop {
        if let Err(error) = collect_all(&state.pool, &state.secrets).await {
            tracing::error!(%error, "traffic collection pass failed");
        }
        if let Err(error) = schedule_expirations(&state.pool).await {
            tracing::error!(%error, "failed to schedule expired-user revocations");
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

async fn collect_all(pool: &PgPool, secrets: &SecretBox) -> Result<()> {
    let rows = sqlx::query("SELECT id FROM nodes WHERE deployed_revision IS NOT NULL AND state NOT IN ('deleting', 'delete_failed') ORDER BY id")
        .fetch_all(pool)
        .await?;
    for row in rows {
        let node_id: String = row.get("id");
        if let Err(error) = sample_node(pool, secrets, &node_id).await {
            record_gap(pool, &node_id, classify_sample_error(&error)).await?;
            sqlx::query("UPDATE nodes SET state = CASE WHEN state IN ('deployed', 'syncing') THEN 'unreachable' ELSE state END, updated_at = $1 WHERE id = $2")
                .bind(now()).bind(&node_id).execute(pool).await?;
        }
    }
    Ok(())
}

async fn sample_node(pool: &PgPool, secrets: &SecretBox, node_id: &str) -> Result<()> {
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
    let before_instance = systemd_instance(&session).await?;
    let stats_secret_enc: String =
        sqlx::query_scalar("SELECT traffic_stats_secret_enc FROM nodes WHERE id = $1")
            .bind(node_id)
            .fetch_one(pool)
            .await?;
    let stats_secret = secrets.decrypt(&stats_secret_enc)?;
    let traffic_stats_port =
        deployment::load_deployed_traffic_stats_port(pool, secrets, node_id).await?;
    let body = session
        .loopback_http_get(u32::from(traffic_stats_port), "/traffic", &stats_secret)
        .await?;
    let traffic: Value =
        serde_json::from_slice(&body).context("Hysteria traffic API returned invalid JSON")?;
    let after_instance = systemd_instance(&session).await?;
    if before_instance != after_instance {
        record_gap(pool, node_id, "hysteria_restarted_during_sample").await?;
        return Ok(());
    }
    let counters = traffic
        .as_object()
        .context("Hysteria traffic API returned an invalid response shape")?;
    apply_sample(pool, node_id, &before_instance, counters).await?;
    let sampled_at = now();
    sqlx::query("UPDATE nodes SET state = CASE WHEN state = 'unreachable' THEN 'deployed' ELSE state END, last_seen_at = $1, last_sample_at = $2, updated_at = $3 WHERE id = $4")
        .bind(sampled_at).bind(sampled_at).bind(sampled_at).bind(node_id).execute(pool).await?;
    sqlx::query("UPDATE data_gaps SET resolved_at = $1 WHERE node_id = $2 AND resolved_at IS NULL")
        .bind(now())
        .bind(node_id)
        .execute(pool)
        .await?;
    Ok(())
}

async fn systemd_instance(session: &ssh::SshSession) -> Result<String> {
    let direct = session
        .execute("systemctl show hysteriax.service --property=InvocationID --value")
        .await?;
    let value = if direct.code == 0 {
        direct.stdout
    } else {
        session
            .execute_checked(
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

async fn apply_sample(
    pool: &PgPool,
    node_id: &str,
    instance_id: &str,
    counters: &Map<String, Value>,
) -> Result<()> {
    let counters = parse_counters(counters)?;
    let timestamp = now();
    let mut tx = db::begin_write(pool).await?;
    for (user_id, tx_total, rx_total) in counters {
        let tx_total_i64 = tx_total as i64;
        let rx_total_i64 = rx_total as i64;
        let user = sqlx::query("SELECT u.usage_bytes, u.quota_bytes, u.enabled, u.expires_at FROM users u JOIN node_assignments a ON a.user_id = u.id WHERE u.id = $1 AND a.node_id = $2")
            .bind(&user_id).bind(node_id).fetch_optional(&mut *tx).await?;
        let Some(user) = user else { continue };
        let baseline = sqlx::query("SELECT instance_id, tx_total, rx_total FROM traffic_baselines WHERE node_id = $1 AND user_id = $2")
            .bind(node_id).bind(&user_id).fetch_optional(&mut *tx).await?;
        let previous = baseline.as_ref().map(|row| {
            (
                row.get::<String, _>("instance_id"),
                row.get::<i64, _>("tx_total") as u64,
                row.get::<i64, _>("rx_total") as u64,
            )
        });
        let delta = counter_delta(
            previous
                .as_ref()
                .map(|(instance, tx, rx)| (instance.as_str(), *tx, *rx)),
            instance_id,
            tx_total,
            rx_total,
        );
        let delta_tx = i64::try_from(delta.tx).unwrap_or(i64::MAX);
        let delta_rx = i64::try_from(delta.rx).unwrap_or(i64::MAX);
        let delta_bytes = delta_tx.saturating_add(delta_rx);
        let old_usage: i64 = user.get("usage_bytes");
        let new_usage = old_usage.saturating_add(delta_bytes);
        sqlx::query("INSERT INTO traffic_baselines (node_id, user_id, instance_id, tx_total, rx_total, sampled_at) VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT(node_id, user_id) DO UPDATE SET instance_id = excluded.instance_id, tx_total = excluded.tx_total, rx_total = excluded.rx_total, sampled_at = excluded.sampled_at")
            .bind(node_id).bind(&user_id).bind(instance_id).bind(tx_total_i64).bind(rx_total_i64).bind(timestamp).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO traffic_records (id, node_id, user_id, instance_id, baseline_tx, baseline_rx, delta_tx, delta_rx, gap_reason, sampled_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)")
            .bind(Uuid::new_v4().to_string()).bind(node_id).bind(&user_id).bind(instance_id).bind(tx_total_i64).bind(rx_total_i64).bind(delta_tx).bind(delta_rx).bind(delta.gap_reason).bind(timestamp).execute(&mut *tx).await?;
        sqlx::query("UPDATE users SET usage_bytes = $1, updated_at = $2 WHERE id = $3")
            .bind(new_usage)
            .bind(timestamp)
            .bind(&user_id)
            .execute(&mut *tx)
            .await?;

        let enabled: bool = user.get("enabled");
        let expires_at: Option<chrono::DateTime<chrono::Utc>> = user.get("expires_at");
        let quota: Option<i64> = user.get("quota_bytes");
        let expired = expires_at.is_some_and(|expiration| expiration <= chrono::Utc::now());
        let restricted = !enabled || expired || quota.is_some_and(|limit| new_usage >= limit);
        if restricted {
            let marked: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM users WHERE id = $1 AND access_kick_enqueued_at IS NOT NULL",
            )
            .bind(&user_id)
            .fetch_one(&mut *tx)
            .await?;
            if marked == 0 {
                let assignments = sqlx::query_scalar::<_, String>(
                    "SELECT node_id FROM node_assignments WHERE user_id = $1",
                )
                .bind(&user_id)
                .fetch_all(&mut *tx)
                .await?;
                sqlx::query(
                    "UPDATE users SET access_kick_enqueued_at = $1, updated_at = $2 WHERE id = $3",
                )
                .bind(timestamp)
                .bind(timestamp)
                .bind(&user_id)
                .execute(&mut *tx)
                .await?;
                for assignment_node in assignments {
                    enqueue_job_with_payload_in_tx(
                        &mut tx,
                        "kick",
                        Some(&assignment_node),
                        None,
                        json!({"user_id": user_id}),
                    )
                    .await
                    .map_err(|error| anyhow::anyhow!(error.message))?;
                }
            }
        }
        if let Some(reason) = delta.gap_reason {
            record_gap_in_tx(&mut tx, node_id, reason, &timestamp).await?;
        }
    }
    tx.commit().await?;
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
    let timestamp = now();
    let rows = sqlx::query("SELECT id FROM users WHERE enabled = FALSE OR (expires_at IS NOT NULL AND expires_at <= $1) OR (quota_bytes IS NOT NULL AND usage_bytes >= quota_bytes)")
        .bind(timestamp).fetch_all(pool).await?;
    for row in rows {
        let user_id: String = row.get("id");
        let mut tx = db::begin_write(pool).await?;
        let needs_kick: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE id = $1 AND access_kick_enqueued_at IS NULL",
        )
        .bind(&user_id)
        .fetch_one(&mut *tx)
        .await?;
        if needs_kick > 0 {
            let node_ids = sqlx::query_scalar::<_, String>(
                "SELECT node_id FROM node_assignments WHERE user_id = $1",
            )
            .bind(&user_id)
            .fetch_all(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE users SET access_kick_enqueued_at = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(timestamp)
            .bind(timestamp)
            .bind(&user_id)
            .execute(&mut *tx)
            .await?;
            for node_id in node_ids {
                enqueue_job_with_payload_in_tx(
                    &mut tx,
                    "kick",
                    Some(&node_id),
                    None,
                    json!({"user_id": user_id}),
                )
                .await
                .map_err(|error| anyhow::anyhow!(error.message))?;
            }
        }
        tx.commit().await?;
    }
    Ok(())
}

async fn record_gap(pool: &PgPool, node_id: &str, reason: &str) -> Result<()> {
    let timestamp = now();
    let mut tx = db::begin_write(pool).await?;
    record_gap_in_tx(&mut tx, node_id, reason, &timestamp).await?;
    tx.commit().await?;
    Ok(())
}

async fn record_gap_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    node_id: &str,
    reason: &str,
    timestamp: &chrono::DateTime<chrono::Utc>,
) -> Result<()> {
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
