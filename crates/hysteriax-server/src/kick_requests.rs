//! Durable revocation obligations, separate from bounded execution jobs.
use anyhow::Result;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{db, deployment::JobInput, error::ApiError};

pub fn transport_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<crate::ssh::SshError>(),
            Some(crate::ssh::SshError::Transport(_))
        )
    })
}

pub fn retry_delay(attempts: i64) -> Option<i64> {
    match attempts {
        1 => Some(30),
        2 => Some(60),
        3 => Some(120),
        4 => Some(300),
        _ => None,
    }
}

fn payload(user: &str, generation: i64, reasons: Value) -> Value {
    let node_only = reasons
        .as_object()
        .is_some_and(|r| r.len() == 1 && r.contains_key("node_limit"));
    json!({"user_id":user,"kick_generation":generation,"kick_reasons":reasons,"node_limit":node_only})
}

pub async fn prepare(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    input: &Value,
) -> Result<(Option<String>, Value), ApiError> {
    let user = input
        .get("user_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("kick requires user_id"))?;
    let reason = input
        .get("kick_reason")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if input.get("node_limit").and_then(Value::as_bool) == Some(true) {
                "node_limit"
            } else {
                "credentials_revoked"
            }
        });
    let row = sqlx::query("INSERT INTO kick_requests(node_id,user_id,reasons,state) VALUES($1,$2,$3,'active') ON CONFLICT(node_id,user_id) DO UPDATE SET reasons=CASE WHEN kick_requests.state IN ('completed','cancelled') THEN EXCLUDED.reasons ELSE kick_requests.reasons || EXCLUDED.reasons END, generation=kick_requests.generation+1,state='active',updated_at=now() RETURNING generation,reasons")
        .bind(node).bind(user).bind(json!({reason:true})).fetch_one(&mut **tx).await?;
    let payload = payload(user, row.get("generation"), row.get("reasons"));
    let existing: Option<(String,String)> = sqlx::query_as("SELECT id,status FROM jobs WHERE kind='kick' AND node_id=$1 AND payload_json->>'user_id'=$2 AND status IN ('queued','running') ORDER BY created_at,id LIMIT 1")
        .bind(node).bind(user).fetch_optional(&mut **tx).await?;
    if let Some((id, status)) = existing {
        // A running worker keeps its original generation. It cannot acknowledge a
        // revocation committed after it started; completion schedules a fresh job.
        if status == "queued" {
            sqlx::query("UPDATE jobs SET payload_json=$2,updated_at=now() WHERE id=$1")
                .bind(&id)
                .bind(&payload)
                .execute(&mut **tx)
                .await?;
        }
        attach(tx, node, user, &id).await?;
        event(tx, &id, "job.kick_merged", json!({"id":id,"kind":"kick","node_id":node,"status":status,"reason":reason,"generation":row.get::<i64,_>("generation")})).await?;
        return Ok((Some(id), payload));
    }
    Ok((None, payload))
}

pub async fn attach(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    user: &str,
    job: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE kick_requests SET latest_job_id=$3,updated_at=now() WHERE node_id=$1 AND user_id=$2")
        .bind(node).bind(user).bind(job).execute(&mut **tx).await?;
    Ok(())
}

