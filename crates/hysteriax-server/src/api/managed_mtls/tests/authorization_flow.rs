use super::node_flow::managed_node;
use super::*;
use crate::api::authorization_groups as groups;

async fn prepare(
    state: &AppState,
    _name: &str,
    users: &[&str],
    nodes: &[&str],
) -> credentials::authorization::Prepared {
    let mut tx = crate::read_snapshot::transaction(&state.pool)
        .await
        .unwrap();
    let owned_users: Vec<String> = users.iter().map(|s| s.to_string()).collect();
    let existing: std::collections::HashSet<(String,String)> = sqlx::query_as("SELECT gu.user_id,gn.node_id FROM authorization_group_users gu JOIN authorization_group_nodes gn ON gn.group_id=gu.group_id WHERE gu.user_id=ANY($1)")
        .bind(&owned_users).fetch_all(&mut *tx).await.unwrap().into_iter().collect();
    let additions: Vec<_> = users
        .iter()
        .flat_map(|user| {
            nodes
                .iter()
                .map(move |node| (user.to_string(), node.to_string()))
        })
        .filter(|pair| !existing.contains(pair))
        .collect();
    let plan = credentials::authorization::plan_on(&mut tx, &additions)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    credentials::authorization::prepare_plan(plan)
}
async fn commit(
    state: &AppState,
    name: &str,
    users: &[&str],
    nodes: &[&str],
    prepared: &credentials::authorization::Prepared,
) -> Result<Value, ApiError> {
    groups::create_prepared(
        state,
        serde_json::from_value(json!({"name":name,"user_ids":users,"node_ids":nodes})).unwrap(),
        prepared.clone(),
    )
    .await?;
    Ok(groups::group_named(state, name).await)
}
pub(super) async fn group(state: &AppState, name: &str, users: &[&str], nodes: &[&str]) -> Value {
    let _ = groups::create(
        State(state.clone()),
        Json(
            serde_json::from_value(json!({"name":name,"user_ids":users,"node_ids":nodes})).unwrap(),
        ),
    )
    .await
    .unwrap();
    settle(state).await;
    groups::group_named(state, name).await
}
async fn settle(state: &AppState) {
    for _ in 0..identity_count(state).await.max(1) {
        credentials::maintenance::issue_pending(state)
            .await
            .unwrap();
    }
}
async fn work_count(state: &AppState) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal'")
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn audit(state: &AppState, name: &str) -> Value {
    sqlx::query_scalar("SELECT detail_json FROM audit_records WHERE entity_id=(SELECT id FROM authorization_groups WHERE name=$1) ORDER BY created_at DESC LIMIT 1")
        .bind(name).fetch_one(&state.pool).await.unwrap()
}
async fn remove(state: &AppState, group: &Value) {
    let id = group["id"].as_str().unwrap();
    let revision = group["revision"].as_i64().unwrap();
    let _ = groups::delete(
        State(state.clone()),
        Path(id.into()),
        Json(serde_json::from_value(json!({"expected_revision":revision})).unwrap()),
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
async fn two_nodes_one_ca_issue_once_preparation_is_read_only_and_union_controls_archival() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let first = managed_node(&state, &ca).await;
    let second = managed_node(&state, &ca).await;
    let n1 = first["id"].as_str().unwrap();
    let n2 = second["id"].as_str().unwrap();
    let p = prepare(&state, "Both", &["alice"], &[n1, n2]).await;
    assert_eq!(p.plan.automatic_issue_count, 1);
    assert_eq!(p.plan.pending_issue_count, 0);
    assert_eq!(identity_count(&state).await, 0);
    assert_eq!(version_count(&state).await, 0);
    let first_group = commit(&state, "Both", &["alice"], &[n1, n2], &p)
        .await
        .unwrap();
    assert_eq!(identity_count(&state).await, 1);
    assert_eq!(version_count(&state).await, 0);
    assert_eq!(work_count(&state).await, 1);
    settle(&state).await;
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
    let p = prepare(&state, "Overlap", &["alice"], &[n2]).await;
    assert_eq!(p.plan.automatic_issue_count, 0);
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
    let p = prepare(&state, "Pending", &["paused"], &[n]).await;
    assert_eq!(p.plan.automatic_issue_count, 0);
    assert_eq!(p.plan.pending_issue_count, 1);
    let _result = commit(&state, "Pending", &["paused"], &[n], &p)
        .await
        .unwrap();
    assert_eq!(
        audit(&state, "Pending").await["managed_identities"][0]["pending"],
        true
    );
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
    let p = prepare(&state, "During rotation", &["alice"], &[n]).await;
    assert_eq!(p.plan.automatic_issue_count, 1);
    assert_eq!(p.plan.identities[0].ca_versions, vec![1, 2]);
    let _ = commit(&state, "During rotation", &["alice"], &[n], &p)
        .await
        .unwrap();
    assert_eq!(identity_count(&state).await, 1);
    assert_eq!(version_count(&state).await, 0);
    // Rotation issuance starts after node trust preparation has completed.
    sqlx::query("UPDATE mtls_rotations SET phase='identities',next_action_at=now()")
        .execute(&state.pool)
        .await
        .unwrap();
    settle(&state).await;
    credentials::rotations::run_pass(&state).await.unwrap();
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
async fn policy_change_rejects_stale_preparation_without_partial_permissions() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let p = prepare(&state, "Guarded", &["alice"], &[n]).await;
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
async fn concurrent_preparations_do_not_duplicate_current_identity() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let p1 = prepare(&state, "First", &["alice"], &[n]).await;
    let p2 = prepare(&state, "Second", &["alice"], &[n]).await;
    let nodes = [n];
    let (a, b) = tokio::join!(
        commit(&state, "First", &["alice"], &nodes, &p1),
        commit(&state, "Second", &["alice"], &nodes, &p2)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(identity_count(&state).await, 1);
    assert_eq!(version_count(&state).await, 0);
    assert_eq!(work_count(&state).await, 1);
    settle(&state).await;
    assert_eq!(version_count(&state).await, 1);
}

#[tokio::test]
async fn authorization_preparation_does_not_sign_or_create_work() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let p = prepare(&state, "Outside lock", &["alice"], &[n]).await;
    let candidate = p;
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
    let p = prepare(&state, "All contexts", &["alice"], &[id]).await;
    assert_eq!(p.plan.automatic_issue_count, 3);
    let _ = commit(&state, "All contexts", &["alice"], &[id], &p)
        .await
        .unwrap();
    assert_eq!(identity_count(&state).await, 3);
    assert_eq!(version_count(&state).await, 0);
    settle(&state).await;
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
async fn signing_failure_is_deferred_and_does_not_roll_back_permissions() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    let p = prepare(&state, "Async", &["alice"], &[id]).await;
    let _ = commit(&state, "Async", &["alice"], &[id], &p)
        .await
        .unwrap();
    assert_eq!(identity_count(&state).await, 1);
    assert_eq!(version_count(&state).await, 0);
    assert_eq!(work_count(&state).await, 1);
    let bad = AppState::new(
        state.pool.clone(),
        SecretBox::from_base64("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE").unwrap(),
    );
    credentials::maintenance::maintain(&bad).await.unwrap();
    assert_eq!(version_count(&state).await, 0);
    let error: String = sqlx::query_scalar("SELECT maintenance_error FROM mtls_user_identities")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(error, "Managed identity renewal failed; retry scheduled");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM node_assignments")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    sqlx::query("UPDATE mtls_user_identities SET next_action_at=now()")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET available_at=now()")
        .execute(&state.pool)
        .await
        .unwrap();
    settle(&state).await;
    assert_eq!(version_count(&state).await, 1);
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
    let p = prepare(&state, "Batch", &users, &nodes).await;
    assert_eq!(p.plan.automatic_issue_count, 25);
    let _ = commit(&state, "Batch", &users, &nodes, &p).await.unwrap();
    assert_eq!(identity_count(&state).await, 25);
    assert_eq!(version_count(&state).await, 0);
    assert_eq!(work_count(&state).await, 25);
    settle(&state).await;
    assert_eq!(version_count(&state).await, 25);
}

