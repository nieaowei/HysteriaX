use super::*;
use crate::{api::enqueue_job_with_payload_in_tx, security::SecretBox, state::AppState};

pub async fn fixture() -> AppState {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    sqlx::query("WITH fixture_ssh AS (INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('fixture-ssh','Fixture SSH','ssh_password',now(),now()) ON CONFLICT DO NOTHING), fixture_ssh_version AS (INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('fixture-ssh',1,'unused',now()) ON CONFLICT DO NOTHING) INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at,ssh_credential_id,ssh_credential_version) VALUES ('node','Node','127.0.0.1',22,'root','example.test',443,':443','hash','unused','unused','{}',now(),now(),'fixture-ssh',1)")
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
    let request: (String, String) = sqlx::query_as("SELECT state,latest_job_id FROM kick_requests")
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
        &anyhow::Error::new(crate::ssh::SshError::Transport("timeout".into())).context("wrapped")
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