pub async fn still_required(pool: &PgPool, job: &JobInput) -> Result<bool> {
    let node = job.node_id.as_deref().unwrap_or_default();
    let user = job
        .payload
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reasons: Option<Value> = sqlx::query_scalar("SELECT reasons FROM kick_requests WHERE node_id=$1 AND user_id=$2 AND state NOT IN ('completed','cancelled')")
        .bind(node).bind(user).fetch_optional(pool).await?;
    let reasons = reasons
        .or_else(|| job.payload.get("kick_reasons").cloned())
        .unwrap_or_else(|| {
            if job.payload.get("node_limit").and_then(Value::as_bool) == Some(true) {
                json!({"node_limit":true})
            } else {
                json!({"legacy_revocation":true})
            }
        });
    if reasons.as_object().is_some_and(|r| {
        r.keys()
            .any(|key| key != "node_limit" && key != "access_restricted")
    }) {
        return Ok(true);
    }
    if reasons.get("node_limit").is_some() && crate::node_limits::restricted(pool, node).await? {
        return Ok(true);
    }
    if reasons.get("access_restricted").is_some() {
        let restricted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1 AND (NOT enabled OR expires_at<=now() OR usage_bytes>=quota_bytes))")
            .bind(user).fetch_one(pool).await?;
        if restricted {
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn finish(
    tx: &mut Transaction<'_, Postgres>,
    job: &JobInput,
    state: &str,
) -> Result<(), sqlx::Error> {
    if job.kind != "kick" {
        return Ok(());
    }
    let generation = job
        .payload
        .get("kick_generation")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    sqlx::query("UPDATE kick_requests SET state=$4,updated_at=now() WHERE node_id=$1 AND user_id=$2 AND generation=$3 AND latest_job_id=$5")
        .bind(&job.node_id).bind(job.payload.get("user_id").and_then(Value::as_str)).bind(generation).bind(state).bind(&job.id).execute(&mut **tx).await?;
    Ok(())
}

// Remove only a conditional reason; merged credential/deletion obligations survive.
pub async fn clear_reason(
    tx: &mut Transaction<'_, Postgres>,
    node: Option<&str>,
    user: Option<&str>,
    reason: &str,
) -> Result<(), sqlx::Error> {
    let rows = sqlx::query("UPDATE kick_requests SET reasons=reasons-$3,generation=generation+1,updated_at=now() WHERE ($1::text IS NULL OR node_id=$1) AND ($2::text IS NULL OR user_id=$2) AND reasons ? $3 AND state NOT IN ('completed','cancelled') RETURNING node_id,user_id,generation,reasons")
        .bind(node).bind(user).bind(reason).fetch_all(&mut **tx).await?;
    for row in rows {
        let node: String = row.get("node_id");
        let user: String = row.get("user_id");
        let reasons: Value = row.get("reasons");
        let empty = reasons.as_object().is_some_and(|r| r.is_empty());
        if empty {
            sqlx::query(
                "UPDATE kick_requests SET state='cancelled' WHERE node_id=$1 AND user_id=$2",
            )
            .bind(&node)
            .bind(&user)
            .execute(&mut **tx)
            .await?;
            let ids: Vec<String> = sqlx::query_scalar("UPDATE jobs SET status='cancelled',stage='restriction_cleared',finished_at=now(),updated_at=now() WHERE kind='kick' AND node_id=$1 AND payload_json->>'user_id'=$2 AND status='queued' RETURNING id")
                .bind(&node).bind(&user).fetch_all(&mut **tx).await?;
            for id in ids {
                event(tx,&id,"job.cancelled",json!({"id":id,"node_id":node,"status":"cancelled","stage":"restriction_cleared"})).await?;
            }
        } else {
            sqlx::query("UPDATE jobs SET payload_json=$3,updated_at=now() WHERE kind='kick' AND node_id=$1 AND payload_json->>'user_id'=$2 AND status='queued'")
                .bind(&node).bind(&user).bind(payload(&user,row.get("generation"),reasons)).execute(&mut **tx).await?;
        }
    }
    Ok(())
}

pub async fn event(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    kind: &str,
    payload: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES($1,$2,$3,now())",
    )
    .bind(id)
    .bind(kind)
    .bind(payload)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// Called after trusted SSH has actually recovered. Permanent failures are left
// for an explicit operator action, rather than retried on every monitoring pass.
pub async fn resume_node(pool: &PgPool, node: &str) -> Result<()> {
    let mut tx = db::begin_write(pool).await?;
    schedule(&mut tx, Some(node), true)
        .await
        .map_err(|e| anyhow::anyhow!(e.message))?;
    tx.commit().await?;
    Ok(())
}

pub async fn schedule(
    tx: &mut Transaction<'_, Postgres>,
    node: Option<&str>,
    recovered: bool,
) -> Result<(), ApiError> {
    let rows=sqlx::query("SELECT r.* FROM kick_requests r JOIN nodes n ON n.id=r.node_id WHERE ($1::text IS NULL OR r.node_id=$1) AND (r.state='active' OR ($2 AND r.state='waiting_recovery')) AND n.state NOT IN ('deleting','delete_failed') AND NOT EXISTS(SELECT 1 FROM jobs j WHERE j.kind='kick' AND j.node_id=r.node_id AND j.payload_json->>'user_id'=r.user_id AND j.status IN ('queued','running')) ORDER BY r.updated_at LIMIT 100")
        .bind(node).bind(recovered).fetch_all(&mut **tx).await?;
    for row in rows {
        let node: String = row.get("node_id");
        let user: String = row.get("user_id");
        let id = uuid::Uuid::new_v4().to_string();
        let parent: Option<String> = row.get("latest_job_id");
        let data = payload(&user, row.get("generation"), row.get("reasons"));
        sqlx::query("INSERT INTO jobs(id,kind,node_id,node_name,payload_json,status,stage,available_at,created_at,updated_at,retry_of_job_id) SELECT $1,'kick',id,name,$3,'queued','queued',now(),now(),now(),(SELECT id FROM jobs WHERE id=$4 AND status='failed') FROM nodes WHERE id=$2")
            .bind(&id).bind(&node).bind(data).bind(&parent).execute(&mut **tx).await?;
        sqlx::query("UPDATE kick_requests SET state='active',latest_job_id=$3,updated_at=now() WHERE node_id=$1 AND user_id=$2")
            .bind(&node).bind(&user).bind(&id).execute(&mut **tx).await?;
        event(tx,&id,"job.queued",json!({"id":id,"kind":"kick","node_id":node,"status":"queued","stage":"queued","recovery_of_job_id":parent})).await?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{api::enqueue_job_with_payload_in_tx, security::SecretBox, state::AppState};

    pub async fn fixture() -> AppState {
        let pool = crate::db::test_pool_with_max_connections(3).await;
        sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES('node','Node','127.0.0.1',22,'root','private_key','unused','example.test',443,':443','hash','unused','unused','{}',now(),now())")
            .execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO users(id,name,created_at,updated_at) VALUES('user','User',now(),now())",
        )
        .execute(&pool)
        .await
        .unwrap();
        AppState::new(
            pool,
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
        )
    }

    pub async fn enqueue(state: &AppState, reason: &str) -> String {
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        let id = enqueue_job_with_payload_in_tx(
            &mut tx,
            "kick",
            Some("node"),
            None,
            json!({"user_id":"user","kick_reason":reason}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }

    pub async fn job(state: &AppState, id: &str) -> JobInput {
        let row = sqlx::query("SELECT payload_json,attempts FROM jobs WHERE id=$1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        JobInput {
            id: id.into(),
            kind: "kick".into(),
            node_id: Some("node".into()),
            target_revision: None,
            payload: row.get("payload_json"),
            attempts: row.get("attempts"),
        }
    }

    #[tokio::test]
    async fn concurrent_revocations_merge_and_survive_user_deletion() {
        let state = fixture().await;
        let (a, b) = tokio::join!(
            enqueue(&state, "credentials_revoked"),
            enqueue(&state, "user_deleted")
        );
        assert_eq!(a, b);
        let reasons: Value = sqlx::query_scalar("SELECT reasons FROM kick_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            reasons,
            json!({"credentials_revoked":true,"user_deleted":true})
        );
        sqlx::query("DELETE FROM users")
            .execute(&state.pool)
            .await
            .unwrap();
        assert!(
            still_required(&state.pool, &job(&state, &a).await)
                .await
                .unwrap()
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE status='queued'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn renewal_cancels_restriction_but_preserves_revocation() {
        let state = fixture().await;
        let id = enqueue(&state, "access_restricted").await;
        assert!(
            !still_required(&state.pool, &job(&state, &id).await)
                .await
                .unwrap()
        );
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        clear_reason(&mut tx, None, Some("user"), "access_restricted")
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "cancelled");
        let id = enqueue(&state, "node_limit").await;
        assert_eq!(id, enqueue(&state, "credentials_revoked").await);
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        clear_reason(&mut tx, Some("node"), None, "node_limit")
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert!(
            still_required(&state.pool, &job(&state, &id).await)
                .await
                .unwrap()
        );
        let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(status, "queued");
    }

    #[tokio::test]
    async fn running_job_cannot_acknowledge_a_newer_revocation() {
        let state = fixture().await;
        let id = enqueue(&state, "node_limit").await;
        sqlx::query("UPDATE jobs SET status='running' WHERE id=$1")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();
        let old = job(&state, &id).await;
        assert_eq!(id, enqueue(&state, "user_deleted").await);
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        finish(&mut tx, &old, "completed").await.unwrap();
        sqlx::query("UPDATE jobs SET status='succeeded' WHERE id=$1")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .unwrap();
        schedule(&mut tx, None, false).await.unwrap();
        tx.commit().await.unwrap();
        let request: (String, String) =
            sqlx::query_as("SELECT state,latest_job_id FROM kick_requests")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(request.0, "active");
        assert_ne!(request.1, id);
        assert!(
            still_required(&state.pool, &job(&state, &request.1).await)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn ssh_recovery_resumes_once_and_does_not_retry_permanent_errors() {
        let state = fixture().await;
        let id = enqueue(&state, "user_deleted").await;
        sqlx::query("UPDATE jobs SET status='failed',stage='waiting_recovery' WHERE id=$1")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE kick_requests SET state='waiting_recovery'")
            .execute(&state.pool)
            .await
            .unwrap();
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        schedule(&mut tx, None, false).await.unwrap();
        tx.commit().await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        resume_node(&state.pool, "node").await.unwrap();
        resume_node(&state.pool, "node").await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
        let child: String = sqlx::query_scalar("SELECT latest_job_id FROM kick_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let parent: String = sqlx::query_scalar("SELECT retry_of_job_id FROM jobs WHERE id=$1")
            .bind(&child)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(parent, id);
        sqlx::query("UPDATE jobs SET status='failed',stage='needs_attention' WHERE id=$1")
            .bind(&child)
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE kick_requests SET state='needs_attention'")
            .execute(&state.pool)
            .await
            .unwrap();
        resume_node(&state.pool, "node").await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
        let (_, reply) = crate::api::job_retries::retry(
            axum::extract::State(state.clone()),
            axum::extract::Path(child.clone()),
            axum::Json(crate::api::job_retries::RetryRequest {
                expected_revision: 1,
            }),
        )
        .await
        .unwrap();
        let (_, again) = crate::api::job_retries::retry(
            axum::extract::State(state.clone()),
            axum::extract::Path(child),
            axum::Json(crate::api::job_retries::RetryRequest {
                expected_revision: 1,
            }),
        )
        .await
        .unwrap();
        assert_eq!(reply.0, again.0);
    }

    #[tokio::test]
    async fn migration_consolidates_exhausted_deleted_user_jobs_and_keeps_events() {
        let state = fixture().await;
        sqlx::raw_sql("DROP TABLE kick_requests; DROP INDEX jobs_active_kick_idx;")
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users")
            .execute(&state.pool)
            .await
            .unwrap();
        for id in ["one", "two", "three"] {
            sqlx::query("INSERT INTO jobs(id,kind,node_id,payload_json,status,stage,attempts,error_message,available_at,created_at,updated_at) VALUES($1,'kick','node',$2,'queued','retry_wait',115,'SSH connection failed: SSH connection timed out',now(),now(),now())")
                .bind(id).bind(json!({"user_id":"user"})).execute(&state.pool).await.unwrap();
        }
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/0008_kick_requests.sql"))
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let states: Vec<String> = sqlx::query_scalar("SELECT status FROM jobs ORDER BY id")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(states.iter().filter(|s| *s == "failed").count(), 1);
        assert_eq!(states.iter().filter(|s| *s == "cancelled").count(), 2);
        let r: (String, Value) = sqlx::query_as("SELECT state,reasons FROM kick_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(r.0, "waiting_recovery");
        assert_eq!(r.1, json!({"legacy_revocation":true}));
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM job_events")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(events, 3);
    }

    #[test]
    fn retry_budget_and_error_classification() {
        assert_eq!(
            (1..=6).map(retry_delay).collect::<Vec<_>>(),
            vec![Some(30), Some(60), Some(120), Some(300), None, None]
        );
        assert!(transport_error(
            &anyhow::Error::new(crate::ssh::SshError::Transport("timeout".into()))
                .context("wrapped")
        ));
        assert!(!transport_error(
            &crate::ssh::SshError::AuthenticationFailed.into()
        ));
        assert!(!crate::deployment::retryable(
            &crate::ssh::SshError::AuthenticationFailed.into()
        ));
        assert!(crate::deployment::retryable(
            &crate::ssh::SshError::ClientsStillOnline { remaining: 1 }.into()
        ));
    }
}
