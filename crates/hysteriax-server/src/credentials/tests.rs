use super::*;
use crate::{
    api::{AdminActor, credentials as api},
    state::AppState,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
};

const CERT: &str = include_str!("../../../../tests/fixtures/credentials-test.crt");
const KEY: &str = include_str!("../../../../tests/fixtures/credentials-test.key");

fn secrets() -> SecretBox {
    SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap()
}
async fn state() -> AppState {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    migration::migrate(&pool, &secrets()).await.unwrap();
    AppState::new(pool, secrets())
}
fn actor() -> Extension<AdminActor> {
    Extension(AdminActor {
        id: "test-admin".into(),
    })
}
async fn create(state: &AppState, kind: &str, payload: Value) -> String {
    let (_, Json(result)) = api::create(
        State(state.clone()),
        actor(),
        Json(api::CreateCredential {
            name: format!("Test {kind}"),
            kind: kind.into(),
            owner_user_id: None,
            reminder_at: None,
            payload,
        }),
    )
    .await
    .unwrap();
    result["id"].as_str().unwrap().into()
}
async fn node(state: &AppState, name: &str, ssh: &str, config: Value) -> String {
    let (_,Json(result))=crate::api::nodes::create(State(state.clone()),Json(serde_json::from_value(json!({"name":name,"ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_credential_id":ssh,"ssh_credential_version":1,"public_host":"credentials.example.test","public_port":443,"listen_addr":":443","config":config})).unwrap())).await.unwrap();
    result["node"]["id"].as_str().unwrap().into()
}

#[test]
fn references_reject_invalid_paths_and_replace_only_the_selected_identity() {
    for value in [
        "credential://a/1/key",
        "credential://00000000-0000-0000-0000-000000000000/0/key",
        "credential://00000000-0000-0000-0000-000000000000/1/../key",
    ] {
        assert!(Reference::parse(value).is_err());
    }
    let first = Uuid::new_v4().to_string();
    let second = Uuid::new_v4().to_string();
    let mut value = json!({"tls":{"cert":Reference{id:first.clone(),version:1,field:"certificate".into()}.uri(),"key":Reference{id:second.clone(),version:1,field:"content".into()}.uri()}});
    assert!(replace_version(&mut value, &first, 2).unwrap());
    let refs = references(&value).unwrap();
    assert_eq!(
        refs.iter().find(|(_, r)| r.id == first).unwrap().1.version,
        2
    );
    assert_eq!(
        refs.iter().find(|(_, r)| r.id == second).unwrap().1.version,
        1
    );
}

#[test]
fn identity_validation_extracts_public_metadata_and_rejects_invalid_secrets() {
    let metadata = validate_payload(
        "tls_identity",
        &json!({"certificate":CERT,"private_key":KEY}),
    )
    .unwrap();
    assert_eq!(
        metadata["domains"],
        json!(["credentials.example.test", "other.example.test"])
    );
    assert!(metadata["expires_at"].is_string());
    assert!(!metadata.to_string().contains("PRIVATE KEY"));
    assert!(
        validate_payload(
            "tls_identity",
            &json!({"certificate":CERT,"private_key":"invalid"})
        )
        .is_err()
    );
    assert!(validate_payload("ssh_private_key", &json!({"secret":"invalid"})).is_err());
    assert!(
        validate_payload(
            "ssh_password",
            &json!({"secret":"password","unexpected":"value"})
        )
        .is_err()
    );
    assert!(validate_payload("dns", &json!({"provider":"cloudflare","config":{}})).is_err());
}

#[tokio::test]
async fn dns_tokens_with_quotes_and_newlines_never_appear_in_node_preview() {
    let state = state().await;
    let ssh = create(&state, "ssh_password", json!({"secret":"unused"})).await;
    let token = "test-token\nwith-'quotes'-and-\\backslash";
    let dns = create(
        &state,
        "dns",
        json!({"provider":"cloudflare","config":{"cloudflare_api_token":token}}),
    )
    .await;
    let config = json!({"acme":{"domains":["credentials.example.test"],"type":"dns","dns":{"name":"cloudflare","config":{"cloudflare_api_token":Reference{id:dns,version:1,field:"cloudflare_api_token".into()}.uri()}}}});
    let id = node(&state, "DNS node", &ssh, config).await;
    let Json(detail) = crate::api::nodes::get(State(state.clone()), Path(id))
        .await
        .unwrap();
    let preview: Value = serde_yaml::from_str(detail["yaml_preview"].as_str().unwrap()).unwrap();
    assert_eq!(
        preview
            .pointer("/acme/dns/config/cloudflare_api_token")
            .unwrap(),
        "[redacted]"
    );
    assert!(!detail.to_string().contains("test-token"));
}

