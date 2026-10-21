use super::*;
use axum::{
    Json,
    extract::{Path, State},
};
use serde_json::json;
use sqlx::{PgPool, Row};

async fn fixture() -> (crate::state::AppState, String, String) {
    let imported = crate::db::test_pool_with_max_connections(4).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    super::migration::migrate(&imported, &secrets)
        .await
        .unwrap();
    reference_index::initialize(&imported, &secrets)
        .await
        .unwrap();
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&imported)
        .await
        .unwrap();
    imported.close().await;
    let url = std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap();
    let pool = crate::db::pool_options()
        .max_connections(4)
        .after_connect(move |connection, _| {
            let schema = schema.clone();
            Box::pin(async move {
                sqlx::query(&format!("SET search_path TO {schema}"))
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("SET lock_timeout='1s'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let maintenance: Option<String> =
        sqlx::query_scalar("SELECT current_setting('hysteriax.reference_maintenance',true)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(
        maintenance.as_deref(),
        Some("on"),
        "must exercise production commit checks"
    );
    let state = crate::state::AppState::new(pool, secrets);
    let mut tx = state.pool.begin().await.unwrap();
    let ssh = insert(
        &mut tx,
        &state.secrets,
        "SSH",
        "ssh_password",
        None,
        &json!({"secret":"test"}),
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    let target = insert(
        &mut tx,
        &state.secrets,
        "Pinned",
        "ssh_password",
        None,
        &json!({"secret":"retained"}),
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,artifact_names,created_at) SELECT credential_id,2,payload_enc,metadata,artifact_names,now() FROM credential_versions WHERE credential_id=$1 AND version=1").bind(&target).execute(&mut *tx).await.unwrap();
    sqlx::query("UPDATE credentials SET latest_version=2 WHERE id=$1")
        .bind(&target)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let request=serde_json::from_value(json!({"name":"Node","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_credential_id":ssh,"ssh_credential_version":1,"public_host":"example.test","public_port":443,"listen_addr":":443"})).unwrap();
    let (_, Json(created)) = crate::api::nodes::create(State(state.clone()), Json(request))
        .await
        .unwrap();
    (
        state,
        created["node"]["id"].as_str().unwrap().to_owned(),
        target,
    )
}
async fn set_reference(
    tx: &mut sqlx::PgConnection,
    state: &crate::state::AppState,
    node: &str,
    id: &str,
    version: i64,
) -> String {
    let cipher = state
        .secrets
        .encrypt(&json!({"reference":format!("credential://{id}/{version}/secret")}).to_string())
        .unwrap();
    sqlx::query("UPDATE nodes SET desired_config_enc=$1 WHERE id=$2")
        .bind(&cipher)
        .bind(node)
        .execute(tx)
        .await
        .unwrap();
    cipher
}
async fn disable_legacy_lock(pool: &PgPool) {
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(pool).await.unwrap();
}
async fn wait_for_lock(pool: &PgPool, pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT coalesce((SELECT wait_event_type='Lock' FROM pg_stat_activity WHERE pid=$1),false)").bind(pid).fetch_one(pool).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
}

#[tokio::test]
async fn production_commit_rejects_a_configuration_without_transactional_edges() {
    let (state, node, id) = fixture().await;
    let before: String = sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id=$1")
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    set_reference(&mut tx, &state, &node, &id, 1).await;
    let error = tx.commit().await.unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().constraint(),
        Some("reference_source_transaction")
    );
    let after: String = sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id=$1")
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
async fn node_and_history_edges_commit_and_roll_back_with_unchanged_cipher_bytes() {
    let (state, node, id) = fixture().await;
    let mut tx = state.pool.begin().await.unwrap();
    let cipher = set_reference(&mut tx, &state, &node, &id, 1).await;
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES('pinned-history',$1,2,$2,'retained-hash',now())").bind(&node).bind(&cipher).execute(&mut *tx).await.unwrap();
    reference_index::flush_node_on(&mut tx, &state.secrets, &node)
        .await
        .unwrap();
    let visible: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM credential_reference_edges WHERE credential_id=$1",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(visible, 0);
    tx.commit().await.unwrap();
    let row=sqlx::query("SELECT desired_config_enc,(SELECT count(*) FROM credential_reference_edges WHERE credential_id=$2) refs FROM nodes WHERE id=$1").bind(&node).bind(&id).fetch_one(&state.pool).await.unwrap();
    assert_eq!(row.get::<String, _>("desired_config_enc"), cipher);
    assert_eq!(row.get::<i64, _>("refs"), 2);
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE nodes SET desired_config_enc=$1 WHERE id=$2")
        .bind(state.secrets.encrypt("{}").unwrap())
        .bind(&node)
        .execute(&mut *tx)
        .await
        .unwrap();
    reference_index::flush_node_on(&mut tx, &state.secrets, &node)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    let refs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM credential_reference_edges WHERE credential_id=$1",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(refs, 2);
    reference_index::assert_ready(&state.pool).await.unwrap();
}

#[tokio::test]
async fn a_missing_version_rolls_back_the_configuration_and_existing_edges() {
    let (state, node, id) = fixture().await;
    let mut tx = state.pool.begin().await.unwrap();
    let original = set_reference(&mut tx, &state, &node, &id, 1).await;
    reference_index::flush_node_on(&mut tx, &state.secrets, &node)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    set_reference(&mut tx, &state, &node, &id, 99).await;
    let error = reference_index::flush_node_on(&mut tx, &state.secrets, &node)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().constraint(),
        Some("reference_edge_version")
    );
    tx.rollback().await.unwrap();
    let stored: String = sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id=$1")
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(stored, original);
}

#[tokio::test]
async fn publishing_a_reference_first_blocks_concurrent_version_deletion() {
    let (state, node, id) = fixture().await;
    disable_legacy_lock(&state.pool).await;
    let mut source = state.pool.begin().await.unwrap();
    set_reference(&mut source, &state, &node, &id, 1).await;
    reference_index::flush_node_on(&mut source, &state.secrets, &node)
        .await
        .unwrap();
    let mut deletion = state.pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *deletion)
        .await
        .unwrap();
    let target = id.clone();
    let work = tokio::spawn(async move {
        let result =
            sqlx::query("DELETE FROM credential_versions WHERE credential_id=$1 AND version=1")
                .bind(target)
                .execute(&mut *deletion)
                .await;
        deletion.rollback().await.unwrap();
        result
    });
    wait_for_lock(&state.pool, pid).await;
    source.commit().await.unwrap();
    let error = work.await.unwrap().unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().constraint(),
        Some("reference_edge_version")
    );
    let row: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM credential_versions WHERE credential_id=$1 AND version=1)",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(row);
}

#[tokio::test]
async fn deleting_a_version_first_rejects_a_late_reference_without_dangling_data() {
    let (state, node, id) = fixture().await;
    disable_legacy_lock(&state.pool).await;
    let mut deletion = state.pool.begin().await.unwrap();
    sqlx::query("DELETE FROM credential_versions WHERE credential_id=$1 AND version=1")
        .bind(&id)
        .execute(&mut *deletion)
        .await
        .unwrap();
    let mut source = state.pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *source)
        .await
        .unwrap();
    let original: String = sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id=$1")
        .bind(&node)
        .fetch_one(&mut *source)
        .await
        .unwrap();
    set_reference(&mut source, &state, &node, &id, 1).await;
    let secrets = state.secrets.clone();
    let target = node.clone();
    let work = tokio::spawn(async move {
        let result = reference_index::flush_node_on(&mut source, &secrets, &target).await;
        source.rollback().await.unwrap();
        result
    });
    wait_for_lock(&state.pool, pid).await;
    deletion.commit().await.unwrap();
    let error = work.await.unwrap().unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().constraint(),
        Some("reference_edge_version")
    );
    let stored: String = sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id=$1")
        .bind(node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(stored, original);
    reference_index::assert_ready(&state.pool).await.unwrap();
}

#[tokio::test]
async fn production_node_edit_and_usage_copy_leave_no_unindexed_history() {
    let (state, node, _) = fixture().await;
    let input = serde_json::from_value(json!({"expected_revision":1,"name":"Edited"})).unwrap();
    let _ = crate::api::nodes::patch(State(state.clone()), Path(node.clone()), Json(input))
        .await
        .unwrap();
    let input = serde_json::from_value(json!({"expected_revision":2,"usage_bytes":42})).unwrap();
    let _ = crate::node_limits::update_usage(State(state.clone()), Path(node), Json(input))
        .await
        .unwrap();
    reference_index::assert_ready(&state.pool).await.unwrap();
}
