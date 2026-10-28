//! Durable per-user obligations executed by one bounded, resumable task per node.
use crate::{db, deployment::JobInput, error::ApiError};
use anyhow::Result;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};

pub const BATCH_SIZE: i64 = 200;
pub const BATCHES_PER_PASS: usize = 10;

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

pub async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    input: &Value,
) -> Result<String, ApiError> {
    let user = input["user_id"]
        .as_str()
        .ok_or_else(|| ApiError::bad_request("kick requires user_id"))?;
    let reason = input["kick_reason"]
        .as_str()
        .unwrap_or(if input["node_limit"] == true {
            "node_limit"
        } else {
            "credentials_revoked"
        });
    enqueue_many(tx, node, &[user.to_owned()], reason).await
}

pub async fn enqueue_many(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    users: &[String],
    reason: &str,
) -> Result<String, ApiError> {
    let users: Vec<_> = users
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if users.is_empty() {
        return Err(ApiError::bad_request("kick requires at least one user"));
    }
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(node)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("node"))?;
    let id = ensure_job(tx, node, None).await?;
    record_many(tx, node, &users, reason).await?;
    sqlx::query("UPDATE kick_requests SET latest_job_id=$3,execution_generation=CASE WHEN (SELECT status FROM jobs WHERE id=$3)='queued' THEN generation ELSE execution_generation END WHERE node_id=$1 AND user_id=ANY($2)")
        .bind(node)
        .bind(&users)
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    Ok(id)
}

pub async fn record_many(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    users: &[String],
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO kick_requests(node_id,user_id,reasons,state) SELECT $1,u,jsonb_build_object($3::text,true),'active' FROM unnest($2::text[]) u ORDER BY u ON CONFLICT(node_id,user_id) DO UPDATE SET reasons=CASE WHEN kick_requests.state IN ('completed','cancelled') THEN EXCLUDED.reasons ELSE kick_requests.reasons||EXCLUDED.reasons END,generation=kick_requests.generation+1,state='active',updated_at=now()")
        .bind(node).bind(users).bind(reason).execute(&mut **tx).await?;
    Ok(())
}

pub async fn ensure_job(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    parent: Option<&str>,
) -> Result<String, ApiError> {
    let id = uuid::Uuid::new_v4().to_string();
    let row=sqlx::query("INSERT INTO jobs(id,kind,node_id,node_name,payload_json,status,stage,available_at,created_at,updated_at,retry_of_job_id) SELECT $1,'kick',id,name,'{\"scope\":\"node\"}'::jsonb,'queued','queued',now(),now(),now(),(SELECT id FROM jobs WHERE id=$3 AND status='failed') FROM nodes WHERE id=$2 ON CONFLICT(node_id) WHERE kind='kick' AND status IN ('queued','running') DO UPDATE SET updated_at=jobs.updated_at RETURNING id,(xmax=0) AS inserted")
        .bind(&id).bind(node).bind(parent).fetch_one(&mut **tx).await?;
    let id: String = row.get("id");
    sqlx::query("UPDATE kick_requests SET latest_job_id=$2,execution_generation=generation,updated_at=now() WHERE node_id=$1 AND state='active' AND latest_job_id IS DISTINCT FROM $2").bind(node).bind(&id).execute(&mut **tx).await?;
    if row.get::<bool, _>("inserted") {
        event(
            tx,
            &id,
            "job.queued",
            json!({"id":id,"kind":"kick","node_id":node,"status":"queued","stage":"queued"}),
        )
        .await?;
    }
    Ok(id)
}

#[derive(Clone, Debug)]
pub struct Request {
    pub user: String,
    pub generation: i64,
    pub required: bool,
}

