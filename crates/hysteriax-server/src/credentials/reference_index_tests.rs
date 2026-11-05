use super::*;
use axum::{
    Json,
    extract::{Extension, Path, Query, State},
};
use serde_json::json;
#[tokio::test]
async fn maintenance_initialization_backfills_bounded_batches_without_changing_ciphertext() {
    let pool = crate::db::test_pool_with_max_connections(2).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();

    let node = "upgrade-node";
    let mut tx = pool.begin().await.unwrap();
    let credential = super::super::insert(
        &mut tx,
        &secrets,
        "Retained",
        "ssh_password",
        None,
        &json!({"secret":"preserved"}),
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    let cipher = secrets
        .encrypt(&json!({"reference":format!("credential://{credential}/1/secret")}).to_string())
        .unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_credential_id,ssh_credential_version,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) VALUES($1,'Upgrade','127.0.0.1',22,'root',$2,1,'example.test',443,':443','hash',$3,$3,$4,now(),now())")
            .bind(node).bind(&credential).bind(secrets.encrypt("retained-token").unwrap()).bind(&cipher).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) SELECT 'upgrade-'||n,$1,n,$2,'retained-hash',now() FROM generate_series(1,300) n").bind(node).bind(&cipher).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let snapshot = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n)-'traffic_capture_mode' ORDER BY id) FROM nodes n),'history',(SELECT jsonb_agg(to_jsonb(v) ORDER BY id) FROM config_versions v),'credentials',(SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM credentials c),'versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v))";
    let before: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
    crate::db::run_migrations(&pool).await.unwrap();
    assert!(assert_ready(&pool).await.is_err());
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(refresh_batch_on(&mut tx, &secrets).await.unwrap(), 256);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credential_reference_dirty")
            .fetch_one(&mut *tx)
            .await
            .unwrap(),
        45
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credential_reference_dirty")
            .fetch_one(&pool)
            .await
            .unwrap(),
        301
    );
    initialize(&pool, &secrets).await.unwrap();
    assert_ready(&pool).await.unwrap();
    let after: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
    assert_eq!(
        before, after,
        "maintenance must preserve every source row and encrypted byte"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM credential_reference_edges WHERE credential_id=$1"
        )
        .bind(&credential)
        .fetch_one(&pool)
        .await
        .unwrap(),
        301
    );
    initialize(&pool, &secrets).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Value>(snapshot)
            .fetch_one(&pool)
            .await
            .unwrap(),
        before
    );
}
#[tokio::test]
async fn runtime_reads_reject_uninitialized_sources_without_writing() {
    let pool = crate::db::test_pool().await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    sqlx::query("INSERT INTO credential_reference_dirty(kind,source_id) VALUES('node','missing')")
        .execute(&pool)
        .await
        .unwrap();
    let result = begin_read(&pool, &secrets).await;
    assert!(result.is_err());
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM credential_reference_dirty")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        pending, 1,
        "ordinary reads must not rebuild or acknowledge sources"
    );
    initialize(&pool, &secrets).await.unwrap();
    let mut tx = begin_read(&pool, &secrets).await.unwrap();
    let readonly: String = sqlx::query_scalar("SHOW transaction_read_only")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(readonly, "on");
    tx.commit().await.unwrap();
}
#[tokio::test]
async fn changed_and_historical_references_block_deletion_without_prior_index_refresh() {
    let state = crate::state::AppState::new(
        crate::db::test_pool().await,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    let mut ids = Vec::new();
    for name in ["First", "Second"] {
        let mut tx = state.pool.begin().await.unwrap();
        let id = super::super::insert(
            &mut tx,
            &state.secrets,
            name,
            "ssh_password",
            None,
            &json!({"secret":"test"}),
            &json!({}),
            &json!({}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        ids.push(id);
    }
    let node=crate::credentials::test_node_request(&state,json!({"name":"Node","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"test","public_host":"example.test","public_port":443,"listen_addr":":443"})).await;
    let (_, Json(created)) = crate::api::nodes::create(State(state.clone()), Json(node))
        .await
        .unwrap();
    let node = created["node"]["id"].as_str().unwrap();
    for id in &ids {
        let cipher = state
            .secrets
            .encrypt(&json!({"test_reference":format!("credential://{id}/1/secret")}).to_string())
            .unwrap();
        let mut source_tx = state.pool.begin().await.unwrap();
        sqlx::query("UPDATE nodes SET desired_config_enc=$1 WHERE id=$2")
            .bind(&cipher)
            .bind(node)
            .execute(&mut *source_tx)
            .await
            .unwrap();
        flush_node_on(&mut source_tx, &state.secrets, node)
            .await
            .unwrap();
        source_tx.commit().await.unwrap();
        let error = crate::api::credentials::delete(
            State(state.clone()),
            Extension(crate::api::AdminActor { id: "test".into() }),
            Path(id.clone()),
            Query(crate::api::nodes::RevisionQuery {
                expected_revision: Some(1),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
    }
    let first = crate::api::credentials::find_references(&state, &ids[0])
        .await
        .unwrap();
    assert!(first.is_empty());
    let history = state
        .secrets
        .encrypt(&json!({"old_reference":format!("credential://{}/1/secret",ids[0])}).to_string())
        .unwrap();
    let mut source_tx = state.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES('history-test',$1,2,$2,'hash',now())").bind(node).bind(history).execute(&mut *source_tx).await.unwrap();
    flush_node_on(&mut source_tx, &state.secrets, node)
        .await
        .unwrap();
    source_tx.commit().await.unwrap();
    let references = crate::api::credentials::find_references(&state, &ids[0])
        .await
        .unwrap();
    assert!(references.iter().any(|r| r["source"] == "history"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credential_reference_dirty")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}
