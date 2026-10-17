use super::*;
use credentials::{
    activation,
    node_mtls::{self, Settings},
};

async fn node_request(state: &AppState, ca: &str, version: i64) -> crate::api::nodes::CreateNode {
    let (_, Json(ssh)) = api::create(
        State(state.clone()),
        actor(),
        Json(api::CreateCredential {
            name: "SSH".into(),
            kind: "ssh_password".into(),
            reminder_at: None,
            payload: json!({"secret":"fixture-password"}),
        }),
    )
    .await
    .unwrap();
    let (_,Json(tls))=api::create(State(state.clone()),actor(),Json(api::CreateCredential{name:"Server TLS".into(),kind:"tls_identity".into(),reminder_at:None,payload:json!({"certificate":include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/../../tests/fixtures/credentials-test.crt")),"private_key":include_str!(concat!(env!("CARGO_MANIFEST_DIR"),"/../../tests/fixtures/credentials-test.key"))})})).await.unwrap();
    serde_json::from_value(json!({"name":"Managed node","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_credential_id":ssh["id"],"ssh_credential_version":1,"public_host":"example.test","public_port":443,"listen_addr":":443","config":{"tls":{"cert":format!("credential://{}/1/certificate",tls["id"].as_str().unwrap()),"key":format!("credential://{}/1/private_key",tls["id"].as_str().unwrap())},"mtls":{"mode":"managed","ca_credential_id":ca,"ca_credential_version":version}}})).unwrap()
}
pub(super) async fn managed_node(state: &AppState, ca: &str) -> Value {
    let receipt =
        crate::api::nodes::create(State(state.clone()), Json(node_request(state, ca, 1).await))
            .await
            .unwrap()
            .1
            .0;
    assert_eq!(receipt["node"]["mtls_required"], true);
    crate::api::nodes::get(
        State(state.clone()),
        Path(receipt["node"]["id"].as_str().unwrap().into()),
    )
    .await
    .unwrap()
    .0
}

async fn job(state: &AppState, node: &str) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,available_at,created_at,updated_at) VALUES($1,'deploy',$2,1,'running','starting',now(),now(),now())").bind(&id).bind(node).execute(&state.pool).await.unwrap();
    id
}
async fn auth(state: &AppState, node: &str, token: &str, password: &str) -> bool {
    let Json(result) = crate::api::subscriptions::hy2_auth(
        State(state.clone()),
        Path((node.into(), token.into())),
        Json(serde_json::from_value(json!({"addr":"127.0.0.1:1","auth":password,"tx":0})).unwrap()),
    )
    .await
    .unwrap();
    serde_json::to_value(result).unwrap()["ok"] == true
}

#[test]
fn node_policy_accepts_only_two_modes_and_rejects_manual_client_ca_or_acme() {
    for mut config in [
        json!({"mtls":{"mode":"external"}}),
        json!({"mtls":{"mode":"off","ca_credential_id":"x"}}),
        json!({"tls":{"clientCA":"credential://x/1/content"}}),
        json!({"acme":{},"mtls":{"mode":"managed","ca_credential_id":uuid::Uuid::new_v4().to_string(),"ca_credential_version":1}}),
    ] {
        assert!(node_mtls::validate_input(&mut config).is_err());
    }
    let mut config = json!({});
    assert_eq!(
        node_mtls::validate_input(&mut config).unwrap(),
        Settings::Off
    );
    assert_eq!(config["mtls"]["mode"], "off");
}

#[tokio::test]
async fn managed_node_zero_user_preparation_has_public_trust_and_independent_probe() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    assert_eq!(node["mtls_required"], true);
    assert!(node["config"]["tls"].get("clientCA").is_none());
    let policy = Settings::from_config(&node["config"]).unwrap();
    let trust = node_mtls::desired_trust(&state.pool, id, &policy)
        .await
        .unwrap();
    node_mtls::prepare_users(&state.pool, &state.secrets, id, &policy, &trust, false)
        .await
        .unwrap();
    credentials::maintenance::renew_probe_on(&state.pool, &state.secrets, id, &ca, 1)
        .await
        .unwrap();
    let probe = node_mtls::probe_material(&state.pool, &state.secrets, id, &ca, Some(1))
        .await
        .unwrap()
        .unwrap();
    assert!(probe.1.contains("PRIVATE KEY"));
    let (config, resource) = node_mtls::materialize(&state.pool, &node["config"], &trust)
        .await
        .unwrap();
    assert!(
        config["tls"]["clientCA"]
            .as_str()
            .unwrap()
            .contains("mtls-trust-")
    );
    assert!(
        !String::from_utf8(resource.unwrap().content)
            .unwrap()
            .contains("PRIVATE KEY")
    );
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(users, 0);
    let (resolved, _) =
        credentials::resolve_config(&state.pool, &state.secrets, &node["config"], false)
            .await
            .unwrap();
    let resolved = node_mtls::materialize(&state.pool, &resolved, &trust)
        .await
        .unwrap()
        .0;
    let yaml = crate::config::render_server_yaml_with_traffic_stats_port(
        &resolved,
        ":443",
        id,
        "node-token",
        "stats-secret",
        "https://example.test",
        9780,
    )
    .unwrap();
    assert!(yaml.contains("clientCA:"));
    assert!(!yaml.contains("mtls:"));
    assert!(!yaml.contains("PRIVATE KEY"));
}

#[tokio::test]
async fn ordinary_node_writes_cannot_pin_to_unpublished_ca_version() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let (_, Json(rotation)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "node-rotation")),
    )
    .await
    .unwrap();
    assert_eq!(rotation["published"], false);
    let mut config = node["config"].clone();
    config["mtls"]["ca_credential_version"] = json!(2);
    let err = crate::api::nodes::patch(
        State(state.clone()),
        Path(node["id"].as_str().unwrap().into()),
        Json(serde_json::from_value(json!({"expected_revision":1,"config":config})).unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::CONFLICT);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM config_versions WHERE node_id=$1")
        .bind(node["id"].as_str().unwrap())
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn activation_blocks_user_and_subscription_but_only_bound_live_job_probe_bypasses() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    user(&state, "alice", true).await;
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('alice',$1,$2,$3,now())").bind(id).bind(crate::security::token_digest("password")).bind(state.secrets.encrypt("password").unwrap()).execute(&state.pool).await.unwrap();
    let policy = Settings::from_config(&node["config"]).unwrap();
    let trust = node_mtls::desired_trust(&state.pool, id, &policy)
        .await
        .unwrap();
    node_mtls::prepare_users(&state.pool, &state.secrets, id, &policy, &trust, false)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE nodes SET deployed_revision=1,deployed_config_enc=desired_config_enc WHERE id=$1",
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
    let token = state
        .secrets
        .decrypt(
            &sqlx::query_scalar::<_, String>("SELECT node_token_enc FROM nodes WHERE id=$1")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
        )
        .unwrap();
    assert!(auth(&state, id, &token, "password").await);
    assert_eq!(
        crate::api::subscriptions::subscription_node_count(&state, "alice")
            .await
            .unwrap(),
        1
    );
    let job = job(&state, id).await;
    let activation = activation::begin(
        &state.pool,
        &state.secrets,
        activation::Begin {
            node: id,
            job: &job,
            revision: 1,
            digest: "new-config",
            snapshot: &node["config"],
            trust: &trust,
            probe: None,
        },
    )
    .await
    .unwrap();
    assert!(activation.fresh);
    assert!(!auth(&state, id, &token, "password").await);
    assert_eq!(
        crate::api::subscriptions::subscription_node_count(&state, "alice")
            .await
            .unwrap(),
        0
    );
    sqlx::query("INSERT INTO deployment_probe_tokens(node_id,token_hash,expires_at,job_id) VALUES($1,$2,now()+interval '1 minute',$3)").bind(id).bind(crate::security::token_digest("probe")).bind(&job).execute(&state.pool).await.unwrap();
    assert!(auth(&state, id, &token, "probe").await);
    sqlx::query(
        "UPDATE deployment_probe_tokens SET expires_at=now()-interval '1 second' WHERE node_id=$1",
    )
    .bind(id)
    .execute(&state.pool)
    .await
    .unwrap();
    assert!(!auth(&state, id, &token, "probe").await);
    activation::verified(&state.pool, &job).await.unwrap();
    assert!(!auth(&state, id, &token, "password").await);
    let mut tx = db::begin_write(&state.pool).await.unwrap();
    activation::commit(&mut tx, &job, "new-config")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(auth(&state, id, &token, "password").await);
    assert_eq!(
        crate::api::subscriptions::subscription_node_count(&state, "alice")
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn interrupted_activation_reuses_generation_and_mismatched_rollback_stays_inaccessible() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    let job = job(&state, id).await;
    sqlx::query("UPDATE nodes SET deployed_content_sha256='old-config' WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
    let trust = node_mtls::desired_trust(
        &state.pool,
        id,
        &Settings::from_config(&node["config"]).unwrap(),
    )
    .await
    .unwrap();
    let begin = || activation::Begin {
        node: id,
        job: &job,
        revision: 1,
        digest: "new-config",
        snapshot: &node["config"],
        trust: &trust,
        probe: None,
    };
    let first = activation::begin(&state.pool, &state.secrets, begin())
        .await
        .unwrap();
    activation::uncertain(&state.pool, &job).await.unwrap();
    let second = activation::begin(&state.pool, &state.secrets, begin())
        .await
        .unwrap();
    assert_eq!(first.id, second.id);
    assert_eq!(first.generation, second.generation);
    assert!(!second.fresh);
    assert!(
        activation::rolled_back(&state.pool, &job, Some("unknown-config"))
            .await
            .is_err()
    );
    let blocked: String = sqlx::query_scalar("SELECT mtls_state FROM nodes WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_ne!(blocked, "ready");
    activation::rolled_back(&state.pool, &job, Some("old-config"))
        .await
        .unwrap();
    let status: String = sqlx::query_scalar("SELECT mtls_state FROM nodes WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, "ready");
}

#[tokio::test]
async fn pending_configuration_requires_explicit_policy_before_recovery_deployment() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    sqlx::query("UPDATE nodes SET mtls_state='needs_configuration',mtls_recovery_configured=FALSE WHERE id=$1").bind(id).execute(&state.pool).await.unwrap();
    let implicit = crate::api::nodes::patch(
        State(state.clone()),
        Path(id.into()),
        Json(serde_json::from_value(json!({"expected_revision":1,"config":{}})).unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(implicit.status, StatusCode::BAD_REQUEST);
    let action = || Json(serde_json::from_value(json!({"expected_revision":1})).unwrap());
    assert_eq!(
        crate::api::nodes::deploy(State(state.clone()), Path(id.into()), action())
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    let _ = crate::api::nodes::ssh_test(State(state.clone()), Path(id.into()), action())
        .await
        .unwrap();
    let _ = crate::api::nodes::patch(
        State(state.clone()),
        Path(id.into()),
        Json(
            serde_json::from_value(json!({"expected_revision":1,"config":node["config"]})).unwrap(),
        ),
    )
    .await
    .unwrap();
    let status: String = sqlx::query_scalar("SELECT mtls_state FROM nodes WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, "needs_configuration");
    let payload: Value = sqlx::query_scalar(
        "SELECT payload_json FROM jobs WHERE node_id=$1 AND kind='sync' AND status='queued'",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(payload["force"], true);
    let implicit = crate::api::nodes::patch(
        State(state.clone()),
        Path(id.into()),
        Json(serde_json::from_value(json!({"expected_revision":2,"config":{}})).unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(implicit.status, StatusCode::BAD_REQUEST);
    let _ = crate::api::nodes::deploy(
        State(state.clone()),
        Path(id.into()),
        Json(serde_json::from_value(json!({"expected_revision":2})).unwrap()),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn expired_historical_probe_rejects_rollback_and_internal_probe_cannot_be_server_tls() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    let (probe, version) =
        credentials::maintenance::renew_probe_on(&state.pool, &state.secrets, id, &ca, 1)
            .await
            .unwrap();
    let mut input = serde_json::to_value(node_request(&state, &ca, 1).await).unwrap();
    input["config"]["tls"]["cert"] = json!(format!("credential://{probe}/{version}/certificate"));
    input["config"]["tls"]["key"] = json!(format!("credential://{probe}/{version}/private_key"));
    let err = crate::api::nodes::create(
        State(state.clone()),
        Json(serde_json::from_value(input).unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, StatusCode::CONFLICT);
    let policy = Settings::from_config(&node["config"]).unwrap();
    let trust = node_mtls::desired_trust(&state.pool, id, &policy)
        .await
        .unwrap();
    let job = job(&state, id).await;
    let _ = activation::begin(
        &state.pool,
        &state.secrets,
        activation::Begin {
            node: id,
            job: &job,
            revision: 1,
            digest: "target",
            snapshot: &node["config"],
            trust: &trust,
            probe: Some((&probe, version)),
        },
    )
    .await
    .unwrap();
    activation::verified(&state.pool, &job).await.unwrap();
    let mut tx = db::begin_write(&state.pool).await.unwrap();
    activation::commit(&mut tx, &job, "target").await.unwrap();
    tx.commit().await.unwrap();
    assert!(
        activation::historical_probe(&state.pool, id, 1)
            .await
            .unwrap()
            .is_some()
    );
    sqlx::query("UPDATE mtls_probe_versions SET expires_at=now()-interval '1 second' WHERE credential_id=$1 AND credential_version=$2").bind(&probe).bind(version).execute(&state.pool).await.unwrap();
    assert!(
        activation::historical_probe(&state.pool, id, 1)
            .await
            .unwrap_err()
            .to_string()
            .contains("expired")
    );
}

#[tokio::test]
async fn legacy_mtls_snapshot_is_rejected_before_ssh_or_signing() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    let job_id = job(&state, id).await;
    let mut options = node["config"].clone();
    options["tls"]["clientCA"] = json!("/legacy/ca.pem");
    sqlx::query("UPDATE config_versions SET config_enc=$2 WHERE node_id=$1 AND revision=1")
        .bind(id)
        .bind(
            state
                .secrets
                .encrypt(&json!({"server_config":options,"listen_addr":":443"}).to_string())
                .unwrap(),
        )
        .execute(&state.pool)
        .await
        .unwrap();
    let job = crate::deployment::JobInput {
        id: job_id,
        kind: "deploy".into(),
        node_id: Some(id.into()),
        target_revision: Some(1),
        payload: json!({}),
        attempts: 1,
    };
    let error = crate::deployment::run_job(&state.pool, &state.secrets, &job)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("legacy mTLS snapshots"));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM mtls_activations")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM mtls_probe_versions")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
