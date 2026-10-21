use super::*;
use crate::{
    api::{self, AdminActor},
    background::maintenance,
    state::AppState,
};
use axum::{
    Json,
    extract::{Extension, State},
};

async fn fixture() -> (AppState, String, String) {
    let pool = db::test_pool_with_max_connections(8).await;
    let state = AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    super::super::migration::migrate(&state.pool, &state.secrets)
        .await
        .unwrap();
    let (_, Json(ca)) = api::managed_mtls::create(
        State(state.clone()),
        Extension(AdminActor { id: "test".into() }),
        Json(
            serde_json::from_value(
                json!({"name":"CA","source":"generated","policy":{"client_validity_days":180,"renew_before_hours":720},"idempotency_key":"probe-ca"}),
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    let ca = ca["id"].as_str().unwrap().to_owned();
    let (_,Json(node))=api::nodes::create(State(state.clone()),Json(super::super::test_node_request(&state,json!({"name":"Probe transaction node","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"unused","public_host":"example.test","public_port":443,"listen_addr":":443"})).await)).await.unwrap();
    let node = node["node"]["id"].as_str().unwrap().to_owned();
    sqlx::query("UPDATE nodes SET mtls_state='ready',mtls_error='Probe certificate renewal failed: previous failure' WHERE id=$1").bind(&node).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,desired,deployed) VALUES($1,$2,1,true,true)").bind(&node).bind(&ca).execute(&state.pool).await.unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    (state, node, ca)
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn native_retry_reuses_prepared_private_material_and_commits_one_active_probe() {
    let (state, node, ca) = fixture().await;
    let p = prepare(&state.pool, &state.secrets, &node, &ca, 1)
        .await
        .unwrap();
    let (id, cipher) = (p.new_credential.clone(), p.payload_enc.clone());
    sqlx::raw_sql("CREATE SEQUENCE probe_attempts; CREATE FUNCTION retry_probe_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('probe_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_probe_insert BEFORE INSERT ON mtls_probe_versions FOR EACH ROW EXECUTE FUNCTION retry_probe_insert();").execute(&state.pool).await.unwrap();
    assert_eq!(commit(&state.pool, p).await.unwrap(), (id.clone(), 1));
    assert_eq!(
        count(&state, "SELECT last_value FROM probe_attempts").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_probe_versions").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_active_probes").await,
        1
    );
    let stored: String = sqlx::query_scalar(
        "SELECT payload_enc FROM credential_versions WHERE credential_id=$1 AND version=1",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(stored, cipher);
    let error: Option<String> = sqlx::query_scalar("SELECT mtls_error FROM nodes WHERE id=$1")
        .bind(node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(error.is_none());
}

#[tokio::test]
async fn simultaneous_first_signatures_publish_only_one_identity_without_the_global_lock() {
    let (state, node, ca) = fixture().await;
    let (a, b) = tokio::join!(
        prepare(&state.pool, &state.secrets, &node, &ca, 1),
        prepare(&state.pool, &state.secrets, &node, &ca, 1)
    );
    let (a, b) = tokio::join!(
        commit(&state.pool, a.unwrap()),
        commit(&state.pool, b.unwrap())
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_probe_versions").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_active_probes").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE metadata->>'internal_probe'='true'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn concurrent_renewals_keep_one_next_version_and_reject_the_losing_baseline() {
    let (state, node, ca) = fixture().await;
    let p = prepare(&state.pool, &state.secrets, &node, &ca, 1)
        .await
        .unwrap();
    let (id, _) = commit(&state.pool, p).await.unwrap();
    sqlx::query("UPDATE mtls_probe_versions SET expires_at=clock_timestamp()-interval '1 day'")
        .execute(&state.pool)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        prepare(&state.pool, &state.secrets, &node, &ca, 1),
        prepare(&state.pool, &state.secrets, &node, &ca, 1)
    );
    let (a, b) = tokio::join!(
        commit(&state.pool, a.unwrap()),
        commit(&state.pool, b.unwrap())
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(a.or(b).unwrap(), (id, 2));
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_probe_versions").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT credential_version FROM mtls_active_probes").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE metadata->>'internal_probe'='true'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn node_revision_deletion_and_ca_state_are_rechecked_after_signing() {
    let (state, node, ca) = fixture().await;
    let p = prepare(&state.pool, &state.secrets, &node, &ca, 1)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET desired_revision=desired_revision+1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(commit(&state.pool, p).await.is_err());
    let p = prepare(&state.pool, &state.secrets, &node, &ca, 1)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET state='deleting' WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(commit(&state.pool, p).await.is_err());
    assert!(
        prepare(&state.pool, &state.secrets, &node, &ca, 1)
            .await
            .is_err()
    );
    sqlx::query("UPDATE nodes SET state='draft' WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let p = prepare(&state.pool, &state.secrets, &node, &ca, 1)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_ca_generations SET state='retired' WHERE ca_id=$1")
        .bind(&ca)
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(commit(&state.pool, p).await.is_err());
    assert!(
        prepare(&state.pool, &state.secrets, &node, &ca, 1)
            .await
            .is_err()
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE metadata->>'internal_probe'='true'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn active_probe_failure_rolls_back_key_version_probe_and_node_error_together() {
    let (state, node, ca) = fixture().await;
    let p = prepare(&state.pool, &state.secrets, &node, &ca, 1)
        .await
        .unwrap();
    let id = p.new_credential.clone();
    sqlx::raw_sql("CREATE FUNCTION reject_active_probe() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'activation failed'; END $$; CREATE TRIGGER reject_active_probe BEFORE INSERT ON mtls_active_probes FOR EACH ROW EXECUTE FUNCTION reject_active_probe();").execute(&state.pool).await.unwrap();
    assert!(commit(&state.pool, p).await.is_err());
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials WHERE id=$1)")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(!exists);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_probe_versions").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_active_probes").await,
        0
    );
    let error: Option<String> = sqlx::query_scalar("SELECT mtls_error FROM nodes WHERE id=$1")
        .bind(node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        error.as_deref(),
        Some("Probe certificate renewal failed: previous failure")
    );
}

#[tokio::test]
async fn expired_prepared_material_and_replaced_execution_cannot_publish() {
    let (state, node, ca) = fixture().await;
    let mut p = prepare(&state.pool, &state.secrets, &node, &ca, 1)
        .await
        .unwrap();
    p.expires_at = Utc::now() - chrono::Duration::seconds(1);
    assert!(commit(&state.pool, p).await.is_err());
    let p = prepare(&state.pool, &state.secrets, &node, &ca, 1)
        .await
        .unwrap();
    maintenance::ensure(&state.pool, "probe-test", "probe-test", json!({}))
        .await
        .unwrap();
    let old = maintenance::claim(&state.pool, "probe-test", "old")
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = maintenance::claim(&state.pool, "probe-test", "new")
        .await
        .unwrap()
        .unwrap();
    assert!(
        crate::execution::supervise(state.pool.clone(), old.leases, commit(&state.pool, p))
            .await
            .is_none()
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_probe_versions").await,
        0
    );
    let p = prepare(&state.pool, &state.secrets, &node, &ca, 1)
        .await
        .unwrap();
    crate::execution::supervise(state.pool.clone(), next.leases, commit(&state.pool, p))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_probe_versions").await,
        1
    );
}
