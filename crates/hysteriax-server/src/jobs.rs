use std::time::Duration;

use crate::{
    deployment::{self, JobInput, JobOutput},
    security::{SecretBox, redact_config_secrets, redact_secret_values},
    ssh::SshError,
    state::AppState,
};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};
use tokio::task::JoinSet;

const MAX_PARALLEL_NODES: usize = 4;
const MAX_ATTEMPTS: i64 = 3;

pub async fn run(state: AppState) {
    if let Err(error) = recover_interrupted(&state.pool).await {
        tracing::error!(%error, "failed to recover interrupted HysteriaX jobs");
    }

    let mut tasks = JoinSet::new();
    loop {
        while tasks.len() < MAX_PARALLEL_NODES {
            match claim_next(&state.pool).await {
                Ok(Some(job)) => {
                    let worker_state = state.clone();
                    tasks.spawn(async move { execute_one(worker_state, job).await });
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::error!(%error, "failed to claim background job");
                    break;
                }
            }
        }

        tokio::select! {
            joined = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(error)) = joined {
                    tracing::error!(%error, "background job task panicked");
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}

async fn recover_interrupted(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let rows = sqlx::query("SELECT id, kind, node_id FROM jobs WHERE status = 'running'")
        .fetch_all(&mut *tx)
        .await?;
    for row in rows {
        let id: String = row.get("id");
        let kind: String = row.get("kind");
        let node_id: Option<String> = row.get("node_id");
        let timestamp = now();
        sqlx::query("UPDATE jobs SET status = 'queued', stage = 'recovered', available_at = ?, updated_at = ?, finished_at = NULL WHERE id = ? AND status = 'running'")
            .bind(&timestamp).bind(&timestamp).bind(&id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES (?, 'job.recovered', ?, ?)")
            .bind(&id)
            .bind(json!({"id": id, "kind": kind, "node_id": node_id, "status": "queued", "stage": "recovered"}).to_string())
            .bind(&timestamp)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

async fn claim_next(pool: &SqlitePool) -> Result<Option<JobInput>, sqlx::Error> {
    let timestamp = now();
    let mut tx = pool.begin().await?;
    let row = sqlx::query("UPDATE jobs SET status = 'running', stage = 'starting', attempts = attempts + 1, started_at = COALESCE(started_at, ?), updated_at = ? WHERE id = (SELECT candidate.id FROM jobs AS candidate WHERE candidate.status = 'queued' AND candidate.available_at <= ? AND (candidate.node_id IS NULL OR NOT EXISTS (SELECT 1 FROM jobs AS active WHERE active.node_id = candidate.node_id AND active.status = 'running')) ORDER BY candidate.created_at LIMIT 1) RETURNING id, kind, node_id, target_revision, payload_json, attempts")
        .bind(&timestamp).bind(&timestamp).bind(&timestamp).fetch_optional(&mut *tx).await?;
    let job = if let Some(row) = row {
        let job = JobInput {
            id: row.get("id"),
            kind: row.get("kind"),
            node_id: row.get("node_id"),
            target_revision: row.get("target_revision"),
            payload: serde_json::from_str::<Value>(&row.get::<String, _>("payload_json"))
                .unwrap_or_else(|_| json!({})),
            attempts: row.get("attempts"),
        };
        let payload = json!({"id": job.id, "kind": job.kind, "node_id": job.node_id, "status": "running", "stage": "starting", "attempts": job.attempts});
        sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES (?, 'job.started', ?, ?)")
            .bind(&job.id).bind(payload.to_string()).bind(&timestamp).execute(&mut *tx).await?;
        Some(job)
    } else {
        None
    };
    tx.commit().await?;
    Ok(job)
}

async fn execute_one(state: AppState, job: JobInput) {
    let result = deployment::run_job(&state.pool, &state.secrets, &job).await;
    match result {
        Ok(output) => {
            if let Err(error) = succeed(&state.pool, &job, output).await {
                tracing::error!(job_id = %job.id, %error, "failed to persist job result");
            }
        }
        Err(error) => {
            let message = safe_error(&state.pool, &state.secrets, &job, &error).await;
            let retry = deployment::retryable(&error)
                && (job.kind == "kick" || job.attempts < MAX_ATTEMPTS);
            let rolled_back = was_rolled_back(&error);
            let rollback_failed = was_rollback_failed(&error);
            if let Err(persist_error) = fail(
                &state.pool,
                &job,
                &message,
                retry,
                rolled_back,
                rollback_failed,
            )
            .await
            {
                tracing::error!(job_id = %job.id, error = %persist_error, "failed to persist job failure");
            }
        }
    }
}

async fn succeed(pool: &SqlitePool, job: &JobInput, output: JobOutput) -> Result<(), sqlx::Error> {
    let timestamp = now();
    let result = json!({"stage": output.stage, "result": output.result});
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE jobs SET status = 'succeeded', stage = ?, result_json = ?, error_message = NULL, updated_at = ?, finished_at = ? WHERE id = ? AND status = 'running'")
        .bind(&output.stage).bind(result.to_string()).bind(&timestamp).bind(&timestamp).bind(&job.id).execute(&mut *tx).await?;
    if !output.delete_node
        && let (Some(node_id), Some(state)) = (&job.node_id, &output.node_state)
    {
        if let (Some(revision), Some(config)) =
            (output.deployed_revision, output.deployed_config.as_deref())
        {
            sqlx::query("UPDATE nodes SET state = CASE WHEN state IN ('deleting', 'delete_failed') THEN state ELSE ? END, deployed_revision = ?, deployed_config_enc = ?, deployed_content_sha256 = ?, last_seen_at = ?, updated_at = ? WHERE id = ?")
                .bind(state).bind(revision).bind(config).bind(output.deployed_sha256.as_deref()).bind(&timestamp).bind(&timestamp).bind(node_id).execute(&mut *tx).await?;
            sqlx::query("UPDATE config_versions SET deployed_success = 1 WHERE node_id = ? AND revision = ?")
                .bind(node_id).bind(revision).execute(&mut *tx).await?;
        } else {
            sqlx::query("UPDATE nodes SET state = CASE WHEN state IN ('deleting', 'delete_failed') THEN state ELSE ? END, updated_at = ? WHERE id = ?")
                .bind(state)
                .bind(&timestamp)
                .bind(node_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    let payload = json!({"id": job.id, "kind": job.kind, "node_id": job.node_id, "status": "succeeded", "stage": output.stage, "result": output.result});
    sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES (?, 'job.succeeded', ?, ?)")
        .bind(&job.id).bind(payload.to_string()).bind(&timestamp).execute(&mut *tx).await?;
    if output.delete_node
        && let Some(node_id) = &job.node_id
    {
        sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES (?, 'admin', 'node.deleted', 'node', ?, ?, ?)")
                .bind(uuid::Uuid::new_v4().to_string()).bind(node_id).bind(json!({"job_id": job.id, "remote_uninstall": true}).to_string()).bind(&timestamp).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM nodes WHERE id = ? AND state = 'deleting'")
            .bind(node_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn fail(
    pool: &SqlitePool,
    job: &JobInput,
    message: &str,
    retry: bool,
    rolled_back: bool,
    rollback_failed: bool,
) -> Result<(), sqlx::Error> {
    let timestamp = now();
    let (status, stage, available_at, finished_at, event_name) = if retry {
        let later = (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc3339();
        ("queued", "retry_wait", later, None, "job.retrying")
    } else if rolled_back {
        (
            "rolled_back",
            "rolled_back",
            timestamp.clone(),
            Some(timestamp.clone()),
            "job.rolled_back",
        )
    } else if rollback_failed {
        (
            "failed",
            "rollback_failed",
            timestamp.clone(),
            Some(timestamp.clone()),
            "job.failed",
        )
    } else {
        (
            "failed",
            "failed",
            timestamp.clone(),
            Some(timestamp.clone()),
            "job.failed",
        )
    };
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE jobs SET status = ?, stage = ?, error_message = ?, available_at = ?, updated_at = ?, finished_at = ? WHERE id = ? AND status = 'running'")
        .bind(status).bind(stage).bind(message).bind(available_at).bind(&timestamp).bind(finished_at).bind(&job.id).execute(&mut *tx).await?;
    if let Some(node_id) = &job.node_id {
        let state = if rolled_back {
            Some("rolled_back")
        } else if rollback_failed {
            Some("rollback_failed")
        } else if job.kind == "uninstall" {
            Some("delete_failed")
        } else if message.contains("confirm this SSH host fingerprint") {
            Some("needs_fingerprint")
        } else if message.contains("SSH host key changed") {
            Some("fingerprint_changed")
        } else if message.contains("changed outside HysteriaX") {
            Some("drift")
        } else if !retry && job.kind != "ssh-test" {
            Some("sync_failed")
        } else {
            None
        };
        if let Some(state) = state {
            sqlx::query("UPDATE nodes SET state = CASE WHEN state = 'deleting' AND ? != 'delete_failed' THEN state ELSE ? END, updated_at = ? WHERE id = ?")
                .bind(state)
                .bind(state)
                .bind(&timestamp)
                .bind(node_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    let payload = json!({"id": job.id, "kind": job.kind, "node_id": job.node_id, "status": status, "stage": stage, "error": message, "attempts": job.attempts});
    sqlx::query(
        "INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&job.id)
    .bind(event_name)
    .bind(payload.to_string())
    .bind(&timestamp)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn safe_error(
    pool: &SqlitePool,
    secrets: &SecretBox,
    job: &JobInput,
    error: &anyhow::Error,
) -> String {
    let mut message = error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    let mut secret_values = Vec::new();
    if let Some(node_id) = &job.node_id
        && let Ok(Some(row)) = sqlx::query("SELECT ssh_secret_enc, ssh_passphrase_enc, node_token_enc, traffic_stats_secret_enc, desired_config_enc FROM nodes WHERE id = ?")
            .bind(node_id).fetch_optional(pool).await {
                for field in ["ssh_secret_enc", "ssh_passphrase_enc", "node_token_enc", "traffic_stats_secret_enc"] {
                    if let Ok(Some(encrypted)) = row.try_get::<Option<String>, _>(field)
                        && let Ok(secret) = secrets.decrypt(&encrypted)
                    {
                        secret_values.push(secret);
                    }
                }
                if let Ok(config_json) = secrets.decrypt(&row.get::<String, _>("desired_config_enc"))
                    && let Ok(config) = serde_json::from_str::<Value>(&config_json)
                {
                    redact_config_secrets(&mut message, &config);
                }
                if let Some(revision) = job.target_revision
                    && let Ok(Some(config_enc)) = sqlx::query_scalar::<_, String>(
                        "SELECT config_enc FROM config_versions WHERE node_id = ? AND revision = ?",
                    )
                    .bind(node_id)
                    .bind(revision)
                    .fetch_optional(pool)
                    .await
                    && let Ok(snapshot) = secrets.decrypt(&config_enc)
                    && let Ok(snapshot) = serde_json::from_str::<Value>(&snapshot)
                {
                    redact_config_secrets(&mut message, &snapshot);
                }
                if let Ok(assignments) = sqlx::query(
                    "SELECT credential_enc, client_certificate_enc, client_private_key_enc FROM node_assignments WHERE node_id = ?",
                )
                .bind(node_id)
                .fetch_all(pool)
                .await
                {
                    for assignment in assignments {
                        for field in ["credential_enc", "client_certificate_enc", "client_private_key_enc"] {
                            if let Ok(Some(encrypted)) = assignment.try_get::<Option<String>, _>(field)
                                && let Ok(secret) = secrets.decrypt(&encrypted)
                            {
                                secret_values.push(secret);
                            }
                        }
                    }
                }
    }
    redact_secret_values(&mut message, secret_values);
    message
        .chars()
        .filter(|character| !character.is_control() || *character == '\n' || *character == '\t')
        .take(2_000)
        .collect()
}

fn was_rolled_back(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<SshError>()
            .is_some_and(|error| matches!(error, SshError::Command { code: 53, .. }))
    })
}

fn was_rollback_failed(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<SshError>()
            .is_some_and(|error| matches!(error, SshError::Command { code: 54, .. }))
    })
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};

    use super::{JobInput, recover_interrupted, safe_error};
    use crate::security::SecretBox;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn insert_running_job(pool: &SqlitePool) {
        sqlx::query("INSERT INTO jobs (id, kind, status, stage, available_at, created_at, updated_at, started_at, attempts) VALUES ('job-running', 'sync', 'running', 'installing', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 2)")
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn interrupted_jobs_are_requeued_with_a_recovery_event() {
        let pool = test_pool().await;
        insert_running_job(&pool).await;

        recover_interrupted(&pool).await.unwrap();

        let row = sqlx::query("SELECT status, stage, attempts, started_at, finished_at, available_at FROM jobs WHERE id = 'job-running'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("status"), "queued");
        assert_eq!(row.get::<String, _>("stage"), "recovered");
        assert_eq!(row.get::<i64, _>("attempts"), 2);
        assert_eq!(row.get::<String, _>("started_at"), "2026-01-01T00:00:00Z");
        assert!(row.get::<Option<String>, _>("finished_at").is_none());
        assert_ne!(row.get::<String, _>("available_at"), "2026-01-01T00:00:00Z");

        let event: (String, String) = sqlx::query_as(
            "SELECT event_type, payload_json FROM job_events WHERE job_id = 'job-running'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event.0, "job.recovered");
        let payload: serde_json::Value = serde_json::from_str(&event.1).unwrap();
        assert_eq!(payload["id"], "job-running");
        assert_eq!(payload["status"], "queued");
        assert_eq!(payload["stage"], "recovered");
    }

    #[tokio::test]
    async fn failed_recovery_event_keeps_the_job_running() {
        let pool = test_pool().await;
        insert_running_job(&pool).await;
        sqlx::query("CREATE TRIGGER reject_recovery_event BEFORE INSERT ON job_events WHEN NEW.event_type = 'job.recovered' BEGIN SELECT RAISE(ABORT, 'injected failure'); END")
            .execute(&pool)
            .await
            .unwrap();

        assert!(recover_interrupted(&pool).await.is_err());

        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id = 'job-running'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        let events: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM job_events WHERE job_id = 'job-running'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(events, 0);
    }

    #[tokio::test]
    async fn job_errors_redact_saved_config_and_assigned_user_secrets() {
        let pool = test_pool().await;
        let secrets =
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let values = [
            "ssh-password",
            "ssh-passphrase",
            "node-token",
            "stats-secret",
            "obfs-secret",
            "realm-token",
            "socks-user",
            "socks-password",
            "provider-token",
            "hy2-user-password",
            "client-certificate",
            "client-private-key",
        ];
        let config = json!({
            "server_config": {
                "obfs": {"gecko": {"password": values[4]}},
                "realm": {"connection": {"token": values[5]}},
                "outbounds": [{"socks5": {"username": values[6], "password": values[7]}}],
                "acme": {"dns": {"config": {"api_token": values[8]}}}
            }
        });
        let encrypted = |value: &str| secrets.encrypt(value).unwrap();
        sqlx::query("INSERT INTO nodes (id, name, ssh_host, ssh_port, ssh_username, ssh_auth_type, ssh_secret_enc, ssh_passphrase_enc, public_host, public_port, listen_addr, node_token_hash, node_token_enc, traffic_stats_secret_enc, desired_config_enc, created_at, updated_at) VALUES ('redaction-node', 'Node', 'node.example.test', 22, 'root', 'password', ?, ?, 'node.example.test', 443, ':443', 'node-hash', ?, ?, ?, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
            .bind(encrypted(values[0]))
            .bind(encrypted(values[1]))
            .bind(encrypted(values[2]))
            .bind(encrypted(values[3]))
            .bind(encrypted(&config.to_string()))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO users (id, name, created_at, updated_at) VALUES ('redaction-user', 'User', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO node_assignments (user_id, node_id, credential_hash, credential_enc, client_certificate_enc, client_private_key_enc, created_at) VALUES ('redaction-user', 'redaction-node', 'credential-hash', ?, ?, ?, '2026-01-01T00:00:00Z')")
            .bind(encrypted(values[9]))
            .bind(encrypted(values[10]))
            .bind(encrypted(values[11]))
            .execute(&pool)
            .await
            .unwrap();

        let job = JobInput {
            id: "redaction-job".to_owned(),
            kind: "sync".to_owned(),
            node_id: Some("redaction-node".to_owned()),
            target_revision: None,
            payload: json!({}),
            attempts: 1,
        };
        let error_message = values.join(" | ");
        let error = anyhow::anyhow!(error_message);

        let safe = safe_error(&pool, &secrets, &job, &error).await;

        for value in values {
            assert!(!safe.contains(value), "job error leaked a secret: {value}");
        }
        assert!(safe.contains("[redacted]"));
    }
}
