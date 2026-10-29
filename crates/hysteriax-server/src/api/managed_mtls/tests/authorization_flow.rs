use super::*;
pub(super) async fn group(state: &AppState, name: &str, users: &[&str], nodes: &[&str]) -> Value {
    let _ = Box::pin(crate::api::authorization_groups::create(
        State(state.clone()),
        Json(
            serde_json::from_value(json!({"name":name,"user_ids":users,"node_ids":nodes})).unwrap(),
        ),
    ))
    .await
    .unwrap();
    for _ in 0..users.len() {
        Box::pin(credentials::maintenance::issue_pending(state))
            .await
            .unwrap();
    }
    crate::api::authorization_groups::group_named(state, name).await
}

#[tokio::test]
async fn authorization_job_only_admits_signing_and_revoked_pending_work_cannot_publish() {
    use super::node_flow::managed_node;
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    for user_id in ["alice", "bob"] {
        user(&state, user_id, true).await;
        let _ = Box::pin(crate::api::authorization_groups::create(
            State(state.clone()),
            Json(
                serde_json::from_value(
                    json!({"name":user_id,"user_ids":[user_id],"node_ids":[id]}),
                )
                .unwrap(),
            ),
        ))
        .await
        .unwrap();
    }
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM mtls_user_versions")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(before, 0);
    let queued:i64=sqlx::query_scalar("SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal' AND state='queued'").fetch_one(&state.pool).await.unwrap();
    assert_eq!(queued, 2);
    let bob = crate::api::authorization_groups::group_named(&state, "bob").await;
    let _ = Box::pin(crate::api::authorization_groups::delete(
        State(state.clone()),
        Path(bob["id"].as_str().unwrap().into()),
        Json(serde_json::from_value(json!({"expected_revision":bob["revision"]})).unwrap()),
    ))
    .await
    .unwrap();
    for _ in 0..2 {
        Box::pin(credentials::maintenance::issue_pending(&state))
            .await
            .unwrap();
    }
    let users:Vec<String>=sqlx::query_scalar("SELECT DISTINCT i.user_id FROM mtls_user_versions v JOIN mtls_user_identities i ON i.id=v.identity_id").fetch_all(&state.pool).await.unwrap();
    assert_eq!(users, vec!["alice"]);
}
