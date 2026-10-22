use super::*;
use crate::background::maintenance;

async fn fixture() -> (AppState, String, String) {
    trust::tests::fixture(0).await
}
async fn phase(state: &AppState, id: &str) -> String {
    sqlx::query_scalar("SELECT phase FROM mtls_rotations WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn count(state: &AppState, q: &str) -> i64 {
    sqlx::query_scalar(q).fetch_one(&state.pool).await.unwrap()
}

#[tokio::test]
async fn an_old_ca_session_lock_cannot_block_the_resource_owned_coordinator() {
    let (state, ca, id) = fixture().await;
    let mut held = state.pool.acquire().await.unwrap();
    held.close_on_drop();
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1,0))")
        .bind(format!("hysteriax:mtls-ca:{ca}"))
        .execute(&mut *held)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), run_pass(&state))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(phase(&state, &id).await, "trust");
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-rotation-trust' AND state='complete'").await,1);
    sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1,0))")
        .bind(format!("hysteriax:mtls-ca:{ca}"))
        .execute(&mut *held)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_healthy_ca_work_keeps_its_owner_and_error_while_other_coordinator_calls_skip_it() {
    let (state, ca, id) = fixture().await;
    maintenance::ensure(&state.pool,&format!("rotation-trust:{id}"),"mtls-rotation-trust",json!({"rotation_id":id,"ca_id":ca,"ca_version":2,"cursor":"","lease_resources":[format!("ca:{ca}")]})).await.unwrap();
    let work = maintenance::claim(&state.pool, "mtls-rotation-trust", "healthy")
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE mtls_rotations SET error_message='Owned pending state' WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    run_pass(&state).await.unwrap();
    assert_eq!(phase(&state, &id).await, "trust");
    let error: Option<String> =
        sqlx::query_scalar("SELECT error_message FROM mtls_rotations WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(error.as_deref(), Some("Owned pending state"));
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-rotation-trust' AND owner='healthy' AND state='running'").await,1);
    for lease in &work.leases {
        assert!(lease.renew(&state.pool).await.unwrap());
    }
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}

#[tokio::test]
async fn two_coordinators_share_the_same_work_without_duplicate_completion_or_remote_jobs() {
    let (state, _, id) = fixture().await;
    let other = AppState::new(
        state.pool.clone(),
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    assert_ne!(state.instance_id, other.instance_id);
    let (a, b) = tokio::join!(run_pass(&state), run_pass(&other));
    a.unwrap();
    b.unwrap();
    assert_ne!(phase(&state, &id).await, "trust");
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-trust'"
        )
        .await,
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    assert!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.published'"
        )
        .await
            <= 1
    );
}