#[tokio::test]
async fn imports_never_return_secrets_and_versions_are_database_immutable() {
    let state = state().await;
    let secret = "credential-api-token-do-not-expose";
    let id = create(&state, "api_token", json!({"token":secret})).await;
    let Json(list) = api::list(State(state.clone())).await.unwrap();
    let Json(detail) = api::get(State(state.clone()), Path(id.clone()))
        .await
        .unwrap();
    assert!(!json!(list).to_string().contains(secret));
    assert!(!detail.to_string().contains(secret));
    let cipher: String =
        sqlx::query_scalar("SELECT payload_enc FROM credential_versions WHERE credential_id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(!cipher.contains(secret));
    assert_eq!(
        serde_json::from_str::<Value>(&state.secrets.decrypt(&cipher).unwrap()).unwrap()["token"],
        secret
    );
    assert!(
        sqlx::query("UPDATE credential_versions SET metadata='{}' WHERE credential_id=$1")
            .bind(&id)
            .execute(&state.pool)
            .await
            .is_err()
    );
    let (_, Json(published)) = api::publish(
        State(state.clone()),
        actor(),
        Path(id.clone()),
        Json(api::PublishVersion {
            expected_revision: 1,
            payload: json!({"token":"new-token"}),
        }),
    )
    .await
    .unwrap();
    assert_eq!(published["version"], 2);
    assert_eq!(published["affected_count"], 0);
    assert_eq!(
        api::publish(
            State(state.clone()),
            actor(),
            Path(id),
            Json(api::PublishVersion {
                expected_revision: 1,
                payload: json!({"token":"stale-token"})
            })
        )
        .await
        .unwrap_err()
        .status,
        axum::http::StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn publication_updates_all_references_and_preserves_pinned_history() {
    let state = state().await;
    let ssh = create(&state, "ssh_password", json!({"secret":"unused-password"})).await;
    let identity = create(
        &state,
        "tls_identity",
        json!({"certificate":CERT,"private_key":KEY}),
    )
    .await;
    let config = json!({"tls":{"cert":Reference{id:identity.clone(),version:1,field:"certificate".into()}.uri(),"key":Reference{id:identity.clone(),version:1,field:"private_key".into()}.uri()}});
    let first = node(&state, "First", &ssh, config.clone()).await;
    let second = node(&state, "Second", &ssh, config).await;
    let (_, Json(published)) = api::publish(
        State(state.clone()),
        actor(),
        Path(identity.clone()),
        Json(api::PublishVersion {
            expected_revision: 1,
            payload: json!({"certificate":CERT,"private_key":KEY}),
        }),
    )
    .await
    .unwrap();
    assert_eq!(published["affected_count"], 2);
    let jobs=sqlx::query("SELECT id,node_id,payload_json FROM jobs WHERE kind='credential-apply' ORDER BY created_at").fetch_all(&state.pool).await.unwrap();
    for row in jobs {
        let job = crate::deployment::JobInput {
            id: row.get("id"),
            node_id: row.get("node_id"),
            payload: row.get("payload_json"),
            kind: "credential-apply".into(),
            target_revision: None,
            attempts: 1,
        };
        let result = worker::apply(&state.pool, &state.secrets, &job)
            .await
            .unwrap();
        assert_eq!(result.stage, "credential_deployment_queued");
        // A crash after the transactional handoff but before jobs::succeed
        // must acknowledge the existing follow-up, not enqueue it twice.
        let recovered = worker::apply(&state.pool, &state.secrets, &job)
            .await
            .unwrap();
        assert_eq!(recovered.result["already_applied"], true);
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM config_versions WHERE node_id=$1")
                .bind(job.node_id.as_deref())
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(count, 2);
    }
    for node in [first, second] {
        let cipher: String = sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let current: Value =
            serde_json::from_str(&state.secrets.decrypt(&cipher).unwrap()).unwrap();
        assert!(
            references(&current)
                .unwrap()
                .iter()
                .all(|(_, r)| r.version == 2)
        );
        let cipher: String = sqlx::query_scalar(
            "SELECT config_enc FROM config_versions WHERE node_id=$1 AND revision=1",
        )
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        let old: Value = serde_json::from_str(&state.secrets.decrypt(&cipher).unwrap()).unwrap();
        assert!(
            references(&old)
                .unwrap()
                .iter()
                .all(|(_, r)| r.version == 1)
        );
    }
    let Json(batch) = api::batch(
        State(state.clone()),
        Path(published["batch_id"].as_str().unwrap().into()),
    )
    .await
    .unwrap();
    assert!(
        batch["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["status"] == "queued")
    );
    assert_eq!(
        api::delete(
            State(state.clone()),
            actor(),
            Path(identity),
            axum::extract::Query(crate::api::nodes::RevisionQuery {
                expected_revision: Some(2)
            })
        )
        .await
        .unwrap_err()
        .status,
        axum::http::StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn concurrent_node_edits_and_newer_publications_prevent_stale_application() {
    let state = state().await;
    let ssh = create(&state, "ssh_password", json!({"secret":"old"})).await;
    let node = node(&state, "Node", &ssh, json!({})).await;
    let _ = api::publish(
        State(state.clone()),
        actor(),
        Path(ssh.clone()),
        Json(api::PublishVersion {
            expected_revision: 1,
            payload: json!({"secret":"new"}),
        }),
    )
    .await
    .unwrap();
    let row = sqlx::query("SELECT id,node_id,payload_json FROM jobs WHERE kind='credential-apply'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let job = crate::deployment::JobInput {
        id: row.get("id"),
        node_id: row.get("node_id"),
        payload: row.get("payload_json"),
        kind: "credential-apply".into(),
        target_revision: None,
        attempts: 1,
    };
    sqlx::query("UPDATE nodes SET desired_revision=2 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let error = worker::apply(&state.pool, &state.secrets, &job)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("node changed"));
    let _ = api::publish(
        State(state.clone()),
        actor(),
        Path(ssh),
        Json(api::PublishVersion {
            expected_revision: 2,
            payload: json!({"secret":"newest"}),
        }),
    )
    .await
    .unwrap();
    assert!(
        worker::apply(&state.pool, &state.secrets, &job)
            .await
            .unwrap_err()
            .to_string()
            .contains("superseded")
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT ssh_credential_version FROM nodes WHERE id=$1")
            .bind(node)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn restart_acknowledges_committed_ssh_application_without_reconnecting_or_reverting() {
    let state = state().await;
    let ssh = create(&state, "ssh_password", json!({"secret":"old-password"})).await;
    let node = node(&state, "SSH recovery node", &ssh, json!({})).await;
    let (_, Json(published)) = api::publish(
        State(state.clone()),
        actor(),
        Path(ssh.clone()),
        Json(api::PublishVersion {
            expected_revision: 1,
            payload: json!({"secret":"replacement-password"}),
        }),
    )
    .await
    .unwrap();
    let row = sqlx::query("SELECT id,node_id,payload_json FROM jobs WHERE kind='credential-apply'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let job = crate::deployment::JobInput {
        id: row.get("id"),
        node_id: row.get("node_id"),
        payload: row.get("payload_json"),
        kind: "credential-apply".into(),
        target_revision: None,
        attempts: 2,
    };
    // Inject the durable state of a completed SSH transaction followed by a
    // process crash before the final job result was persisted.
    let mut tx = crate::db::begin_write(&state.pool).await.unwrap();
    sqlx::query("UPDATE nodes SET ssh_credential_version=2,desired_revision=2 WHERE id=$1")
        .bind(&node)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE credential_batch_items SET applied_at=now(),apply_stage='credential_applied' WHERE batch_id=$1").bind(published["batch_id"].as_str().unwrap()).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let _ = api::publish(
        State(state.clone()),
        actor(),
        Path(ssh),
        Json(api::PublishVersion {
            expected_revision: 2,
            payload: json!({"secret":"newest-password"}),
        }),
    )
    .await
    .unwrap();
    let result = worker::apply(&state.pool, &state.secrets, &job)
        .await
        .unwrap();
    assert_eq!(result.result["already_applied"], true);
    assert_eq!(result.stage, "credential_applied");
    let row = sqlx::query("SELECT ssh_credential_version,desired_revision FROM nodes WHERE id=$1")
        .bind(node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("ssh_credential_version"), 2);
    assert_eq!(row.get::<i64, _>("desired_revision"), 2);
}

#[tokio::test]
async fn migration_preserves_existing_tokens_and_rolls_back_bad_ciphertext() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let secrets = secrets();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES('legacy','Legacy','127.0.0.1',22,'root','password',$1,'example.test',443,':443','node-hash',$2,$3,$4,now(),now())").bind(secrets.encrypt("old-password").unwrap()).bind(secrets.encrypt("existing-node-token").unwrap()).bind("bad-ciphertext").bind(secrets.encrypt("{}").unwrap()).execute(&pool).await.unwrap();
    assert!(migration::migrate(&pool, &secrets).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM credentials")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM credential_migrations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("UPDATE nodes SET traffic_stats_secret_enc=$1 WHERE id='legacy'")
        .bind(secrets.encrypt("existing-stats-token").unwrap())
        .execute(&pool)
        .await
        .unwrap();
    migration::migrate(&pool, &secrets).await.unwrap();
    migration::migrate(&pool, &secrets).await.unwrap();
    let row = sqlx::query("SELECT * FROM nodes WHERE id='legacy'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        super::ssh_node(&pool, &secrets, &row).await.unwrap().secret,
        "old-password"
    );
    assert_eq!(
        secrets
            .decrypt(&row.get::<String, _>("node_token_enc"))
            .unwrap(),
        "existing-node-token"
    );
    assert!(row.try_get::<String, _>("ssh_secret_enc").is_err());
}

#[tokio::test]
async fn v1_http_contract_authentication_and_current_token_protection() {
    let state = state().await;
    for (id, token) in [
        ("admin-one", "first-administrator-token"),
        ("admin-two", "second-administrator-token"),
    ] {
        sqlx::query(
            "INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES($1,$2,$1,now())",
        )
        .bind(id)
        .bind(crate::security::token_digest(token))
        .execute(&state.pool)
        .await
        .unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = crate::api::router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{base}/api/v2/version"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/credentials"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let version: Value = client
        .get(format!("{base}/api/v1/version"))
        .bearer_auth("first-administrator-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(version["api_version"], "1.0.0");
    assert_eq!(version["current_admin_token_id"], "admin-one");
    assert_eq!(
        client
            .delete(format!("{base}/api/v1/admin/tokens/admin-one"))
            .bearer_auth("first-administrator-token")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::CONFLICT
    );
    assert_eq!(
        client
            .delete(format!("{base}/api/v1/admin/tokens/admin-two"))
            .bearer_auth("first-administrator-token")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let result=client.post(format!("{base}/api/v1/credentials")).bearer_auth("first-administrator-token").json(&json!({"name":"HTTP token","kind":"api_token","payload":{"token":"secret-never-returned"}})).send().await.unwrap();
    assert_eq!(result.status(), reqwest::StatusCode::CREATED);
    let receipt: Value = result.json().await.unwrap();
    let detail = client
        .get(format!(
            "{base}/api/v1/credentials/{}",
            receipt["id"].as_str().unwrap()
        ))
        .bearer_auth("first-administrator-token")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!detail.contains("secret-never-returned"));
    let actor: String = sqlx::query_scalar(
        "SELECT actor FROM audit_records WHERE entity_id=$1 AND action='credential.created'",
    )
    .bind(receipt["id"].as_str().unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(actor, "admin-one");
    let used: bool = sqlx::query_scalar(
        "SELECT last_used_at IS NOT NULL FROM admin_tokens WHERE id='admin-one'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(used);
    server.abort();
}

#[tokio::test]
async fn user_identity_scope_archiving_and_unreferenced_deletion_are_enforced() {
    let state = state().await;
    let (_, Json(owner)) = crate::api::users::create(
        State(state.clone()),
        Json(serde_json::from_value(json!({"name":"Identity owner"})).unwrap()),
    )
    .await
    .unwrap();
    let (_, Json(other)) = crate::api::users::create(
        State(state.clone()),
        Json(serde_json::from_value(json!({"name":"Other user"})).unwrap()),
    )
    .await
    .unwrap();
    let owner = owner["id"].as_str().unwrap().to_owned();
    let other = other["id"].as_str().unwrap().to_owned();
    let (_, Json(identity)) = api::create(
        State(state.clone()),
        actor(),
        Json(api::CreateCredential {
            name: "Owned mTLS".into(),
            kind: "tls_identity".into(),
            owner_user_id: Some(owner.clone()),
            reminder_at: None,
            payload: json!({"certificate":CERT,"private_key":KEY}),
        }),
    )
    .await
    .unwrap();
    let identity = identity["id"].as_str().unwrap().to_owned();
    let ssh = create(&state, "ssh_password", json!({"secret":"unused"})).await;
    let node = node(&state, "Scope node", &ssh, json!({})).await;
    let assignment = |user: String| {
        let state = state.clone();
        let node = node.clone();
        let identity = identity.clone();
        async move {
            let draft = json!({
                "action":"create",
                "name":format!("mTLS {user}"),
                "user_ids":[user],
                "node_ids":[node],
                "mtls_bindings":[{"user_id":user,"node_id":node,"credential_id":identity,"credential_version":1}],
            });
            let Json(preview) = crate::api::authorization_groups::preview_create(
                State(state.clone()),
                Json(serde_json::from_value(draft.clone()).unwrap()),
            )
            .await?;
            let mut request = draft;
            request.as_object_mut().unwrap().remove("action");
            request["preview_token"] = preview["preview_token"].clone();
            crate::api::authorization_groups::create(
                State(state),
                Json(serde_json::from_value(request).unwrap()),
            )
            .await
        }
    };
    assert_eq!(
        assignment(other.clone()).await.unwrap_err().status,
        axum::http::StatusCode::BAD_REQUEST
    );
    let _ = assignment(owner.clone()).await.unwrap();
    let config = json!({"tls":{"cert":Reference{id:identity.clone(),version:1,field:"certificate".into()}.uri(),"key":Reference{id:identity.clone(),version:1,field:"private_key".into()}.uri()}});
    assert!(
        super::resolve_config(&state.pool, &state.secrets, &config, true)
            .await
            .is_err()
    );
    let _ = api::patch(
        State(state.clone()),
        actor(),
        Path(identity.clone()),
        Json(api::PatchCredential {
            expected_revision: 1,
            name: "Owned mTLS".into(),
            archived: true,
            reminder_at: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        api::publish(
            State(state.clone()),
            actor(),
            Path(identity.clone()),
            Json(api::PublishVersion {
                expected_revision: 2,
                payload: json!({"certificate":CERT,"private_key":KEY})
            })
        )
        .await
        .unwrap_err()
        .status,
        axum::http::StatusCode::CONFLICT
    );
    let existing=sqlx::query("SELECT user_id,mtls_credential_id,mtls_credential_version FROM node_assignments WHERE user_id=$1").bind(owner).fetch_one(&state.pool).await.unwrap();
    assert!(
        super::assignment_identity(&state.pool, &state.secrets, &existing)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        assignment(other).await.unwrap_err().status,
        axum::http::StatusCode::BAD_REQUEST
    );
    assert_eq!(
        api::delete(
            State(state.clone()),
            actor(),
            Path(identity),
            axum::extract::Query(crate::api::nodes::RevisionQuery {
                expected_revision: Some(2)
            })
        )
        .await
        .unwrap_err()
        .status,
        axum::http::StatusCode::CONFLICT
    );
    let unused = create(&state, "api_token", json!({"token":"unused-secret"})).await;
    let _ = api::publish(
        State(state.clone()),
        actor(),
        Path(unused.clone()),
        Json(api::PublishVersion {
            expected_revision: 1,
            payload: json!({"token":"unused-replacement"}),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        api::delete(
            State(state.clone()),
            actor(),
            Path(unused.clone()),
            axum::extract::Query(crate::api::nodes::RevisionQuery {
                expected_revision: Some(2)
            })
        )
        .await
        .unwrap(),
        axum::http::StatusCode::NO_CONTENT
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM credential_versions WHERE credential_id=$1"
        )
        .bind(unused)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
}

#[test]
fn only_certificate_pairs_and_ca_credentials_are_accepted() {
    assert!(validate_payload("certificate", &json!({"content":CERT})).is_err());
    assert!(validate_payload("private_key", &json!({"content":KEY})).is_err());
    assert!(validate_payload("ca_certificate", &json!({"content":CERT})).is_ok());
    let first = Uuid::new_v4().to_string();
    let second = Uuid::new_v4().to_string();
    let mixed = json!({"tls":{"cert":Reference{id:first.clone(),version:1,field:"certificate".into()}.uri(),"key":Reference{id:second,version:1,field:"private_key".into()}.uri()}});
    assert!(require_managed_config(&mixed).is_err());
    let paired = json!({"tls":{"cert":Reference{id:first.clone(),version:1,field:"certificate".into()}.uri(),"key":Reference{id:first,version:1,field:"private_key".into()}.uri()}});
    assert!(require_managed_config(&paired).is_ok());
}

async fn legacy_pair_fixture(bad_key: bool) -> (AppState, String, Value) {
    let state = state().await;
    sqlx::query("DELETE FROM credential_migrations WHERE name='tls_pairs_v1'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE credentials DROP CONSTRAINT credentials_kind_check")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE credentials ADD CONSTRAINT credentials_kind_check CHECK (kind IN ('ssh_private_key','ssh_password','tls_identity','ca_certificate','certificate','private_key','ech_key','dns','api_token'))").execute(&state.pool).await.unwrap();
    let mut tx = crate::db::begin_write(&state.pool).await.unwrap();
    let cert = insert(
        &mut tx,
        &state.secrets,
        "Old cert",
        "certificate",
        None,
        &json!({"content":CERT}),
        &certificate_metadata(CERT).unwrap(),
        &json!({"content":"old-cert-file"}),
    )
    .await
    .unwrap();
    let key = insert(
        &mut tx,
        &state.secrets,
        "Old key",
        "private_key",
        None,
        &json!({"content":if bad_key {"invalid-key"} else {KEY}}),
        &json!({}),
        &json!({"content":"old-key-file"}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let ssh = create(&state, "ssh_password", json!({"secret":"unchanged"})).await;
    let id = node(&state, "Migration pair", &ssh, json!({})).await;
    let config = json!({"tls":{"cert":Reference{id:cert.clone(),version:1,field:"content".into()}.uri(),"key":Reference{id:key,version:1,field:"content".into()}.uri(),"clientCA":Reference{id:cert,version:1,field:"content".into()}.uri()}});
    let cipher = state.secrets.encrypt(&config.to_string()).unwrap();
    sqlx::query("UPDATE nodes SET desired_config_enc=$1,deployed_config_enc=$1 WHERE id=$2")
        .bind(&cipher)
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    let snapshot = json!({"server_config":config});
    sqlx::query("UPDATE config_versions SET config_enc=$1,content_sha256=$2 WHERE node_id=$3")
        .bind(state.secrets.encrypt(&snapshot.to_string()).unwrap())
        .bind(hex::encode(Sha256::digest(snapshot.to_string().as_bytes())))
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    (state, id, config)
}

#[tokio::test]
async fn tls_pair_migration_preserves_material_artifacts_history_and_is_idempotent() {
    let (state, id, _) = legacy_pair_fixture(false).await;
    migration::migrate_tls_pairs(&state.pool, &state.secrets)
        .await
        .unwrap();
    let row = sqlx::query("SELECT * FROM nodes WHERE id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let desired: Value = serde_json::from_str(
        &state
            .secrets
            .decrypt(&row.get::<String, _>("desired_config_enc"))
            .unwrap(),
    )
    .unwrap();
    require_managed_config(&desired).unwrap();
    let cert = Reference::parse(desired["tls"]["cert"].as_str().unwrap()).unwrap();
    let key = Reference::parse(desired["tls"]["key"].as_str().unwrap()).unwrap();
    assert_eq!(
        (cert.id.clone(), cert.version),
        (key.id.clone(), key.version)
    );
    let (kind, _, _, payload, artifacts) =
        load(&state.pool, &state.secrets, &cert.id, cert.version)
            .await
            .unwrap();
    assert_eq!(kind, "tls_identity");
    assert_eq!(payload, json!({"certificate":CERT,"private_key":KEY}));
    assert_eq!(
        artifacts,
        json!({"certificate":"old-cert-file","private_key":"old-key-file"})
    );
    let deployed: Value = serde_json::from_str(
        &state
            .secrets
            .decrypt(&row.get::<String, _>("deployed_config_enc"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(desired, deployed);
    assert_eq!(row.get::<i64, _>("desired_revision"), 1);
    for cipher in
        sqlx::query_scalar::<_, String>("SELECT config_enc FROM config_versions WHERE node_id=$1")
            .bind(&id)
            .fetch_all(&state.pool)
            .await
            .unwrap()
    {
        let snapshot: Value =
            serde_json::from_str(&state.secrets.decrypt(&cipher).unwrap()).unwrap();
        assert_eq!(snapshot["server_config"], desired);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM credentials WHERE kind IN ('certificate','private_key')"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
    let count = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credentials")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    migration::migrate_tls_pairs(&state.pool, &state.secrets)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credentials")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        count
    );
}

#[tokio::test]
async fn invalid_legacy_pair_rolls_back_without_dropping_material() {
    let (state, id, original) = legacy_pair_fixture(true).await;
    assert!(
        migration::migrate_tls_pairs(&state.pool, &state.secrets)
            .await
            .is_err()
    );
    let cipher: String = sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&state.secrets.decrypt(&cipher).unwrap()).unwrap(),
        original
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credentials WHERE kind='private_key'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credentials WHERE kind='tls_identity'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}
