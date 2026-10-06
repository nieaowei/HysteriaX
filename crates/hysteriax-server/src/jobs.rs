use crate::db;
use std::time::Duration;

use crate::{
    deployment::{self, JobInput, JobOutput},
    security::{SecretBox, redact_config_secrets, redact_secret_values},
    ssh::SshError,
    state::AppState,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio::task::JoinSet;

const MAX_PARALLEL_NODES: usize = 4;
const MAX_ATTEMPTS: i64 = 3;

pub async fn run(state: AppState) {
    if let Err(error) = recover_interrupted(&state.pool).await {
        tracing::error!(%error, "failed to recover interrupted HysteriaX jobs");
    }

    let mut tasks = JoinSet::new();
    let mut reconciliation = tokio::time::interval(Duration::from_secs(10));
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
            _ = reconciliation.tick() => {
                if let Err(error) = reconcile_kicks(&state.pool).await {
                    tracing::error!(%error, "failed to reconcile durable kick requests");
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
}

async fn reconcile_kicks(pool: &PgPool) -> anyhow::Result<()> {
    let mut tx = db::begin_write(pool).await?;
    crate::kick_requests::schedule(&mut tx, None, false)
        .await
        .map_err(|e| anyhow::anyhow!(e.message))?;
    tx.commit().await?;
    Ok(())
}

async fn recover_interrupted(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut tx = db::begin_write(pool).await?;
    let rows = sqlx::query(
        "SELECT id, kind, node_id, attempts, payload_json FROM jobs WHERE status = 'running'",
    )
    .fetch_all(&mut *tx)
    .await?;
    for row in rows {
        let id: String = row.get("id");
        let kind: String = row.get("kind");
        let node_id: Option<String> = row.get("node_id");
        let timestamp = now();
        if kind == "kick" && row.get::<i64, _>("attempts") >= 5 {
            let job = JobInput {
                id: id.clone(),
                kind: kind.clone(),
                node_id: node_id.clone(),
                target_revision: None,
                payload: row.get("payload_json"),
                attempts: row.get("attempts"),
            };
            sqlx::query("UPDATE jobs SET status='failed',stage='needs_attention',error_message='Kick interrupted after exhausting its execution budget',finished_at=now(),updated_at=now() WHERE id=$1")
                .bind(&id).execute(&mut *tx).await?;
            crate::kick_requests::finish(&mut tx, &job, "needs_attention").await?;
            crate::kick_requests::event(&mut tx,&id,"job.failed",json!({"id":id,"kind":kind,"node_id":node_id,"status":"failed","stage":"needs_attention"})).await?;
            continue;
        }
        sqlx::query("UPDATE jobs SET status = 'queued', stage = 'recovered', available_at = $1, updated_at = $2, finished_at = NULL WHERE id = $3 AND status = 'running'")
            .bind(timestamp).bind(timestamp).bind(&id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ($1, 'job.recovered', $2, $3)")
            .bind(&id)
            .bind(json!({"id": id, "kind": kind, "node_id": node_id, "status": "queued", "stage": "recovered"}))
            .bind(timestamp)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

async fn claim_next(pool: &PgPool) -> Result<Option<JobInput>, sqlx::Error> {
    let timestamp = now();
    let mut tx = db::begin_write(pool).await?;
    let row = sqlx::query("UPDATE jobs SET status = 'running', stage = 'starting', attempts = attempts + 1, started_at = COALESCE(started_at, $1), updated_at = $2 WHERE id = (SELECT candidate.id FROM jobs AS candidate WHERE candidate.status = 'queued' AND candidate.available_at <= $3 AND (candidate.node_id IS NULL OR NOT EXISTS (SELECT 1 FROM jobs AS active WHERE active.node_id = candidate.node_id AND active.status = 'running')) AND (candidate.kind NOT IN ('deploy','sync','rollback') OR NOT EXISTS (SELECT 1 FROM jobs AS dependency WHERE dependency.node_id=candidate.node_id AND dependency.kind LIKE 'dns-%' AND dependency.status IN ('queued','running'))) AND (candidate.resource_key IS NULL OR NOT EXISTS (SELECT 1 FROM jobs AS resource_active WHERE resource_active.resource_key=candidate.resource_key AND resource_active.status='running')) ORDER BY candidate.available_at, candidate.created_at, candidate.id LIMIT 1) RETURNING id, kind, node_id, target_revision, payload_json, attempts")
        .bind(timestamp).bind(timestamp).bind(timestamp).fetch_optional(&mut *tx).await?;
    let job = if let Some(row) = row {
        let job = JobInput {
            id: row.get("id"),
            kind: row.get("kind"),
            node_id: row.get("node_id"),
            target_revision: row.get("target_revision"),
            payload: row.get::<Value, _>("payload_json"),
            attempts: row.get("attempts"),
        };
        let payload = json!({"id": job.id, "kind": job.kind, "node_id": job.node_id, "status": "running", "stage": "starting", "attempts": job.attempts});
        sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ($1, 'job.started', $2, $3)")
            .bind(&job.id).bind(payload).bind(timestamp).execute(&mut *tx).await?;
        Some(job)
    } else {
        None
    };
    tx.commit().await?;
    Ok(job)
}

async fn job_is_running(pool: &PgPool, id: &str) -> bool {
    match sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM jobs WHERE id=$1 AND status='running')",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    {
        Ok(running) => running,
        Err(error) => {
            tracing::error!(job_id=id,%error,"failed to check job cancellation");
            false
        }
    }
}

async fn wait_for_node_removal(
    state: &AppState,
    job: &JobInput,
    mut receiver: tokio::sync::broadcast::Receiver<String>,
) {
    loop {
        match receiver.recv().await {
            Ok(node) if job.node_id.as_deref() == Some(node.as_str()) => return,
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                if !job_is_running(&state.pool, &job.id).await {
                    return;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn execute_one(state: AppState, job: JobInput) {
    // Subscribe before the database check: a removal before subscription is
    // caught by persisted status, and a later removal is caught by the signal.
    let receiver = state.removed_nodes.subscribe();
    if !job_is_running(&state.pool, &job.id).await {
        return;
    }
    let result = tokio::select! {
        biased;
        _ = wait_for_node_removal(&state,&job,receiver) => return,
        result = deployment::run_job(&state.pool, &state.secrets, &job) => result,
    };
    match result {
        Ok(output) => {
            if let Err(error) = succeed(&state.pool, &job, output).await {
                tracing::error!(job_id = %job.id, %error, "failed to persist job result");
            }
        }
        Err(error) => {
            let message = safe_error(&state.pool, &state.secrets, &job, &error).await;
            let mut job = job;
            if let Some(delay) = crate::dns::provider::retry_delay(&error) {
                job.payload["dns_retry_after"] = json!(delay);
            }
            let retry = deployment::retryable(&error)
                && if job.kind == "kick" {
                    crate::kick_requests::retry_delay(job.attempts).is_some()
                } else {
                    job.attempts < MAX_ATTEMPTS
                };
            let waiting_recovery =
                job.kind == "kick" && crate::kick_requests::transport_error(&error);
            let rolled_back = was_rolled_back(&error);
            let rollback_failed = was_rollback_failed(&error);
            if let Err(persist_error) = fail(
                &state.pool,
                &job,
                &message,
                retry,
                rolled_back,
                rollback_failed,
                waiting_recovery,
            )
            .await
            {
                tracing::error!(job_id = %job.id, error = %persist_error, "failed to persist job failure");
            }
        }
    }
}

async fn succeed(pool: &PgPool, job: &JobInput, output: JobOutput) -> Result<(), sqlx::Error> {
    let timestamp = now();
    let result = json!({"stage": output.stage, "result": output.result});
    let mut tx = db::begin_write(pool).await?;
    let changed = sqlx::query("UPDATE jobs SET status = 'succeeded', stage = $1, result_json = $2, error_message = NULL, updated_at = $3, finished_at = $4 WHERE id = $5 AND status = 'running'")
        .bind(&output.stage).bind(result).bind(timestamp).bind(timestamp).bind(&job.id).execute(&mut *tx).await?;
    if changed.rows_affected() == 0 {
        tx.commit().await?;
        return Ok(());
    }
    crate::kick_requests::finish(
        &mut tx,
        job,
        if output.stage == "restriction_cleared" {
            "cancelled"
        } else {
            "completed"
        },
    )
    .await?;
    if !output.delete_node
        && let (Some(node_id), Some(state)) = (&job.node_id, &output.node_state)
    {
        if let (Some(revision), Some(config)) =
            (output.deployed_revision, output.deployed_config.as_deref())
        {
            sqlx::query("UPDATE nodes SET state = CASE WHEN state IN ('deleting', 'delete_failed') THEN state ELSE $1 END, deployed_revision = $2, deployed_config_enc = $3, deployed_content_sha256 = $4, last_seen_at = $5, updated_at = $6 WHERE id = $7")
                .bind(state).bind(revision).bind(config).bind(output.deployed_sha256.as_deref()).bind(timestamp).bind(timestamp).bind(node_id).execute(&mut *tx).await?;
            if let Some(connection) = &output.published_connection {
                sqlx::query("UPDATE nodes SET published_connection=$1 WHERE id=$2")
                    .bind(connection)
                    .bind(node_id)
                    .execute(&mut *tx)
                    .await?;
            }
            sqlx::query("UPDATE config_versions SET deployed_success = TRUE WHERE node_id = $1 AND revision = $2")
                .bind(node_id).bind(revision).execute(&mut *tx).await?;
        } else {
            sqlx::query("UPDATE nodes SET state = CASE WHEN state IN ('deleting', 'delete_failed') THEN state ELSE $1 END, updated_at = $2 WHERE id = $3")
                .bind(state)
                .bind(timestamp)
                .bind(node_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    let payload = json!({"id": job.id, "kind": job.kind, "node_id": job.node_id, "status": "succeeded", "stage": output.stage, "result": output.result});
    sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ($1, 'job.succeeded', $2, $3)")
        .bind(&job.id).bind(payload).bind(timestamp).execute(&mut *tx).await?;
    if output.delete_node
        && let Some(node_id) = &job.node_id
    {
        let pending = sqlx::query("UPDATE jobs SET status = 'cancelled', stage = 'node_removed', updated_at = $1, finished_at = $1 WHERE node_id = $2 AND id <> $3 AND status IN ('queued', 'running') RETURNING id, kind")
            .bind(timestamp)
            .bind(node_id)
            .bind(&job.id)
            .fetch_all(&mut *tx)
            .await?;
        for row in pending {
            let id: String = row.get("id");
            let kind: String = row.get("kind");
            let payload = json!({"id": id, "kind": kind, "node_id": node_id, "status": "cancelled", "stage": "node_removed"});
            sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ($1, 'job.cancelled', $2, $3)")
                .bind(&id)
                .bind(payload)
                .bind(timestamp)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', 'node.deleted', 'node', $2, $3, $4)")
                .bind(uuid::Uuid::new_v4().to_string()).bind(node_id).bind(json!({"job_id": job.id, "remote_uninstall": true})).bind(timestamp).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM nodes WHERE id = $1 AND state = 'deleting'")
            .bind(node_id)
            .execute(&mut *tx)
            .await?;
    }
    if job.kind != "kick"
        && !output.delete_node
        && output.stage != "fingerprint_confirmation_required"
        && let Some(node) = &job.node_id
    {
        crate::kick_requests::schedule(&mut tx, Some(node), true)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.message))?;
    }
    tx.commit().await?;
    Ok(())
}

async fn fail(
    pool: &PgPool,
    job: &JobInput,
    message: &str,
    retry: bool,
    rolled_back: bool,
    rollback_failed: bool,
    waiting_recovery: bool,
) -> Result<(), sqlx::Error> {
    let timestamp = now();
    let (status, stage, available_at, finished_at, event_name) = if retry {
        let delay = if job.kind == "kick" {
            crate::kick_requests::retry_delay(job.attempts).unwrap_or(300)
        } else if job.kind.starts_with("dns-") {
            job.payload["dns_retry_after"]
                .as_i64()
                .unwrap_or(30 * job.attempts)
                .max(30)
        } else {
            30
        };
        let later = now() + chrono::Duration::seconds(delay);
        ("queued", "retry_wait", later, None, "job.retrying")
    } else if job.kind == "kick" {
        (
            "failed",
            if waiting_recovery {
                "waiting_recovery"
            } else {
                "needs_attention"
            },
            timestamp,
            Some(timestamp),
            "job.failed",
        )
    } else if rolled_back {
        (
            "rolled_back",
            "rolled_back",
            timestamp,
            Some(timestamp),
            "job.rolled_back",
        )
    } else if rollback_failed {
        (
            "failed",
            "rollback_failed",
            timestamp,
            Some(timestamp),
            "job.failed",
        )
    } else {
        ("failed", "failed", timestamp, Some(timestamp), "job.failed")
    };
    let mut tx = db::begin_write(pool).await?;
    let changed = sqlx::query("UPDATE jobs SET status = $1, stage = $2, error_message = $3, available_at = $4, updated_at = $5, finished_at = $6 WHERE id = $7 AND status = 'running'")
        .bind(status).bind(stage).bind(message).bind(available_at).bind(timestamp).bind(finished_at).bind(&job.id).execute(&mut *tx).await?;
    if changed.rows_affected() == 0 {
        tx.commit().await?;
        return Ok(());
    }
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
        } else if !retry
            && job.kind != "ssh-test"
            && job.kind != "kick"
            && !job.kind.starts_with("dns-")
        {
            Some("sync_failed")
        } else {
            None
        };
        if let Some(state) = state {
            sqlx::query("UPDATE nodes SET state = CASE WHEN state = 'deleting' AND $1 != 'delete_failed' THEN state ELSE $2 END, updated_at = $3 WHERE id = $4")
                .bind(state)
                .bind(state)
                .bind(timestamp)
                .bind(node_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    if job.kind.starts_with("dns-record-") && job.kind != "dns-record-check" {
        sqlx::query("UPDATE dns_records SET state=$1,updated_at=now() WHERE id=(SELECT resource_id FROM dns_operations WHERE id=$2) AND desired IS NOT NULL")
            .bind(if retry { "pending" } else { "failed" }).bind(job.payload["dns_operation_id"].as_str()).execute(&mut *tx).await?;
    }
    if !retry {
        crate::kick_requests::finish(&mut tx, job, stage).await?;
    }
    let payload = json!({"id": job.id, "kind": job.kind, "node_id": job.node_id, "status": status, "stage": stage, "error": message, "attempts": job.attempts});
    sqlx::query(
        "INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(&job.id)
    .bind(event_name)
    .bind(payload)
    .bind(timestamp)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn safe_error(
    pool: &PgPool,
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
        && let Ok(Some(row)) = sqlx::query("SELECT node_token_enc, traffic_stats_secret_enc, desired_config_enc FROM nodes WHERE id = $1")
            .bind(node_id).fetch_optional(pool).await {
                for field in ["node_token_enc", "traffic_stats_secret_enc"] {
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
                        "SELECT config_enc FROM config_versions WHERE node_id = $1 AND revision = $2",
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
                    "SELECT credential_enc FROM node_assignments WHERE node_id = $1",
                )
                .bind(node_id)
                .fetch_all(pool)
                .await
                {
                    for assignment in assignments {
                        for field in ["credential_enc"] {
                            if let Ok(Some(encrypted)) = assignment.try_get::<Option<String>, _>(field)
                                && let Ok(secret) = secrets.decrypt(&encrypted)
                            {
                                secret_values.push(secret);
                            }
                        }
                    }
                }
    }
    if let Ok(versions) =
        sqlx::query_scalar::<_, String>("SELECT payload_enc FROM credential_versions")
            .fetch_all(pool)
            .await
    {
        for cipher in versions {
            if let Ok(plain) = secrets.decrypt(&cipher)
                && let Ok(payload) = serde_json::from_str::<Value>(&plain)
            {
                collect_credential_secrets(&payload, &mut secret_values);
            }
        }
    } else {
        return "Job failed; credential redaction could not be completed".into();
    }
    redact_secret_values(&mut message, secret_values);
    message
        .chars()
        .filter(|character| !character.is_control() || *character == '\n' || *character == '\t')
        .take(2_000)
        .collect()
}

fn collect_credential_secrets(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) if !s.is_empty() => out.push(s.clone()),
        Value::Object(map) => {
            for v in map.values() {
                collect_credential_secrets(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect_credential_secrets(v, out);
            }
        }
        _ => (),
    }
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

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::{PgPool, Row};

    use super::{JobInput, recover_interrupted, safe_error};
    use crate::security::SecretBox;

    async fn test_pool() -> PgPool {
        crate::db::test_pool().await
    }

    async fn insert_running_job(pool: &PgPool) {
        sqlx::query("INSERT INTO jobs (id, kind, status, stage, available_at, created_at, updated_at, started_at, attempts) VALUES ('job-running', 'sync', 'running', 'installing', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 2)")
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn node_removal_interrupts_only_its_worker_and_preserves_cancellation() {
        let state = crate::kick_requests::tests::fixture().await;
        let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
        let job = super::claim_next(&state.pool).await.unwrap().unwrap();
        let receiver = state.removed_nodes.subscribe();
        let wait = super::wait_for_node_removal(&state, &job, receiver);
        tokio::pin!(wait);
        state.removed_nodes.send("other-node".into()).unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut wait)
                .await
                .is_err()
        );
        crate::api::nodes::remove_record(
            axum::extract::State(state.clone()),
            axum::extract::Path("node".into()),
            axum::extract::Query(crate::api::nodes::RevisionQuery {
                expected_revision: Some(1),
            }),
        )
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut wait)
            .await
            .unwrap();
        assert!(!super::job_is_running(&state.pool, &id).await);
        // A stale result/failure cannot overwrite the terminal cancelled history.
        super::fail(&state.pool, &job, "stale timeout", true, false, false, true)
            .await
            .unwrap();
        let row: (String, String) = sqlx::query_as("SELECT status,stage FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(row, ("cancelled".into(), "node_removed".into()));
        assert!(super::claim_next(&state.pool).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn successful_kick_commits_result_and_completes_durable_request() {
        let state = crate::kick_requests::tests::fixture().await;
        let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
        let job = super::claim_next(&state.pool).await.unwrap().unwrap();
        let output = crate::deployment::JobOutput {
            stage: "clients_offline".into(),
            result: json!({"remaining_connections":0}),
            node_state: None,
            deployed_revision: None,
            deployed_config: None,
            deployed_sha256: None,
            published_connection: None,
            delete_node: false,
        };
        super::succeed(&state.pool, &job, output).await.unwrap();
        let result: (String, serde_json::Value) =
            sqlx::query_as("SELECT status,result_json FROM jobs WHERE id=$1")
                .bind(&id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(result.0, "succeeded");
        assert_eq!(result.1["result"]["remaining_connections"], 0);
        let request: String = sqlx::query_scalar("SELECT state FROM kick_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(request, "completed");
        super::reconcile_kicks(&state.pool).await.unwrap();
        assert!(super::claim_next(&state.pool).await.unwrap().is_none());
        let events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM job_events WHERE job_id=$1 AND event_type='job.succeeded'",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(events, 1);
    }

    #[tokio::test]
    async fn bounded_kicks_release_queue_and_keep_the_obligation() {
        let state = crate::kick_requests::tests::fixture().await;
        let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
        let mut job = super::claim_next(&state.pool).await.unwrap().unwrap();
        assert_eq!(job.id, id);
        super::fail(&state.pool, &job, "timeout", true, false, false, true)
            .await
            .unwrap();
        let seconds: i64 = sqlx::query_scalar("SELECT round(extract(epoch FROM available_at-updated_at))::bigint FROM jobs WHERE id=$1").bind(&id).fetch_one(&state.pool).await.unwrap();
        assert_eq!(seconds, 30);
        sqlx::query("UPDATE jobs SET status='running',attempts=5 WHERE id=$1")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();
        job.attempts = 5;
        super::fail(&state.pool, &job, "timeout", false, false, false, true)
            .await
            .unwrap();
        let status: (String, String) = sqlx::query_as("SELECT status,stage FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, ("failed".into(), "waiting_recovery".into()));
        let request: String = sqlx::query_scalar("SELECT state FROM kick_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(request, "waiting_recovery");
        let mut tx = crate::db::begin_write(&state.pool).await.unwrap();
        let ssh = crate::api::enqueue_job_in_tx(&mut tx, "ssh-test", Some("node"), None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            super::claim_next(&state.pool).await.unwrap().unwrap().id,
            ssh
        );
    }

    #[tokio::test]
    async fn fifth_interrupted_kick_is_not_executed_again_after_restart() {
        let state = crate::kick_requests::tests::fixture().await;
        let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
        sqlx::query("UPDATE jobs SET status='running',attempts=5 WHERE id=$1")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();
        super::recover_interrupted(&state.pool).await.unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "failed");
        assert!(super::claim_next(&state.pool).await.unwrap().is_none());
        let state: String = sqlx::query_scalar("SELECT state FROM kick_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(state, "needs_attention");
    }

    #[tokio::test]
    async fn failed_event_persistence_does_not_acknowledge_or_suspend_request() {
        let state = crate::kick_requests::tests::fixture().await;
        let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
        let job = super::claim_next(&state.pool).await.unwrap().unwrap();
        sqlx::raw_sql("CREATE FUNCTION reject_kick_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.failed' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_kick_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_kick_event();")
            .execute(&state.pool).await.unwrap();
        assert!(
            super::fail(&state.pool, &job, "timeout", false, false, false, true)
                .await
                .is_err()
        );
        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "running");
        let request: String = sqlx::query_scalar("SELECT state FROM kick_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(request, "active");
    }

    #[tokio::test]
    async fn fair_queue_orders_by_availability_instead_of_old_creation_time() {
        let state = crate::kick_requests::tests::fixture().await;
        let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
        let mut tx = crate::db::begin_write(&state.pool).await.unwrap();
        let ssh = crate::api::enqueue_job_in_tx(&mut tx, "ssh-test", Some("node"), None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        sqlx::query("UPDATE jobs SET available_at=now()-interval '1 second' WHERE id=$1")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE jobs SET available_at=now()-interval '2 seconds' WHERE id=$1")
            .bind(&ssh)
            .execute(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            super::claim_next(&state.pool).await.unwrap().unwrap().id,
            ssh
        );
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
        assert_eq!(
            row.get::<chrono::DateTime<chrono::Utc>, _>("started_at"),
            chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        );
        assert!(
            row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("finished_at")
                .is_none()
        );
        assert_ne!(
            row.get::<chrono::DateTime<chrono::Utc>, _>("available_at"),
            chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        );

        let event: (String, serde_json::Value) = sqlx::query_as(
            "SELECT event_type, payload_json FROM job_events WHERE job_id = 'job-running'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(event.0, "job.recovered");
        let payload = event.1;
        assert_eq!(payload["id"], "job-running");
        assert_eq!(payload["status"], "queued");
        assert_eq!(payload["stage"], "recovered");
    }

    #[tokio::test]
    async fn failed_recovery_event_keeps_the_job_running() {
        let pool = test_pool().await;
        insert_running_job(&pool).await;
        sqlx::query("CREATE FUNCTION reject_recovery_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type = 'job.recovered' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER reject_recovery_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_recovery_event()")
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
        sqlx::query("INSERT INTO nodes (id, name, ssh_host, ssh_port, ssh_username, ssh_auth_type, ssh_secret_enc, ssh_passphrase_enc, public_host, public_port, listen_addr, node_token_hash, node_token_enc, traffic_stats_secret_enc, desired_config_enc, created_at, updated_at) VALUES ('redaction-node', 'Node', 'node.example.test', 22, 'root', 'password', $1, $2, 'node.example.test', 443, ':443', 'node-hash', $3, $4, $5, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
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
        sqlx::query("INSERT INTO node_assignments (user_id, node_id, credential_hash, credential_enc, client_certificate_enc, client_private_key_enc, created_at) VALUES ('redaction-user', 'redaction-node', 'credential-hash', $1, $2, $3, '2026-01-01T00:00:00Z')")
            .bind(encrypted(values[9]))
            .bind(encrypted(values[10]))
            .bind(encrypted(values[11]))
            .execute(&pool)
            .await
            .unwrap();

        let mut tx = crate::db::begin_write(&pool).await.unwrap();
        crate::credentials::insert(
            &mut tx,
            &secrets,
            "SSH redaction fixture",
            "ssh_private_key",
            None,
            &json!({"secret":values[0],"passphrase":values[1]}),
            &json!({}),
            &json!({}),
        )
        .await
        .unwrap();
        crate::credentials::insert(
            &mut tx,
            &secrets,
            "mTLS redaction fixture",
            "tls_identity",
            Some("redaction-user"),
            &json!({"certificate":values[10],"private_key":values[11]}),
            &json!({}),
            &json!({}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

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

#[cfg(test)]
mod dns_job_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn dns_jobs_serialize_by_zone_and_deployments_wait_for_dns_dependencies() {
        let (state, ssh, connection, zone) = crate::dns::tests::fixture().await;
        let node = crate::dns::tests::node(&state, &ssh).await;
        let mut tx = crate::db::begin_write(&state.pool).await.unwrap();
        let deploy = crate::api::enqueue_job_in_tx(&mut tx, "deploy", Some(&node), Some(1))
            .await
            .unwrap();
        let mut jobs = Vec::new();
        for index in 0..2 {
            let key = format!("zone-job:{index}");
            let receipt = crate::dns::enqueue(
                &mut tx,
                crate::dns::Operation {
                    key: &key,
                    request: &json!({"index":index}),
                    connection: &connection,
                    version: 1,
                    resource_type: "dns_zone",
                    resource: &zone,
                    resource_name: "example.test",
                    resource_key: format!("dns-zone:{zone}"),
                    action: "zone-refresh",
                    payload: json!({}),
                    node: Some(&node),
                },
            )
            .await
            .unwrap();
            jobs.push(receipt["job_id"].as_str().unwrap().to_owned());
        }
        tx.commit().await.unwrap();
        let first = claim_next(&state.pool).await.unwrap().unwrap();
        assert_eq!(first.id, jobs[0]);
        assert!(claim_next(&state.pool).await.unwrap().is_none());
        sqlx::query("UPDATE jobs SET status='succeeded' WHERE id=$1")
            .bind(&first.id)
            .execute(&state.pool)
            .await
            .unwrap();
        let second = claim_next(&state.pool).await.unwrap().unwrap();
        assert_eq!(second.id, jobs[1]);
        sqlx::query("UPDATE jobs SET status='succeeded' WHERE id=$1")
            .bind(&second.id)
            .execute(&state.pool)
            .await
            .unwrap();
        assert_eq!(claim_next(&state.pool).await.unwrap().unwrap().id, deploy);
    }

    #[tokio::test]
    async fn successful_deployment_publishes_executed_connection_and_cancelled_result_cannot_publish()
     {
        let (state, ssh, _, _) = crate::dns::tests::fixture().await;
        let node = crate::dns::tests::node(&state, &ssh).await;
        let mut tx = crate::db::begin_write(&state.pool).await.unwrap();
        crate::api::enqueue_job_in_tx(&mut tx, "deploy", Some(&node), Some(1))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let job = claim_next(&state.pool).await.unwrap().unwrap();
        let connection = json!({"public_host":"old.example.test","public_port":443,"listen_addr":":443","tls_sni":null,"tls_skip_verify":false});
        sqlx::query("UPDATE nodes SET public_host='concurrent.example.test' WHERE id=$1")
            .bind(&node)
            .execute(&state.pool)
            .await
            .unwrap();
        let output = || JobOutput {
            stage: "health_checked".into(),
            result: json!({}),
            node_state: Some("deployed".into()),
            deployed_revision: Some(1),
            deployed_config: Some(state.secrets.encrypt("{}").unwrap()),
            deployed_sha256: Some("digest".into()),
            published_connection: Some(connection.clone()),
            delete_node: false,
        };
        succeed(&state.pool, &job, output()).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, Option<serde_json::Value>>(
                "SELECT published_connection FROM nodes WHERE id=$1"
            )
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
            Some(connection.clone())
        );
        let mut stale = output();
        stale.published_connection = Some(json!({"public_host":"stale.example.test"}));
        succeed(&state.pool, &job, stale).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, Option<serde_json::Value>>(
                "SELECT published_connection FROM nodes WHERE id=$1"
            )
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
            Some(connection)
        );
    }
}
