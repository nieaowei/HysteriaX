use axum::{
    Json,
    extract::{Extension, Path, Query, State},
};
use serde_json::{Value, json};
use sqlx::Row;

use crate::{
    api::{self, AdminActor},
    credentials, db,
    security::SecretBox,
    state::AppState,
};

pub(crate) async fn fixture() -> (AppState, String, String, String) {
    let state = AppState::new(
        db::test_pool_with_max_connections(4).await,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    credentials::migration::migrate(&state.pool, &state.secrets)
        .await
        .unwrap();
    let mut tx = db::begin_write(&state.pool).await.unwrap();
    let ssh = credentials::insert(
        &mut tx,
        &state.secrets,
        "SSH",
        "ssh_password",
        None,
        &json!({"secret":"fixture-password"}),
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    let payload =
        json!({"provider":"cloudflare","config":{"cloudflare_api_token":"fixture-provider-token"}});
    let metadata = credentials::validate_payload("dns", &payload).unwrap();
    let dns = credentials::insert(
        &mut tx,
        &state.secrets,
        "DNS",
        "dns",
        None,
        &payload,
        &metadata,
        &json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (_, Json(connection)) = api::dns::create_connection(
        State(state.clone()),
        Json(api::dns::ConnectionInput {
            name: "Fixture Cloudflare".into(),
            credential_id: dns,
            credential_version: 1,
        }),
    )
    .await
    .unwrap();
    let connection = connection["id"].as_str().unwrap().to_owned();
    sqlx::query("UPDATE dns_connections SET status='verified' WHERE id=$1")
        .bind(&connection)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO dns_zones(id,connection_id,provider_zone_id,name,enabled) VALUES('zone',$1,'remote-zone','example.test',true)").bind(&connection).execute(&state.pool).await.unwrap();
    (state, ssh, connection, "zone".into())
}
pub(crate) fn node_request(ssh: &str, allocation: Option<Value>) -> api::nodes::CreateNode {
    serde_json::from_value(json!({"name":"DNS node","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_credential_id":ssh,"ssh_credential_version":1,"public_host":"old.example.test","public_port":443,"listen_addr":":443","config":{},"dns_allocation":allocation})).unwrap()
}
pub(crate) async fn node(state: &AppState, ssh: &str) -> String {
    let (_, Json(result)) = api::nodes::create(State(state.clone()), Json(node_request(ssh, None)))
        .await
        .unwrap();
    result["node"]["id"].as_str().unwrap().into()
}

#[tokio::test]
async fn allocation_is_atomic_idempotent_and_does_not_configure_acme() {
    let (state, ssh, _, zone) = fixture().await;
    let allocation = json!({"idempotency_key":"create-node","zone_id":zone,"mode":"manual","hostname":"hk.example.test","ipv4":"8.8.8.8","ipv6":"2606:4700:4700::1111"});
    let (_, Json(first)) = api::nodes::create(
        State(state.clone()),
        Json(node_request(&ssh, Some(allocation.clone()))),
    )
    .await
    .unwrap();
    let (_, Json(second)) = api::nodes::create(
        State(state.clone()),
        Json(node_request(&ssh, Some(allocation.clone()))),
    )
    .await
    .unwrap();
    assert_eq!(first["node"]["id"], second["node"]["id"]);
    assert!(second.get("node_auth_token").is_none());
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM nodes")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM dns_records")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind='dns-record-create'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
    let id = first["node"]["id"].as_str().unwrap();
    let Json(detail) = api::nodes::get(State(state.clone()), Path(id.into()))
        .await
        .unwrap();
    assert_eq!(detail["public"]["host"], "hk.example.test");
    assert!(detail["config"].get("acme").is_none());
    let mut changed = allocation.clone();
    changed["hostname"] = json!("different.example.test");
    assert_eq!(
        api::nodes::create(
            State(state.clone()),
            Json(node_request(&ssh, Some(changed)))
        )
        .await
        .unwrap_err()
        .status,
        axum::http::StatusCode::CONFLICT
    );
    let mut duplicate = allocation;
    duplicate["idempotency_key"] = json!("different-key");
    assert!(
        api::nodes::create(
            State(state.clone()),
            Json(node_request(&ssh, Some(duplicate)))
        )
        .await
        .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn record_idempotency_conflicts_and_bound_delete_protection() {
    let (state, ssh, _, zone) = fixture().await;
    let request = || api::dns::CreateRecord {
        zone_id: zone.clone(),
        idempotency_key: "record-create".into(),
        record: super::RecordInput {
            name: "manual.example.test".into(),
            record_type: "A".into(),
            content: "8.8.8.8".into(),
            ttl: 300,
            proxied: false,
        },
    };
    let (_, Json(first)) = api::dns::create_record(State(state.clone()), Json(request()))
        .await
        .unwrap();
    let (_, Json(second)) = api::dns::create_record(State(state.clone()), Json(request()))
        .await
        .unwrap();
    assert_eq!(first, second);
    let record = first["resource_id"].as_str().unwrap();
    let mut input = request();
    input.record.content = "8.8.4.4".into();
    assert!(
        api::dns::create_record(State(state.clone()), Json(input))
            .await
            .is_err()
    );
    let id = node(&state, &ssh).await;
    sqlx::query("UPDATE dns_records SET desired=NULL,state='synced',provider_record_id='remote-record' WHERE id=$1").bind(record).execute(&state.pool).await.unwrap();
    let (_,Json(_))=api::dns::set_binding(State(state.clone()),Path(id.clone()),Json(api::dns::SetBinding{expected_revision:1,allocation:serde_json::from_value(json!({"idempotency_key":"bind-existing","mode":"existing","zone_id":zone,"record_ids":[record]})).unwrap()})).await.unwrap();
    let error = api::dns::delete_record(
        State(state.clone()),
        Path(record.into()),
        Json(api::dns::ActionInput {
            expected_revision: 1,
            idempotency_key: "delete-bound".into(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
    let result = api::dns::update_record(
        State(state.clone()),
        Path(record.into()),
        Json(api::dns::UpdateRecord {
            expected_revision: 1,
            idempotency_key: "rename-bound".into(),
            record: super::RecordInput {
                name: "renamed.example.test".into(),
                ..request().record
            },
        }),
    )
    .await;
    assert!(result.is_err());
    let row = sqlx::query("SELECT desired_revision,published_connection FROM nodes WHERE id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("desired_revision"), 2);
    assert!(
        row.get::<Option<Value>, _>("published_connection")
            .is_none()
    );
    let _ = api::dns::unbind(
        State(state.clone()),
        Path(id.clone()),
        Json(api::dns::Unbind {
            expected_revision: 2,
            idempotency_key: "unbind".into(),
            public_host: "8.8.8.8".into(),
        }),
    )
    .await
    .unwrap();
    assert!(
        api::dns::binding_summary(&state.pool, &id)
            .await
            .unwrap()
            .is_null()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM dns_records")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn dns_credentials_are_referenced_and_publication_creates_connection_targets() {
    let (state, _, connection, _) = fixture().await;
    let credential: String =
        sqlx::query_scalar("SELECT credential_id FROM dns_connections WHERE id=$1")
            .bind(&connection)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let refs = api::credentials::find_references(&state, &credential)
        .await
        .unwrap();
    assert!(refs.iter().any(|r| r["entity_type"] == "dns_connection"));
    let result = api::credentials::delete(
        State(state.clone()),
        Extension(AdminActor {
            id: "fixture".into(),
        }),
        Path(credential.clone()),
        Query(api::nodes::RevisionQuery {
            expected_revision: Some(1),
        }),
    )
    .await;
    assert!(result.is_err());
    let (_,Json(receipt))=api::credentials::publish(State(state.clone()),Extension(AdminActor{id:"fixture".into()}),Path(credential),Json(api::credentials::PublishVersion{expected_revision:1,payload:json!({"provider":"cloudflare","config":{"cloudflare_api_token":"new-fixture-token"}})})).await.unwrap();
    assert_eq!(receipt["affected_count"], 1);
    let Json(batch) = api::credentials::batch(
        State(state.clone()),
        Path(receipt["batch_id"].as_str().unwrap().into()),
    )
    .await
    .unwrap();
    assert_eq!(batch["dns_items"].as_array().unwrap().len(), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT credential_version FROM dns_connections WHERE id=$1")
            .bind(&connection)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert!(!batch.to_string().contains("new-fixture-token"));
}

#[tokio::test]
async fn subscriptions_keep_published_connection_during_target_edits() {
    let (state, ssh, _, _) = fixture().await;
    let id = node(&state, &ssh).await;
    let published = json!({"public_host":"old.example.test","public_port":443,"listen_addr":":443","tls_sni":"old.example.test","tls_skip_verify":false});
    sqlx::query("UPDATE nodes SET deployed_revision=1,deployed_config_enc=desired_config_enc,published_connection=$1 WHERE id=$2").bind(&published).bind(&id).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('user','Fixture user',now(),now())").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('user',$1,'fixture-hash',$2,now())").bind(&id).bind(state.secrets.encrypt("fixture-password").unwrap()).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO subscription_credentials(id,user_id,token_hash,token_enc,created_at) VALUES('subscription','user',$1,$2,now())").bind(crate::security::token_digest("fixture-subscription-token")).bind(state.secrets.encrypt("fixture-subscription-token").unwrap()).execute(&state.pool).await.unwrap();
    let _ = api::nodes::patch(State(state.clone()),Path(id.clone()),Json(serde_json::from_value(json!({"expected_revision":1,"public_host":"new.example.test","public_port":8443,"tls_sni":"new.example.test"})).unwrap())).await.unwrap();
    let response = api::subscriptions::download(
        State(state.clone()),
        Path("fixture-subscription-token".into()),
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .unwrap();
    let yaml: Value = serde_yaml::from_slice(&body).unwrap();
    assert_eq!(yaml["proxies"][0]["server"], "old.example.test");
    assert_eq!(yaml["proxies"][0]["port"], 443);
    assert_eq!(yaml["proxies"][0]["sni"], "old.example.test");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind='sync'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn rollback_checks_retained_external_records_and_does_not_recreate_deleted_dns() {
    let (state, ssh, _, zone) = fixture().await;
    let node = node(&state, &ssh).await;
    sqlx::query("INSERT INTO dns_records(id,zone_id,name,record_type,content,origin,state,deleted_at) VALUES('deleted-external',$1,'old.example.test','A','8.8.8.8','external','deleted',now())")
        .bind(&zone).execute(&state.pool).await.unwrap();
    let error =
        super::binding::ensure_ready(&state.pool, &state.secrets, &node, "old.example.test", true)
            .await
            .unwrap_err();
    assert!(error.to_string().contains("DNS records are missing"));
    // Unmanaged legacy endpoints retain their existing optional DNS integration.
    super::binding::ensure_ready(
        &state.pool,
        &state.secrets,
        &node,
        "old.example.test",
        false,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn new_retry_pins_verified_connection_version_and_keeps_original_operation_history() {
    let (state, _, connection, zone) = fixture().await;
    let (_, Json(receipt)) = api::dns::create_record(
        State(state.clone()),
        Json(api::dns::CreateRecord {
            zone_id: zone,
            idempotency_key: "retry-version".into(),
            record: super::RecordInput {
                name: "retry.example.test".into(),
                record_type: "A".into(),
                content: "8.8.8.8".into(),
                ttl: 1,
                proxied: false,
            },
        }),
    )
    .await
    .unwrap();
    let job = receipt["job_id"].as_str().unwrap();
    sqlx::query("UPDATE jobs SET status='failed' WHERE id=$1")
        .bind(job)
        .execute(&state.pool)
        .await
        .unwrap();
    let credential: String =
        sqlx::query_scalar("SELECT credential_id FROM dns_connections WHERE id=$1")
            .bind(&connection)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let payload = json!({"provider":"cloudflare","config":{"cloudflare_api_token":"new-token"}});
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES($1,2,$2,$3,now())")
        .bind(&credential).bind(state.secrets.encrypt(&payload.to_string()).unwrap()).bind(credentials::validate_payload("dns",&payload).unwrap()).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE dns_connections SET credential_version=2 WHERE id=$1")
        .bind(&connection)
        .execute(&state.pool)
        .await
        .unwrap();
    let (_, Json(retry)) = api::job_retries::retry(
        State(state.clone()),
        Path(job.into()),
        Json(api::job_retries::RetryRequest {
            expected_revision: 1,
        }),
    )
    .await
    .unwrap();
    let child_payload: Value = sqlx::query_scalar("SELECT payload_json FROM jobs WHERE id=$1")
        .bind(retry["job_id"].as_str())
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(child_payload["dns_credential_version"], 2);
    let original_version: i64 =
        sqlx::query_scalar("SELECT credential_version FROM dns_operations WHERE id=$1")
            .bind(child_payload["dns_operation_id"].as_str())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(original_version, 1);
}
