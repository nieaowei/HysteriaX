use super::*;
use crate::{api::credentials as api, security::SecretBox};

async fn state() -> AppState {
    let pool = crate::db::test_pool_with_max_connections(4).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    crate::api::subscriptions::templates::initialize(&pool, &secrets)
        .await
        .unwrap();
    AppState::new(pool, secrets)
}
fn actor() -> Extension<AdminActor> {
    Extension(AdminActor {
        id: "ca-test-admin".into(),
    })
}
fn request(key: &str) -> CreateAuthority {
    CreateAuthority {
        name: "Shared CA".into(),
        source: Source::Generated,
        certificate: None,
        private_key: None,
        policy: Policy::default(),
        idempotency_key: key.into(),
    }
}
async fn generated(state: &AppState) -> String {
    let (_, Json(result)) = create(State(state.clone()), actor(), Json(request("generate")))
        .await
        .unwrap();
    result["id"].as_str().unwrap().into()
}
fn publish_request(revision: i64, key: &str) -> PublishAuthority {
    PublishAuthority {
        expected_revision: revision,
        certificate: None,
        private_key: None,
        replace_rotation_id: None,
        idempotency_key: key.into(),
    }
}

#[tokio::test]
async fn creation_is_idempotent_even_when_concurrent_and_never_returns_private_key() {
    let state = state().await;
    let (a, b) = tokio::join!(
        create(State(state.clone()), actor(), Json(request("same"))),
        create(State(state.clone()), actor(), Json(request("same")))
    );
    let (_, Json(a)) = a.unwrap();
    let (_, Json(b)) = b.unwrap();
    assert_eq!(a, b);
    let id = a["id"].as_str().unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM credentials WHERE kind='ca_identity'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let Json(detail) = api::get(State(state.clone()), Path(id.to_owned()))
        .await
        .unwrap();
    assert_eq!(detail["ca"]["published_version"], 1);
    assert_eq!(detail["ca"]["policy_revision"], 1);
    assert_eq!(detail["ca"]["source"], "generated");
    assert!(!detail.to_string().contains("PRIVATE KEY"));
    assert!(!detail.to_string().contains("private_key"));
    let mut collision = request("same");
    collision.name = "Different request".into();
    assert_eq!(
        create(State(state), actor(), Json(collision))
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn policy_uses_independent_revision_and_does_not_publish_key_or_deployment() {
    let state = state().await;
    let id = generated(&state).await;
    let make = || UpdatePolicy {
        expected_revision: 1,
        policy: Policy {
            client_validity_days: 30,
            renew_before_hours: 24,
        },
        idempotency_key: "policy".into(),
    };
    let Json(a) = update_policy(
        State(state.clone()),
        actor(),
        Path(id.clone()),
        Json(make()),
    )
    .await
    .unwrap();
    let Json(b) = update_policy(
        State(state.clone()),
        actor(),
        Path(id.clone()),
        Json(make()),
    )
    .await
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(a["policy_revision"], 2);
    let Json(detail) = api::get(State(state.clone()), Path(id.clone()))
        .await
        .unwrap();
    assert_eq!(detail["latest_version"], 1);
    assert_eq!(detail["revision"], 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let stale = UpdatePolicy {
        idempotency_key: "stale-policy".into(),
        ..make()
    };
    assert_eq!(
        update_policy(State(state.clone()), actor(), Path(id.clone()), Json(stale))
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    let invalid = UpdatePolicy {
        expected_revision: 2,
        policy: Policy {
            client_validity_days: 1,
            renew_before_hours: 24,
        },
        idempotency_key: "invalid-policy".into(),
    };
    assert_eq!(
        update_policy(State(state), actor(), Path(id), Json(invalid))
            .await
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn unused_ca_can_publish_repeatedly_without_waiting_for_old_ca_expiry() {
    let state = state().await;
    let id = generated(&state).await;
    for revision in 1..=2 {
        let key = format!("publish-{revision}");
        let (_, Json(result)) = publish(
            State(state.clone()),
            actor(),
            Path(id.clone()),
            Json(publish_request(revision, &key)),
        )
        .await
        .unwrap();
        assert_eq!(result["version"], revision + 1);
        assert_eq!(result["published"], true);
        let (_, Json(retry)) = publish(
            State(state.clone()),
            actor(),
            Path(id.clone()),
            Json(publish_request(revision, &key)),
        )
        .await
        .unwrap();
        assert_eq!(result, retry);
    }
    let generations: Vec<(i64, String)> = sqlx::query_as(
        "SELECT ca_version,state FROM mtls_ca_generations WHERE ca_id=$1 ORDER BY ca_version",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        generations,
        vec![
            (1, "retired".into()),
            (2, "retired".into()),
            (3, "published".into())
        ]
    );
    let err = api::publish(
        State(state.clone()),
        actor(),
        Path(id.clone()),
        Json(api::PublishVersion {
            expected_revision: 3,
            payload: json!({}),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    // An unused CA is deletable despite its own version/generation records.
    api::delete(
        State(state.clone()),
        actor(),
        Path(id.clone()),
        axum::extract::Query(crate::api::nodes::RevisionQuery {
            expected_revision: Some(3),
        }),
    )
    .await
    .unwrap();
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials WHERE id=$1)")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(!exists);
}

#[tokio::test]
async fn imported_ca_keeps_its_source_and_validates_material_before_persistence() {
    let state = state().await;
    let ca = Authority::generate(Utc::now()).unwrap();
    let payload = ca.payload();
    let request = CreateAuthority {
        name: "Imported CA".into(),
        source: Source::Imported,
        certificate: Some(payload["certificate"].as_str().unwrap().into()),
        private_key: Some(payload["private_key"].as_str().unwrap().into()),
        policy: Policy::default(),
        idempotency_key: "import".into(),
    };
    let (_, Json(result)) = create(State(state.clone()), actor(), Json(request))
        .await
        .unwrap();
    let id = result["id"].as_str().unwrap().to_owned();
    let err = publish(
        State(state.clone()),
        actor(),
        Path(id.clone()),
        Json(publish_request(1, "wrong-source")),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    let invalid = CreateAuthority {
        name: "Bad CA".into(),
        source: Source::Imported,
        certificate: Some("broken".into()),
        private_key: Some("broken".into()),
        policy: Policy::default(),
        idempotency_key: "bad-import".into(),
    };
    assert_eq!(
        create(State(state.clone()), actor(), Json(invalid))
            .await
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM credentials")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let audits: Vec<Value> = sqlx::query_scalar("SELECT detail_json FROM audit_records")
        .fetch_all(&state.pool)
        .await
        .unwrap();
    assert!(
        audits
            .iter()
            .all(|value| !value.to_string().contains("PRIVATE KEY"))
    );
}

async fn user(state: &AppState, id: &str, enabled: bool) {
    sqlx::query(
        "INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES($1,$1,$2,now(),now())",
    )
    .bind(id)
    .bind(enabled)
    .execute(&state.pool)
    .await
    .unwrap();
}
async fn issue(state: &AppState, user: &str, ca: &str, force: bool) -> (String, Option<i64>) {
    let candidate =
        credentials::identities::prepare_user(&state.pool, &state.secrets, user, ca, 1, force)
            .await
            .unwrap();
    let mut tx = db::begin_write(&state.pool).await.unwrap();
    let result = credentials::identities::commit_user(&mut tx, &state.secrets, candidate)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    result
}

#[tokio::test]
async fn shared_identity_reuses_current_material_and_reissue_never_changes_business_revision_or_kicks()
 {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    user(&state, "bob", true).await;
    let first = issue(&state, "alice", &ca, false).await;
    let repeat = issue(&state, "alice", &ca, false).await;
    assert_eq!(first, repeat);
    assert_eq!(first.1, Some(1));
    let bob = issue(&state, "bob", &ca, false).await;
    assert_ne!(first.0, bob.0);
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM users WHERE id='alice'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let expected: i64 = sqlx::query_scalar("SELECT revision FROM mtls_user_identities WHERE id=$1")
        .bind(&first.0)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let request = || ReissueUser {
        ca_credential_id: Some(ca.clone()),
        expected_revisions: [(ca.clone(), expected)].into(),
        idempotency_key: "reissue".into(),
    };
    let Json(a) = reissue_user(
        State(state.clone()),
        actor(),
        Path("alice".into()),
        Json(request()),
    )
    .await
    .unwrap();
    let Json(b) = reissue_user(
        State(state.clone()),
        actor(),
        Path("alice".into()),
        Json(request()),
    )
    .await
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(a["identities"][0]["credential_version"], 2);
    let after: i64 = sqlx::query_scalar("SELECT revision FROM users WHERE id='alice'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(revision, after);
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0);
    let versions: Vec<(i64,bool)>=sqlx::query_as("SELECT credential_version,released FROM mtls_user_versions WHERE identity_id=$1 ORDER BY credential_version").bind(&first.0).fetch_all(&state.pool).await.unwrap();
    assert_eq!(versions, vec![(1, true), (2, true)]);
    let Json(detail) = crate::api::users::get(State(state.clone()), Path("alice".into()))
        .await
        .unwrap();
    assert_eq!(detail["managed_certificates"][0]["ca_credential_id"], ca);
    assert_eq!(
        detail["managed_certificates"][0]["versions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(!detail.to_string().contains("PRIVATE KEY"));
    assert!(!detail.to_string().contains("private_key"));

    let credential: String =
        sqlx::query_scalar("SELECT credential_id FROM mtls_user_identities WHERE id=$1")
            .bind(&first.0)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let err = api::publish(
        State(state.clone()),
        actor(),
        Path(credential),
        Json(api::PublishVersion {
            expected_revision: 2,
            payload: json!({}),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    let references = api::find_references(&state, &ca).await.unwrap();
    assert!(references.iter().any(|r| r["source"] == "managed_identity"));
    let err = api::patch(
        State(state.clone()),
        actor(),
        Path(ca),
        Json(api::PatchCredential {
            expected_revision: 1,
            name: "CA".into(),
            archived: true,
            reminder_at: None,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn inactive_users_defer_issuance_then_recover_without_expiry_coupling() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "paused", false).await;
    let pending = issue(&state, "paused", &ca, false).await;
    assert_eq!(pending.1, None);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM mtls_user_versions")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    sqlx::query("UPDATE users SET enabled=TRUE,expires_at=now()+interval '1 hour',revision=revision+1 WHERE id='paused'").execute(&state.pool).await.unwrap();
    let recovered = issue(&state, "paused", &ca, false).await;
    assert_eq!(pending.0, recovered.0);
    assert_eq!(recovered.1, Some(1));
    let expires: chrono::DateTime<Utc> =
        sqlx::query_scalar("SELECT expires_at FROM mtls_user_versions WHERE identity_id=$1")
            .bind(&pending.0)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(expires > Utc::now() + Duration::days(179));
}

#[tokio::test]
async fn concurrent_preparations_discard_losing_candidates_and_policy_changes_reject_stale_signatures()
 {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let (a, b) = tokio::join!(
        credentials::identities::prepare_user(&state.pool, &state.secrets, "alice", &ca, 1, false),
        credentials::identities::prepare_user(&state.pool, &state.secrets, "alice", &ca, 1, false)
    );
    let mut tx = db::begin_write(&state.pool).await.unwrap();
    credentials::identities::commit_user(&mut tx, &state.secrets, a.unwrap())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = db::begin_write(&state.pool).await.unwrap();
    assert!(
        credentials::identities::commit_user(&mut tx, &state.secrets, b.unwrap())
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM mtls_user_identities")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let candidate =
        credentials::identities::prepare_user(&state.pool, &state.secrets, "alice", &ca, 1, true)
            .await
            .unwrap();
    let _ = update_policy(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(UpdatePolicy {
            expected_revision: 1,
            policy: Policy {
                client_validity_days: 60,
                renew_before_hours: 72,
            },
            idempotency_key: "change-policy".into(),
        }),
    )
    .await
    .unwrap();
    let mut tx = db::begin_write(&state.pool).await.unwrap();
    assert!(
        credentials::identities::commit_user(&mut tx, &state.secrets, candidate)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM mtls_user_versions")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

async fn node(state: &AppState, id: &str) {
    let mut tx = db::begin_write(&state.pool).await.unwrap();
    let ssh = credentials::insert(
        &mut tx,
        &state.secrets,
        "SSH",
        "ssh_password",
        None,
        &json!({"secret":"test-only"}),
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_credential_id,ssh_credential_version,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES($1,$1,'127.0.0.1',22,'root',$2,1,'example.test',443,':443',$1,$3,$3,$4,now(),now())")
        .bind(id).bind(ssh).bind(state.secrets.encrypt("test-only").unwrap()).bind(state.secrets.encrypt("{}").unwrap()).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
}
async fn trust(state: &AppState, node: &str, ca: &str, version: i64) {
    sqlx::query("INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,desired,deployed) VALUES($1,$2,$3,TRUE,TRUE)").bind(node).bind(ca).bind(version).execute(&state.pool).await.unwrap();
}

#[tokio::test]
async fn subscriptions_reuse_identical_material_for_same_ca_and_require_actual_deployed_trust() {
    use credentials::identities::for_node;
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    user(&state, "bob", true).await;
    node(&state, "n1").await;
    node(&state, "n2").await;
    trust(&state, "n1", &ca, 1).await;
    trust(&state, "n2", &ca, 1).await;
    issue(&state, "alice", &ca, false).await;
    issue(&state, "bob", &ca, false).await;
    let alice = for_node(&state.pool, &state.secrets, "alice", "n1", &ca)
        .await
        .unwrap()
        .unwrap();
    let other_node = for_node(&state.pool, &state.secrets, "alice", "n2", &ca)
        .await
        .unwrap()
        .unwrap();
    let bob = for_node(&state.pool, &state.secrets, "bob", "n1", &ca)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alice, other_node);
    assert_ne!(alice, bob);
    sqlx::query("UPDATE mtls_node_trust SET deployed=FALSE WHERE node_id='n2'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(
        for_node(&state.pool, &state.secrets, "alice", "n2", &ca)
            .await
            .unwrap()
            .is_none()
    );
    // Clock checks are performed in selection, independently of write events or cache state.
    sqlx::query("UPDATE mtls_user_versions SET expires_at=now()-interval '1 second',not_before=now()-interval '2 days' WHERE identity_id IN (SELECT id FROM mtls_user_identities WHERE user_id='alice')").execute(&state.pool).await.unwrap();
    assert!(
        for_node(&state.pool, &state.secrets, "alice", "n1", &ca)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn referenced_ca_publication_keeps_old_generation_until_all_nodes_are_prepared() {
    let state = state().await;
    let ca = generated(&state).await;
    node(&state, "n1").await;
    trust(&state, "n1", &ca, 1).await;
    let (_, Json(result)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "rotation")),
    )
    .await
    .unwrap();
    assert_eq!(result["published"], false);
    assert!(result["rotation_id"].is_string());
    let current: i64 =
        sqlx::query_scalar("SELECT published_version FROM mtls_authorities WHERE credential_id=$1")
            .bind(&ca)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(current, 1);
    let participants: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mtls_rotation_nodes WHERE rotation_id=$1 AND NOT trust_prepared",
    )
    .bind(result["rotation_id"].as_str().unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(participants, 1);
    let Json(batch) = api::batch(
        State(state.clone()),
        Path(result["batch_id"].as_str().unwrap().into()),
    )
    .await
    .unwrap();
    assert_eq!(batch["ca_rotation"]["phase"], "trust");
    assert_eq!(batch["ca_rotation"]["nodes"][0]["node_id"], "n1");

    let err = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(2, "second-before-ready")),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::CONFLICT);
    let err = api::delete(
        State(state.clone()),
        actor(),
        Path(ca),
        axum::extract::Query(crate::api::nodes::RevisionQuery {
            expected_revision: Some(2),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn maintenance_renews_once_pauses_inactive_and_recovers_pending_identities() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let original = issue(&state, "alice", &ca, false).await;
    sqlx::query("UPDATE mtls_user_versions SET next_action_at=now()-interval '1 second' WHERE identity_id=$1").bind(&original.0).execute(&state.pool).await.unwrap();
    sqlx::query(
        "UPDATE mtls_user_identities SET next_action_at=now()-interval '1 second' WHERE id=$1",
    )
    .bind(&original.0)
    .execute(&state.pool)
    .await
    .unwrap();
    credentials::maintenance::maintain(&state).await.unwrap();
    credentials::maintenance::maintain(&state).await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mtls_user_versions WHERE identity_id=$1")
            .bind(&original.0)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(count, 2);
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM users WHERE id='alice'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(revision, 1);
    sqlx::query("UPDATE users SET enabled=FALSE WHERE id='alice'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE mtls_user_identities SET next_action_at=now()-interval '1 second' WHERE id=$1",
    )
    .bind(&original.0)
    .execute(&state.pool)
    .await
    .unwrap();
    credentials::maintenance::maintain(&state).await.unwrap();
    let paused: bool =
        sqlx::query_scalar("SELECT next_action_at IS NULL FROM mtls_user_identities WHERE id=$1")
            .bind(&original.0)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(paused);
    user(&state, "pending", false).await;
    let pending = issue(&state, "pending", &ca, false).await;
    assert_eq!(pending.1, None);
    sqlx::query("UPDATE users SET enabled=TRUE WHERE id='pending'")
        .execute(&state.pool)
        .await
        .unwrap();
    credentials::maintenance::maintain(&state).await.unwrap();
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mtls_user_versions WHERE identity_id=$1")
            .bind(&pending.0)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn probe_lifetime_is_independent_of_users_and_ca_client_policy() {
    let state = state().await;
    let ca = generated(&state).await;
    let node_id = uuid::Uuid::new_v4().to_string();
    node(&state, &node_id).await;
    trust(&state, &node_id, &ca, 1).await;
    let _ = update_policy(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(UpdatePolicy {
            expected_revision: 1,
            policy: Policy {
                client_validity_days: 1,
                renew_before_hours: 1,
            },
            idempotency_key: "probe-policy".into(),
        }),
    )
    .await
    .unwrap();
    let (credential, version) = credentials::maintenance::renew_probe(&state, &node_id, &ca, 1)
        .await
        .unwrap();
    assert_eq!(version, 1);
    let expiry: chrono::DateTime<Utc> =
        sqlx::query_scalar("SELECT expires_at FROM mtls_probe_versions WHERE credential_id=$1")
            .bind(&credential)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(expiry > Utc::now() + Duration::days(6));
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(users, 0);
    let err = api::patch(
        State(state.clone()),
        actor(),
        Path(credential),
        Json(api::PatchCredential {
            expected_revision: 1,
            name: "Probe".into(),
            archived: true,
            reminder_at: None,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(&node_id)
        .execute(&state.pool)
        .await
        .unwrap();
    let retained: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mtls_probe_versions WHERE node_id=$1 AND expires_at>now()",
    )
    .bind(&node_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(retained, 1);
}

#[tokio::test]
async fn subscription_cache_expires_at_certificate_boundary_without_a_write_event() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    node(&state, "n1").await;
    trust(&state, "n1", &ca, 1).await;
    let (identity, _) = issue(&state, "alice", &ca, false).await;
    let config = state
        .secrets
        .encrypt(
            &json!({"mtls":{"mode":"managed","ca_credential_id":ca,"ca_credential_version":1}})
                .to_string(),
        )
        .unwrap();
    sqlx::query("UPDATE nodes SET deployed_revision=1,deployed_config_enc=$1,state='deployed' WHERE id='n1'").bind(config).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('alice','n1','password-hash',$1,now())").bind(state.secrets.encrypt("node-password").unwrap()).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO subscription_credentials(id,user_id,token_hash,token_enc,created_at) VALUES('sub','alice',$1,$2,now())").bind(crate::security::token_digest("test-token")).bind(state.secrets.encrypt("test-token").unwrap()).execute(&state.pool).await.unwrap();
    sqlx::query(
        "UPDATE mtls_user_versions SET expires_at=now()+interval '2 seconds' WHERE identity_id=$1",
    )
    .bind(&identity)
    .execute(&state.pool)
    .await
    .unwrap();
    async fn download(state: &AppState) -> String {
        let response = crate::api::subscriptions::download_auto(
            State(state.clone()),
            Path("test-token".into()),
            axum::extract::RawQuery(Some("format=mihomo&platform=linux".into())),
            axum::http::HeaderMap::new(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        String::from_utf8(
            axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }
    assert!(download(&state).await.contains("example.test"));
    assert!(download(&state).await.contains("example.test"));
    let generation =
        crate::cache_domains::version(&state.pool, &["credentials", "user_config", "node_config"])
            .await
            .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    assert_eq!(
        generation,
        crate::cache_domains::version(&state.pool, &["credentials", "user_config", "node_config"])
            .await
            .unwrap()
    );
    assert!(!download(&state).await.contains("example.test"));
}

#[tokio::test]
async fn ca_expiry_cap_reports_maintenance_error_without_publishing_endless_versions() {
    let state = state().await;
    let authority = Authority::generate(Utc::now() - Duration::days(3640)).unwrap();
    let payload = authority.payload();
    let (_, Json(result)) = create(
        State(state.clone()),
        actor(),
        Json(CreateAuthority {
            name: "Short CA".into(),
            source: Source::Imported,
            certificate: Some(payload["certificate"].as_str().unwrap().into()),
            private_key: Some(payload["private_key"].as_str().unwrap().into()),
            policy: Policy::default(),
            idempotency_key: "short-ca".into(),
        }),
    )
    .await
    .unwrap();
    let ca = result["id"].as_str().unwrap();
    user(&state, "alice", true).await;
    let identity = issue(&state, "alice", ca, false).await;
    for _ in 0..3 {
        credentials::maintenance::maintain(&state).await.unwrap();
    }
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mtls_user_versions WHERE identity_id=$1")
            .bind(&identity.0)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let error: Option<String> =
        sqlx::query_scalar("SELECT maintenance_error FROM mtls_user_identities WHERE id=$1")
            .bind(&identity.0)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(error.unwrap().contains("CA update required"));
}

#[tokio::test]
async fn deleting_user_archives_identity_but_retains_leaf_history_for_trust_cleanup() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let identity = issue(&state, "alice", &ca, false).await;
    crate::api::users::delete(
        State(state.clone()),
        Path("alice".into()),
        axum::extract::Query(crate::api::nodes::RevisionQuery {
            expected_revision: Some(1),
        }),
    )
    .await
    .unwrap();
    let (archived, paused): (bool, bool) = sqlx::query_as(
        "SELECT archived,next_action_at IS NULL FROM mtls_user_identities WHERE id=$1",
    )
    .bind(&identity.0)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(archived && paused);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mtls_user_versions WHERE identity_id=$1 AND expires_at>now()",
    )
    .bind(&identity.0)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
    let references = api::find_references(&state, &ca).await.unwrap();
    assert!(references.iter().any(|r| r["source"] == "managed_identity"));
    assert!(
        credentials::identities::prepare_user(&state.pool, &state.secrets, "alice", &ca, 1, true)
            .await
            .is_err()
    );
}

mod node_flow;

mod authorization_flow;

mod rotation_flow;

#[tokio::test]
async fn failed_ca_requests_are_audited_with_actor_and_without_input_material() {
    let state = state().await;
    let material = Authority::generate(Utc::now()).unwrap().payload();
    let canary = "private-material-canary-keep-out-of-audits";
    let invalid = CreateAuthority {
        name: "Invalid imported CA".into(),
        source: Source::Imported,
        certificate: Some(material["certificate"].as_str().unwrap().into()),
        private_key: Some(canary.into()),
        policy: Policy::default(),
        idempotency_key: "failed-import".into(),
    };
    let error = create(State(state.clone()), actor(), Json(invalid))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    let row=sqlx::query("SELECT actor,entity_type,detail_json FROM audit_records WHERE action='ca.create_request_failed'").fetch_one(&state.pool).await.unwrap();
    assert_eq!(row.get::<String, _>("actor"), "ca-test-admin");
    let detail: Value = row.get("detail_json");
    assert_eq!(detail["http_status"], 400);
    assert!(detail["reason"].as_str().unwrap().contains("private key"));
    assert!(!detail.to_string().contains(canary));
    assert!(
        !detail
            .to_string()
            .contains(material["certificate"].as_str().unwrap())
    );
    assert!(detail.get("private_key").is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credentials")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    let ca = generated(&state).await;
    let error = update_policy(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(UpdatePolicy {
            expected_revision: 99,
            policy: Policy::default(),
            idempotency_key: "stale-policy-audit".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    let row=sqlx::query("SELECT actor,entity_id,detail_json FROM audit_records WHERE action='ca.policy_request_failed'").fetch_one(&state.pool).await.unwrap();
    assert_eq!(row.get::<String, _>("entity_id"), ca);
    assert_eq!(row.get::<String, _>("actor"), "ca-test-admin");
    assert_eq!(
        row.get::<Value, _>("detail_json")["expected_policy_revision"],
        99
    );
    let redacted = audit::request::<(), _>(
        &state,
        &actor().0,
        "ca.create_request_failed",
        ("credential", "redaction-test"),
        json!({}),
        vec![canary.into()],
        async { Err(ApiError::bad_request(format!("signer failure: {canary}"))) },
    )
    .await
    .unwrap_err();
    assert!(!redacted.message.contains(canary));
    let detail: Value = sqlx::query_scalar(
        "SELECT detail_json FROM audit_records WHERE entity_id='redaction-test'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(detail["reason"].as_str().unwrap().contains("[redacted]"));
    let mut invalid_publish = publish_request(1, "failed-publication-audit");
    invalid_publish.private_key = Some(canary.into());
    assert_eq!(
        publish(
            State(state.clone()),
            actor(),
            Path(ca.clone()),
            Json(invalid_publish)
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT actor FROM audit_records WHERE action='ca.publish_request_failed'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        "ca-test-admin"
    );
    user(&state, "audit-user", true).await;
    let identity = issue(&state, "audit-user", &ca, false).await;
    let failed = reissue_user(
        State(state.clone()),
        actor(),
        Path("audit-user".into()),
        Json(ReissueUser {
            ca_credential_id: Some(ca.clone()),
            expected_revisions: [(ca.clone(), 99)].into(),
            idempotency_key: "failed-reissue-audit".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(failed.status, StatusCode::CONFLICT);
    let target: String = sqlx::query_scalar(
        "SELECT entity_type FROM audit_records WHERE action='ca.reissue_request_failed'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(target, "user");
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM mtls_user_identities WHERE id=$1")
        .bind(identity.0)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let _ = reissue_user(
        State(state.clone()),
        actor(),
        Path("audit-user".into()),
        Json(ReissueUser {
            ca_credential_id: Some(ca.clone()),
            expected_revisions: [(ca, revision)].into(),
            idempotency_key: "successful-reissue-audit".into(),
        }),
    )
    .await
    .unwrap();
    let target: String = sqlx::query_scalar(
        "SELECT entity_type FROM audit_records WHERE action='mtls.user_reissued'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(target, "user");
}

#[tokio::test]
async fn replacement_validity_is_not_the_thirty_day_new_binding_rule() {
    let state = state().await;
    let original = Authority::generate(Utc::now()).unwrap().payload();
    let (_, Json(created)) = create(
        State(state.clone()),
        actor(),
        Json(CreateAuthority {
            name: "Imported update lifetime".into(),
            source: Source::Imported,
            certificate: Some(original["certificate"].as_str().unwrap().into()),
            private_key: Some(original["private_key"].as_str().unwrap().into()),
            policy: Policy::default(),
            idempotency_key: "long-import".into(),
        }),
    )
    .await
    .unwrap();
    let ca = created["id"].as_str().unwrap().to_owned();
    let replacement = Authority::generate(Utc::now() - Duration::days(3630))
        .unwrap()
        .payload();
    let mut request = publish_request(1, "short-valid-replacement");
    request.certificate = Some(replacement["certificate"].as_str().unwrap().into());
    request.private_key = Some(replacement["private_key"].as_str().unwrap().into());
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(request),
    )
    .await
    .unwrap();
    assert_eq!(receipt["version"], 2);
    let settings = credentials::node_mtls::Settings::Managed {
        ca_credential_id: ca,
        ca_credential_version: 2,
    };
    assert!(
        credentials::node_mtls::validate_binding(&state.pool, &state.secrets, &settings, false)
            .await
            .is_ok()
    );
    assert!(
        credentials::node_mtls::validate_binding(&state.pool, &state.secrets, &settings, true)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn ca_display_status_tracks_published_generation_during_blocked_rotation() {
    let state = state().await;
    let original = Authority::generate(Utc::now() - Duration::days(3630))
        .unwrap()
        .payload();
    let (_, Json(created)) = create(
        State(state.clone()),
        actor(),
        Json(CreateAuthority {
            name: "Expiring published CA".into(),
            source: Source::Imported,
            certificate: Some(original["certificate"].as_str().unwrap().into()),
            private_key: Some(original["private_key"].as_str().unwrap().into()),
            policy: Policy::default(),
            idempotency_key: "display-original".into(),
        }),
    )
    .await
    .unwrap();
    let ca = created["id"].as_str().unwrap().to_owned();
    node(&state, "blocked-node").await;
    trust(&state, "blocked-node", &ca, 1).await;
    let replacement = Authority::generate(Utc::now()).unwrap().payload();
    let mut update = publish_request(1, "display-replacement");
    update.certificate = Some(replacement["certificate"].as_str().unwrap().into());
    update.private_key = Some(replacement["private_key"].as_str().unwrap().into());
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(update),
    )
    .await
    .unwrap();
    assert_eq!(receipt["published"], false);
    async fn check(state: &AppState, ca: &str, expected: &str) {
        let Json(detail) = api::get(State(state.clone()), Path(ca.into()))
            .await
            .unwrap();
        assert_eq!(detail["latest_version"], 2);
        assert_eq!(detail["status"], expected);
        let published = detail["ca"]["published_version"].as_i64().unwrap();
        let version = detail["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["version"] == published)
            .unwrap();
        assert_eq!(detail["metadata"], version["metadata"]);
        let Json(list) = api::list(
            State(state.clone()),
            axum::extract::Query(
                serde_json::from_value(json!({"kind":"ca_identity","sort":"status"})).unwrap(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(list["items"][0]["status"], expected);
        assert_eq!(list["items"][0]["expires_at"], detail["expires_at"]);
        let status: String =
            sqlx::query_scalar("SELECT status FROM credential_directory WHERE id=$1")
                .bind(ca)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(status, expected);
        let Json(summary) = crate::api::client_summary::get(State(state.clone()))
            .await
            .unwrap();
        assert_eq!(
            summary["notification_credentials"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == ca),
            expected == "expiring"
        );
    }
    check(&state, &ca, "expiring").await;
    // Only move the publication pointer: this verifies the live directory projection.
    sqlx::query("UPDATE mtls_authorities SET published_version=2 WHERE credential_id=$1")
        .bind(&ca)
        .execute(&state.pool)
        .await
        .unwrap();
    check(&state, &ca, "active").await;
}