pub async fn batch(
    pool: &PgPool,
    job: &JobInput,
    users: Option<&[String]>,
) -> Result<Vec<Request>> {
    let node = job
        .node_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("kick requires node"))?;
    let restricted = crate::node_limits::restricted(pool, node).await?;
    let (job, users) = (job.clone(), users.map(|u| u.to_vec()));
    db::write(pool,move|tx|{let (job,users)=(job.clone(),users.clone());Box::pin(async move {
        if crate::execution::job_source_on(tx,&job).await?.is_none(){anyhow::bail!("kick job no longer running");}
        let rows=sqlx::query("SELECT r.user_id,r.generation,(EXISTS(SELECT 1 FROM jsonb_object_keys(r.reasons) k WHERE k NOT IN ('node_limit','access_restricted')) OR (r.reasons ? 'node_limit' AND $4) OR (r.reasons ? 'access_restricted' AND (NOT u.enabled OR u.expires_at<=now() OR u.usage_bytes>=u.quota_bytes))) IS TRUE AS required FROM kick_requests r LEFT JOIN users u ON u.id=r.user_id WHERE r.node_id=$1 AND r.latest_job_id=$2 AND r.state='active' AND ($3::text[] IS NULL OR r.user_id=ANY($3)) ORDER BY r.user_id LIMIT $5 FOR UPDATE OF r SKIP LOCKED")
            .bind(&job.node_id).bind(&job.id).bind(users.as_deref()).bind(restricted).bind(BATCH_SIZE).fetch_all(&mut **tx).await?;
        let requests:Vec<Request>=rows.into_iter().map(|r|Request{user:r.get("user_id"),generation:r.get("generation"),required:r.get("required")}).collect();
        let ids:Vec<_>=requests.iter().map(|r|r.user.clone()).collect();
        sqlx::query("UPDATE kick_requests SET execution_generation=generation WHERE node_id=$1 AND latest_job_id=$2 AND user_id=ANY($3) AND state='active'")
            .bind(&job.node_id).bind(&job.id).bind(ids).execute(&mut **tx).await?;
        Ok(requests)
    })}).await
}
#[cfg(test)]
pub async fn still_required(pool: &PgPool, job: &JobInput) -> Result<bool> {
    // Check all active reasons, rather than letting cleared users in the first page hide later work.
    let node = job.node_id.as_deref().unwrap_or_default();
    let restricted = crate::node_limits::restricted(pool, node).await?;
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM kick_requests r LEFT JOIN users u ON u.id=r.user_id WHERE r.node_id=$1 AND r.latest_job_id=$2 AND r.state='active' AND (EXISTS(SELECT 1 FROM jsonb_object_keys(r.reasons) k WHERE k NOT IN ('node_limit','access_restricted')) OR (r.reasons ? 'node_limit' AND $3) OR (r.reasons ? 'access_restricted' AND (NOT u.enabled OR u.expires_at<=now() OR u.usage_bytes>=u.quota_bytes))))")
        .bind(node).bind(&job.id).bind(restricted).fetch_one(pool).await?)
}

pub async fn acknowledge(
    pool: &PgPool,
    job: &JobInput,
    requests: &[Request],
    state: &str,
) -> Result<()> {
    if requests.is_empty() {
        return Ok(());
    }
    let (job, requests, state) = (job.clone(), requests.to_vec(), state.to_owned());
    db::write(pool,move |tx| {let (job,requests,state)=(job.clone(),requests.clone(),state.clone());Box::pin(async move {
        if crate::execution::job_source_on(tx,&job).await?.is_none() {anyhow::bail!("kick job no longer running");}
        let users:Vec<_>=requests.iter().map(|r|r.user.clone()).collect();
        let generations:Vec<_>=requests.iter().map(|r|r.generation).collect();
        sqlx::query("UPDATE kick_requests r SET state=$5,updated_at=now() FROM unnest($3::text[],$4::bigint[]) b(user_id,generation) WHERE r.node_id=$1 AND r.latest_job_id=$2 AND r.user_id=b.user_id AND r.generation=b.generation AND r.state='active'")
            .bind(&job.node_id).bind(&job.id).bind(users).bind(generations).bind(&state).execute(&mut **tx).await?;
        Ok(())
    })}).await
}