#[tokio::test]
async fn ordinary_ssh_state_updates_do_not_invalidate_prepared_grant() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    let p = prepare(&state, "Offline grant", &["alice"], &[id]).await;
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
async fn retried_grant_preserves_one_managed_identity_and_durable_task() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let p = prepare(&state, "Retry grant", &["alice"], &[n]).await;
    sqlx::raw_sql("CREATE SEQUENCE managed_grant_attempts; CREATE FUNCTION retry_managed_grant_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='authorization_group.created' AND nextval('managed_grant_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_managed_grant_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_managed_grant_audit();").execute(&state.pool).await.unwrap();
    let _result = commit(&state, "Retry grant", &["alice"], &[n], &p)
        .await
        .unwrap();
    assert_eq!(identity_count(&state).await, 1);
    assert_eq!(version_count(&state).await, 0);
    assert_eq!(work_count(&state).await, 1);
    assert_eq!(
        audit(&state, "Retry grant").await["managed_identities"][0]["issued"],
        false
    );
    let id: String =
        sqlx::query_scalar("SELECT id FROM mtls_user_identities WHERE user_id='alice'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        audit(&state, "Retry grant").await["managed_identities"][0]["identity_id"],
        id
    );
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

#[tokio::test]
async fn revoke_before_worker_runs_prevents_issuance() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let node_id = node["id"].as_str().unwrap();
    let empty = group(&state, "Direct", &[], &[node_id]).await;
    let id = empty["id"].as_str().unwrap();
    assert_eq!(identity_count(&state).await, 0);
    for (revision, users, additions, removals) in [(1, vec!["alice"], 1, 0), (2, vec![], 0, 1)] {
        let Json(receipt) = groups::update(State(state.clone()), Path(id.into()), Json(
            serde_json::from_value(json!({"expected_revision":revision,"name":"Direct","user_ids":users,"node_ids":[node_id]})).unwrap()
        )).await.unwrap();
        assert_eq!(receipt.as_object().unwrap().len(), 1);
        let detail = audit(&state, "Direct").await;
        assert_eq!(detail["additions_count"], additions);
        assert_eq!(detail["removals_count"], removals);
        assert_eq!(identity_count(&state).await, 1);
        assert_eq!(version_count(&state).await, 0);
        let archived: bool =
            sqlx::query_scalar("SELECT archived FROM mtls_user_identities WHERE user_id='alice'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(archived, users.is_empty());
    }
    settle(&state).await;
    assert_eq!(version_count(&state).await, 0);
}

