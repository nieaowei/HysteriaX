use super::node_flow::managed_node;
use super::*;
use crate::api::authorization_groups as groups;

async fn preview(state: &AppState, name: &str, users: &[&str], nodes: &[&str]) -> Value {
    groups::preview_create(
        State(state.clone()),
        Json(
            serde_json::from_value(
                json!({"action":"create","name":name,"user_ids":users,"node_ids":nodes}),
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap()
    .0
}
async fn commit(
    state: &AppState,
    name: &str,
    users: &[&str],
    nodes: &[&str],
    preview: &Value,
) -> Result<Value, ApiError> {
    Ok(groups::create(State(state.clone()),Json(serde_json::from_value(json!({"name":name,"user_ids":users,"node_ids":nodes,"preview_token":preview["preview_token"]})).unwrap())).await?.1.0)
}
pub(super) async fn group(state: &AppState, name: &str, users: &[&str], nodes: &[&str]) -> Value {
    let p = preview(state, name, users, nodes).await;
    commit(state, name, users, nodes, &p).await.unwrap()
}
async fn remove(state: &AppState, group: &Value) {
    let id = group["group"]["id"].as_str().unwrap();
    let revision = group["group"]["revision"].as_i64().unwrap();
    let Json(preview) = groups::preview_group(
        State(state.clone()),
        Path(id.into()),
        Json(
            serde_json::from_value(json!({"action":"delete","expected_revision":revision}))
                .unwrap(),
        ),
    )
    .await
    .unwrap();
    let _ = groups::delete(
        State(state.clone()),
        Path(id.into()),
        Json(
            serde_json::from_value(
                json!({"expected_revision":revision,"preview_token":preview["preview_token"]}),
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
}
async fn identity_count(state: &AppState) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM mtls_user_identities")
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn version_count(state: &AppState) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM mtls_user_versions")
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn two_nodes_one_ca_issue_once_preview_is_read_only_and_permissions_union_controls_archival()
{
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let first = managed_node(&state, &ca).await;
    let second = managed_node(&state, &ca).await;
    let n1 = first["id"].as_str().unwrap();
    let n2 = second["id"].as_str().unwrap();
    let p = preview(&state, "Both", &["alice"], &[n1, n2]).await;
    assert_eq!(p["automatic_issue_count"], 1);
    assert_eq!(p["pending_issue_count"], 0);
    assert!(p.get("missing_mtls").is_none());
    assert_eq!(identity_count(&state).await, 0);
    assert_eq!(version_count(&state).await, 0);
    let first_group = commit(&state, "Both", &["alice"], &[n1, n2], &p)
        .await
        .unwrap();
    assert_eq!(identity_count(&state).await, 1);
    assert_eq!(version_count(&state).await, 1);
    sqlx::query("UPDATE mtls_node_trust SET deployed=TRUE WHERE ca_id=$1")
        .bind(&ca)
        .execute(&state.pool)
        .await
        .unwrap();
    let a = credentials::identities::for_node(&state.pool, &state.secrets, "alice", n1, &ca)
        .await
        .unwrap()
        .unwrap();
    let b = credentials::identities::for_node(&state.pool, &state.secrets, "alice", n2, &ca)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a, b);
    let p = preview(&state, "Overlap", &["alice"], &[n2]).await;
    assert_eq!(p["automatic_issue_count"], 0);
    let other_group = commit(&state, "Overlap", &["alice"], &[n2], &p)
        .await
        .unwrap();
    remove(&state, &first_group).await;
    let archived: bool =
        sqlx::query_scalar("SELECT archived FROM mtls_user_identities WHERE user_id='alice'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(!archived);
    assert_eq!(version_count(&state).await, 1);
    assert_eq!(
        a,
        credentials::identities::for_node(&state.pool, &state.secrets, "alice", n2, &ca)
            .await
            .unwrap()
            .unwrap()
    );
    remove(&state, &other_group).await;
    let archived: bool =
        sqlx::query_scalar("SELECT archived FROM mtls_user_identities WHERE user_id='alice'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(archived);
    assert_eq!(version_count(&state).await, 1);
    let _ = group(&state, "Again", &["alice"], &[n2]).await;
    assert_eq!(identity_count(&state).await, 1);
    assert_eq!(version_count(&state).await, 1);
    assert_eq!(
        a,
        credentials::identities::for_node(&state.pool, &state.secrets, "alice", n2, &ca)
            .await
            .unwrap()
            .unwrap()
    );
}

#[tokio::test]
async fn inactive_first_grant_is_pending_and_recovers_without_blocking_group_save() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "paused", false).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let p = preview(&state, "Pending", &["paused"], &[n]).await;
    assert_eq!(p["automatic_issue_count"], 0);
    assert_eq!(p["pending_issue_count"], 1);
    let result = commit(&state, "Pending", &["paused"], &[n], &p)
        .await
        .unwrap();
    assert_eq!(result["managed_identities"][0]["pending"], true);
    assert_eq!(identity_count(&state).await, 1);
    assert_eq!(version_count(&state).await, 0);
    sqlx::query("UPDATE users SET enabled=TRUE,revision=revision+1 WHERE id='paused'")
        .execute(&state.pool)
        .await
        .unwrap();
    credentials::maintenance::maintain(&state).await.unwrap();
    assert_eq!(version_count(&state).await, 1);
}

#[tokio::test]
async fn preparing_rotation_signs_two_generations_under_one_stable_shared_identity() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let _ = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "auth-rotation")),
    )
    .await
    .unwrap();
    let p = preview(&state, "During rotation", &["alice"], &[n]).await;
    assert_eq!(p["automatic_issue_count"], 1);
    assert_eq!(p["managed_identities"][0]["ca_versions"], json!([1, 2]));
    let _ = commit(&state, "During rotation", &["alice"], &[n], &p)
        .await
        .unwrap();
    assert_eq!(identity_count(&state).await, 1);
    assert_eq!(version_count(&state).await, 2);
    let versions: Vec<(i64, bool)> =
        sqlx::query_as("SELECT ca_version,released FROM mtls_user_versions ORDER BY ca_version")
            .fetch_all(&state.pool)
            .await
            .unwrap();
    assert_eq!(versions, vec![(1, true), (2, false)]);
    let identities:i64=sqlx::query_scalar("SELECT count(DISTINCT v.metadata->>'identity_id') FROM credential_versions v JOIN mtls_user_identities i ON i.credential_id=v.credential_id").fetch_one(&state.pool).await.unwrap();
    assert_eq!(identities, 1);
}

#[tokio::test]
async fn policy_change_invalidates_preview_and_never_commits_prepared_credentials_or_partial_permissions()
 {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let p = preview(&state, "Guarded", &["alice"], &[n]).await;
    let _ = update_policy(
        State(state.clone()),
        actor(),
        Path(ca),
        Json(UpdatePolicy {
            expected_revision: 1,
            policy: Policy {
                client_validity_days: 60,
                renew_before_hours: 24,
            },
            idempotency_key: "auth-policy".into(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        commit(&state, "Guarded", &["alice"], &[n], &p)
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(identity_count(&state).await, 0);
    assert_eq!(version_count(&state).await, 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM node_assignments")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn concurrent_previews_do_not_duplicate_current_identity() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let p1 = preview(&state, "First", &["alice"], &[n]).await;
    let p2 = preview(&state, "Second", &["alice"], &[n]).await;
    let nodes = [n];
    let (a, b) = tokio::join!(
        commit(&state, "First", &["alice"], &nodes, &p1),
        commit(&state, "Second", &["alice"], &nodes, &p2)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(identity_count(&state).await, 1);
    assert_eq!(version_count(&state).await, 1);
}

#[tokio::test]
async fn signatures_are_prepared_before_the_authorization_commit() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let p = preview(&state, "Outside lock", &["alice"], &[n]).await;
    let candidate = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        credentials::authorization::prepare_preview(
            &state.pool,
            &state.secrets,
            p["preview_token"].as_str().unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(candidate.plan.automatic_issue_count, 1);
    assert_eq!(version_count(&state).await, 0);
}

#[tokio::test]
async fn grant_covers_desired_deployed_and_inflight_activation_ca_identities() {
    use credentials::{activation, node_mtls::TrustVersion};
    let state = state().await;
    let old = generated(&state).await;
    let (_, Json(next)) = create(State(state.clone()), actor(), Json(request("target-ca")))
        .await
        .unwrap();
    let next = next["id"].as_str().unwrap();
    let (_, Json(inflight)) = create(State(state.clone()), actor(), Json(request("inflight-ca")))
        .await
        .unwrap();
    let inflight = inflight["id"].as_str().unwrap();
    user(&state, "alice", true).await;
    let node = managed_node(&state, &old).await;
    let id = node["id"].as_str().unwrap();
    sqlx::query(
        "UPDATE nodes SET deployed_config_enc=desired_config_enc,deployed_revision=1 WHERE id=$1",
    )
    .bind(id)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE mtls_node_trust SET deployed=TRUE WHERE node_id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
    let mut config = node["config"].clone();
    config["mtls"]["ca_credential_id"] = json!(inflight);
    let _ = crate::api::nodes::patch(
        State(state.clone()),
        Path(id.into()),
        Json(serde_json::from_value(json!({"expected_revision":1,"config":config})).unwrap()),
    )
    .await
    .unwrap();
    let job = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,available_at,created_at,updated_at) VALUES($1,'deploy',$2,2,'running','starting',now(),now(),now())").bind(&job).bind(id).execute(&state.pool).await.unwrap();
    let _ = activation::begin(
        &state.pool,
        &state.secrets,
        activation::Begin {
            node: id,
            job: &job,
            revision: 2,
            digest: "inflight-config",
            snapshot: &config,
            trust: &[TrustVersion {
                ca_id: inflight.into(),
                version: 1,
            }],
            probe: None,
        },
    )
    .await
    .unwrap();
    let archive = api::patch(
        State(state.clone()),
        actor(),
        Path(inflight.into()),
        Json(api::PatchCredential {
            expected_revision: 1,
            name: "Inflight CA".into(),
            archived: true,
            reminder_at: None,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(archive.status, StatusCode::CONFLICT);
    config["mtls"]["ca_credential_id"] = json!(next);
    let _ = crate::api::nodes::patch(
        State(state.clone()),
        Path(id.into()),
        Json(serde_json::from_value(json!({"expected_revision":2,"config":config})).unwrap()),
    )
    .await
    .unwrap();
    let p = preview(&state, "All contexts", &["alice"], &[id]).await;
    assert_eq!(p["automatic_issue_count"], 3);
    let _ = commit(&state, "All contexts", &["alice"], &[id], &p)
        .await
        .unwrap();
    assert_eq!(identity_count(&state).await, 3);
    assert_eq!(version_count(&state).await, 3);
    let details = crate::api::managed_mtls::user_identities(&state, "alice")
        .await
        .unwrap();
    assert!(
        details
            .iter()
            .all(|identity| identity["node_ids"] == json!([id]))
    );

    activation::verified(&state.pool, &job).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    activation::commit(&mut tx, &job, "inflight-config")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let archived: bool = sqlx::query_scalar(
        "SELECT archived FROM mtls_user_identities WHERE user_id='alice' AND ca_id=$1",
    )
    .bind(&old)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(archived);
}

#[tokio::test]
async fn failed_signing_preparation_does_not_commit_any_group_or_permissions() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    let p = preview(&state, "Atomic", &["alice"], &[id]).await;
    let bad = AppState::new(
        state.pool.clone(),
        SecretBox::from_base64("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE").unwrap(),
    );
    assert_eq!(
        commit(&bad, "Atomic", &["alice"], &[id], &p)
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    assert_eq!(identity_count(&state).await, 0);
    assert_eq!(version_count(&state).await, 0);
    let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM authorization_groups),(SELECT count(*) FROM node_assignments)").fetch_one(&state.pool).await.unwrap();
    assert_eq!(counts, (0, 0));
}

#[tokio::test]
async fn batch_grant_deduplicates_signatures_across_many_users_and_nodes() {
    let state = state().await;
    let ca = generated(&state).await;
    let mut users = Vec::new();
    let mut nodes = Vec::new();
    for index in 0..25 {
        let name = format!("batch-{index}");
        user(&state, &name, true).await;
        users.push(name);
    }
    for _ in 0..4 {
        nodes.push(
            managed_node(&state, &ca).await["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let users: Vec<&str> = users.iter().map(String::as_str).collect();
    let nodes: Vec<&str> = nodes.iter().map(String::as_str).collect();
    let p = preview(&state, "Batch", &users, &nodes).await;
    assert_eq!(p["additions_count"], 100);
    assert_eq!(p["automatic_issue_count"], 25);
    let _ = commit(&state, "Batch", &users, &nodes, &p).await.unwrap();
    assert_eq!(identity_count(&state).await, 25);
    assert_eq!(version_count(&state).await, 25);
}

#[tokio::test]
async fn ordinary_ssh_state_updates_do_not_invalidate_permission_preview() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    let p = preview(&state, "Offline grant", &["alice"], &[id]).await;
    sqlx::query("UPDATE nodes SET state='failed',last_seen_at=now() WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
    let _ = commit(&state, "Offline grant", &["alice"], &[id], &p)
        .await
        .unwrap();
    assert_eq!(identity_count(&state).await, 1);
}

#[tokio::test]
async fn retried_grant_preserves_one_prepared_managed_identity_and_version() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let p = preview(&state, "Retry grant", &["alice"], &[n]).await;
    sqlx::raw_sql("CREATE SEQUENCE managed_grant_attempts; CREATE FUNCTION retry_managed_grant_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='authorization_group.created' AND nextval('managed_grant_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_managed_grant_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_managed_grant_audit();").execute(&state.pool).await.unwrap();
    let result = commit(&state, "Retry grant", &["alice"], &[n], &p)
        .await
        .unwrap();
    assert_eq!(identity_count(&state).await, 1);
    assert_eq!(version_count(&state).await, 1);
    assert_eq!(result["managed_identities"][0]["issued"], true);
    let id: String =
        sqlx::query_scalar("SELECT id FROM mtls_user_identities WHERE user_id='alice'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(result["managed_identities"][0]["identity_id"], id);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE action='authorization_group.created'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM managed_grant_attempts")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
}