pub async fn finish(
    tx: &mut Transaction<'_, Postgres>,
    job: &JobInput,
    state: &str,
) -> Result<(), sqlx::Error> {
    if job.kind != "kick" || matches!(state, "completed" | "cancelled") {
        return Ok(());
    }
    // Success is checkpointed per user/generation by the executor, never acknowledged wholesale.
    sqlx::query("UPDATE kick_requests SET state=$3,updated_at=now() WHERE node_id=$1 AND latest_job_id=$2 AND state='active' AND (execution_generation IS NULL OR execution_generation=generation)")
        .bind(&job.node_id).bind(&job.id).bind(state).execute(&mut **tx).await?;
    Ok(())
}

pub async fn clear_reason(
    tx: &mut Transaction<'_, Postgres>,
    node: Option<&str>,
    user: Option<&str>,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT j.id FROM jobs j WHERE kind='kick' AND ($1::text IS NULL OR node_id=$1) AND status IN ('queued','running') AND EXISTS(SELECT 1 FROM kick_requests r WHERE r.node_id=j.node_id AND ($2::text IS NULL OR r.user_id=$2) AND r.reasons ? $3) ORDER BY j.node_id,j.id FOR UPDATE OF j")
        .bind(node).bind(user).bind(reason).fetch_all(&mut **tx).await?;
    let nodes:Vec<String>=sqlx::query_scalar("WITH cleared AS (UPDATE kick_requests SET reasons=reasons-$3,generation=generation+1,state=CASE WHEN reasons-$3='{}'::jsonb THEN 'cancelled' ELSE state END,updated_at=now() WHERE ($1::text IS NULL OR node_id=$1) AND ($2::text IS NULL OR user_id=$2) AND reasons ? $3 AND state NOT IN ('completed','cancelled') RETURNING node_id) SELECT DISTINCT node_id FROM cleared")
        .bind(node).bind(user).bind(reason).fetch_all(&mut **tx).await?;
    let nodes: Vec<_> = nodes
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let ids:Vec<String>=sqlx::query_scalar("UPDATE jobs j SET status='cancelled',stage='restriction_cleared',finished_at=now(),updated_at=now() WHERE kind='kick' AND node_id=ANY($1) AND status='queued' AND NOT EXISTS(SELECT 1 FROM kick_requests r WHERE r.node_id=j.node_id AND r.latest_job_id=j.id AND r.state='active') RETURNING id")
        .bind(nodes).fetch_all(&mut **tx).await?;
    for id in ids {
        event(
            tx,
            &id,
            "job.cancelled",
            json!({"id":id,"status":"cancelled","stage":"restriction_cleared"}),
        )
        .await?;
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
pub async fn resume_node(pool: &PgPool, node: &str) -> Result<()> {
    let node = node.to_owned();
    db::serializable::<_, ApiError, _>(pool, move |tx| {
        let node = node.clone();
        Box::pin(async move {
            let active:Option<String>=sqlx::query_scalar("SELECT id FROM jobs WHERE node_id=$1 AND kind='kick' AND status IN ('queued','running') FOR UPDATE")
                .bind(&node).fetch_optional(&mut **tx).await?;
            if active.is_some() {
                sqlx::query("UPDATE kick_requests SET state='active' WHERE node_id=$1 AND state='waiting_recovery'").bind(&node).execute(&mut **tx).await?;
                ensure_job(tx,&node,None).await?;
                Ok(())
            } else {schedule(tx, Some(&node), true).await}
        })
    })
    .await
    .map_err(|e| anyhow::anyhow!(e.message))
}
pub async fn schedule(
    tx: &mut Transaction<'_, Postgres>,
    node: Option<&str>,
    recovered: bool,
) -> Result<(), ApiError> {
    schedule_batch(tx, node, recovered).await.map(|_| ())
}
pub struct SchedulePage {
    pub count: usize,
    pub next_node: Option<String>,
    pub more: bool,
}
pub async fn schedule_batch(
    tx: &mut Transaction<'_, Postgres>,
    node: Option<&str>,
    recovered: bool,
) -> Result<usize, ApiError> {
    schedule_page(tx, node, recovered, None)
        .await
        .map(|p| p.count)
}
pub async fn schedule_page(
    tx: &mut Transaction<'_, Postgres>,
    node: Option<&str>,
    recovered: bool,
    after: Option<&str>,
) -> Result<SchedulePage, ApiError> {
    let nodes:Vec<String>=sqlx::query_scalar("SELECT n.id FROM nodes n WHERE ($1::text IS NULL OR n.id=$1) AND ($3::text IS NULL OR n.id>$3) AND n.state NOT IN ('deleting','delete_failed') AND EXISTS(SELECT 1 FROM kick_requests r WHERE r.node_id=n.id AND (r.state='active' OR ($2 AND r.state='waiting_recovery')) AND NOT EXISTS(SELECT 1 FROM jobs failed WHERE failed.id=r.latest_job_id AND failed.status='failed' AND (failed.stage IN ('needs_attention','reconciling') OR (failed.stage='waiting_recovery' AND NOT $2)))) AND NOT EXISTS(SELECT 1 FROM jobs j WHERE j.kind='kick' AND j.node_id=n.id AND j.status IN ('queued','running')) ORDER BY n.id LIMIT 100 FOR NO KEY UPDATE OF n SKIP LOCKED")
        .bind(node).bind(recovered).bind(after).fetch_all(&mut **tx).await?;
    for node in &nodes {
        let parent:Option<String>=sqlx::query_scalar("SELECT r.latest_job_id FROM kick_requests r JOIN jobs j ON j.id=r.latest_job_id AND j.status='failed' WHERE r.node_id=$1 AND (r.state='active' OR ($2 AND r.state='waiting_recovery')) AND NOT EXISTS(SELECT 1 FROM jobs failed WHERE failed.id=r.latest_job_id AND failed.status='failed' AND (failed.stage IN ('needs_attention','reconciling') OR (failed.stage='waiting_recovery' AND NOT $2))) ORDER BY r.updated_at,r.user_id LIMIT 1")
            .bind(node).bind(recovered).fetch_optional(&mut **tx).await?.flatten();
        if recovered {
            sqlx::query("UPDATE kick_requests SET state='active' WHERE node_id=$1 AND state='waiting_recovery'").bind(node).execute(&mut **tx).await?;
        }
        ensure_job(tx, node, parent.as_deref()).await?;
    }
    let more:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM kick_requests r JOIN nodes n ON n.id=r.node_id WHERE ($1::text IS NULL OR r.node_id=$1) AND n.state NOT IN ('deleting','delete_failed') AND (r.state='active' OR ($2 AND r.state='waiting_recovery')) AND NOT EXISTS(SELECT 1 FROM jobs failed WHERE failed.id=r.latest_job_id AND failed.status='failed' AND (failed.stage IN ('needs_attention','reconciling') OR (failed.stage='waiting_recovery' AND NOT $2))) AND NOT EXISTS(SELECT 1 FROM jobs j WHERE j.kind='kick' AND j.node_id=r.node_id AND j.status IN ('queued','running')))")
        .bind(node).bind(recovered).fetch_one(&mut **tx).await?;
    Ok(SchedulePage {
        count: nodes.len(),
        next_node: nodes.last().cloned(),
        more,
    })
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
        let mut tx = state.pool.begin().await.unwrap();
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
        let mut tx = state.pool.begin().await.unwrap();
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
        let mut tx = state.pool.begin().await.unwrap();
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
        let mut tx = state.pool.begin().await.unwrap();
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
        let mut tx = state.pool.begin().await.unwrap();
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

#[cfg(test)]
mod batch_tests;