#[tokio::test]
async fn paged_identity_admission_rolls_back_with_tasks_and_permissions() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let node_id = node["id"].as_str().unwrap();
    let users: Vec<String> = (0..260).map(|i| format!("member-{i:04}")).collect();
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) SELECT id,id,now(),now() FROM unnest($1::text[]) AS u(id)")
        .bind(&users).execute(&state.pool).await.unwrap();
    let names: Vec<&str> = users.iter().map(String::as_str).collect();
    let prepared = prepare(&state, "Paged", &names, &[node_id]).await;
    sqlx::raw_sql("CREATE SEQUENCE task_attempt; CREATE FUNCTION fail_task_page() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-identity-renewal' AND nextval('task_attempt')=257 THEN RAISE EXCEPTION 'task page failed'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_task_page BEFORE INSERT ON background_work FOR EACH ROW EXECUTE FUNCTION fail_task_page();")
        .execute(&state.pool).await.unwrap();
    assert!(
        commit(&state, "Paged", &names, &[node_id], &prepared)
            .await
            .is_err()
    );
    assert_eq!(identity_count(&state).await, 0);
    assert_eq!(work_count(&state).await, 0);
    let leftovers: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM authorization_groups)+(SELECT count(*) FROM node_assignments)")
        .fetch_one(&state.pool).await.unwrap();
    assert_eq!(leftovers, 0);
    sqlx::query("DROP TRIGGER fail_task_page ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    let _ = commit(&state, "Paged", &names, &[node_id], &prepared)
        .await
        .unwrap();
    assert_eq!(identity_count(&state).await, 260);
    assert_eq!(work_count(&state).await, 260);
    assert_eq!(version_count(&state).await, 0);
    let valid: i64 = sqlx::query_scalar("SELECT count(*) FROM background_work w JOIN mtls_user_identities i ON w.id='mtls-identity:'||i.id WHERE w.payload->>'identity_id'=i.id AND w.payload->>'user_id'=i.user_id AND w.payload->>'ca_id'=i.ca_id AND w.state='queued'")
        .fetch_one(&state.pool).await.unwrap();
    assert_eq!(valid, 260);
}

#[tokio::test]
async fn http_group_and_membership_grants_enqueue_once_and_resume_on_another_instance() {
    for membership in [false, true] {
        let state = state().await;
        let ca = generated(&state).await;
        user(&state, "alice", true).await;
        let node = managed_node(&state, &ca).await;
        let n = node["id"].as_str().unwrap();
        let group = group(&state, "HTTP async", &[], &[n]).await;
        let id = group["id"].as_str().unwrap();
        sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('async-admin',$1,'async',now(),now())")
            .bind(crate::security::token_digest("async-token")).execute(&state.pool).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api/v2", listener.local_addr().unwrap());
        let app = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, crate::api::router(app))
                .await
                .unwrap()
        });
        let client = reqwest::Client::new();
        let (method, path, body) = if membership {
            (
                reqwest::Method::PUT,
                "/users/alice/authorization-groups".to_string(),
                json!({"expected_revision":1,"group_ids":[id]}),
            )
        } else {
            (
                reqwest::Method::PATCH,
                format!("/authorization-groups/{id}"),
                json!({"expected_revision":1,"add_user_ids":["alice"]}),
            )
        };
        let mut receipt = None;
        for _ in 0..2 {
            let response = client
                .request(method.clone(), format!("{base}{path}"))
                .bearer_auth("async-token")
                .header("Idempotency-Key", "async-grant")
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let value: Value = response.json().await.unwrap();
            if let Some(previous) = &receipt {
                assert_eq!(previous, &value);
            }
            if membership {
                assert_eq!(value["managed_identities"][0]["issued"], false);
                assert_eq!(value["managed_identities"][0]["pending"], true);
            }
            receipt = Some(value);
        }
        assert_eq!(identity_count(&state).await, 1);
        assert_eq!(version_count(&state).await, 0);
        assert_eq!(work_count(&state).await, 1);
        server.abort();
        let resumed = AppState::new(state.pool.clone(), (*state.secrets).clone());
        settle(&resumed).await;
        assert_eq!(version_count(&state).await, 1);
        let done: i64 = sqlx::query_scalar("SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal' AND state='complete'")
            .fetch_one(&state.pool).await.unwrap();
        assert_eq!(done, 1);
    }
}
