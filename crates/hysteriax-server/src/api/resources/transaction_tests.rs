use super::*;
use crate::credentials::reference_index;
use sqlx::{PgConnection, PgPool};

pub(super) async fn fixture() -> (AppState, String, String) {
    let pool = db::test_pool_with_max_connections(4).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    crate::credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    reference_index::initialize(&pool, &secrets).await.unwrap();
    let state = AppState::new(pool, secrets);
    let node = create_node(&state, "Node").await;
    let (_, Json(resource)) = create(
        State(state.clone()),
        AxumPath(node.clone()),
        Json(upload("rules.acl")),
    )
    .await
    .unwrap();
    (state, node, resource["id"].as_str().unwrap().into())
}
async fn create_node(state: &AppState, name: &str) -> String {
    let input=crate::credentials::test_node_request(state,json!({"name":name,"ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"test","public_host":"example.test","public_port":443,"listen_addr":":443"})).await;
    let (_, Json(node)) = crate::api::nodes::create(State(state.clone()), Json(input))
        .await
        .unwrap();
    node["node"]["id"].as_str().unwrap().into()
}
fn upload(name: &str) -> CreateResource {
    CreateResource {
        name: name.into(),
        resource_kind: "acl".into(),
        content_base64: STANDARD.encode("direct(all)"),
    }
}
async fn disable_legacy_lock(pool: &PgPool) {
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(pool).await.unwrap();
}
async fn pin(
    connection: &mut PgConnection,
    state: &AppState,
    node: &str,
    resource: &str,
) -> String {
    sqlx::query("SET LOCAL hysteriax.reference_maintenance='off'")
        .execute(&mut *connection)
        .await
        .unwrap();
    let cipher = state
        .secrets
        .encrypt(&json!({"acl":{"file":format!("resource://{resource}")}}).to_string())
        .unwrap();
    sqlx::query("UPDATE nodes SET desired_config_enc=$1 WHERE id=$2")
        .bind(&cipher)
        .bind(node)
        .execute(connection)
        .await
        .unwrap();
    cipher
}
async fn wait_for_lock(pool: &PgPool, pid: i32) {
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            if sqlx::query_scalar::<_,bool>("SELECT coalesce((SELECT wait_event_type='Lock' FROM pg_stat_activity WHERE pid=$1),false)").bind(pid).fetch_one(pool).await.unwrap() {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
}

#[tokio::test]
async fn duplicate_uploads_commit_one_resource_and_one_audit_without_global_lock() {
    let (state, node, _) = fixture().await;
    disable_legacy_lock(&state.pool).await;
    let (a, b) = tokio::join!(
        create(
            State(state.clone()),
            AxumPath(node.clone()),
            Json(upload("same.acl"))
        ),
        create(
            State(state.clone()),
            AxumPath(node.clone()),
            Json(upload("same.acl"))
        )
    );
    let results = [a, b];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| r
                .as_ref()
                .err()
                .is_some_and(|e| e.status == StatusCode::CONFLICT))
            .count(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM config_resources WHERE name='same.acl'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM audit_records WHERE action='node.resource_uploaded' AND detail_json->>'name'='same.acl'").fetch_one(&state.pool).await.unwrap(),1);
}

#[tokio::test]
async fn resource_upload_and_delete_roll_back_when_their_audit_fails() {
    let (state, node, resource) = fixture().await;
    sqlx::raw_sql("CREATE FUNCTION reject_resource_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action IN ('node.resource_uploaded','node.resource_deleted') THEN RAISE EXCEPTION 'injected audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_resource_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_resource_audit();").execute(&state.pool).await.unwrap();
    assert!(
        create(
            State(state.clone()),
            AxumPath(node.clone()),
            Json(upload("failed.acl"))
        )
        .await
        .is_err()
    );
    assert!(
        delete(State(state.clone()), AxumPath((node, resource.clone())))
            .await
            .is_err()
    );
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM config_resources")
        .fetch_all(&state.pool)
        .await
        .unwrap();
    assert_eq!(ids, vec![resource]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE action='node.resource_deleted'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn desired_deployed_and_history_pins_block_deletion_until_the_sources_are_removed() {
    let (state, node, resource) = fixture().await;
    let mut tx = state.pool.begin().await.unwrap();
    let cipher = pin(&mut tx, &state, &node, &resource).await;
    sqlx::query("UPDATE nodes SET deployed_config_enc=$1 WHERE id=$2")
        .bind(&cipher)
        .bind(&node)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES('resource-history',$1,2,$2,'retained-digest',now())").bind(&node).bind(&cipher).execute(&mut *tx).await.unwrap();
    reference_index::flush_node_on(&mut tx, &state.secrets, &node)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM config_resource_references")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM config_resource_references")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        delete(
            State(state.clone()),
            AxumPath((node.clone(), resource.clone()))
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::CONFLICT
    );
    let stored: String =
        sqlx::query_scalar("SELECT config_enc FROM config_versions WHERE id='resource-history'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, cipher);
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE nodes SET desired_config_enc=$1,deployed_config_enc=NULL WHERE id=$2")
        .bind(state.secrets.encrypt("{}").unwrap())
        .bind(&node)
        .execute(&mut *tx)
        .await
        .unwrap();
    reference_index::flush_node_on(&mut tx, &state.secrets, &node)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        delete(
            State(state.clone()),
            AxumPath((node.clone(), resource.clone()))
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::CONFLICT
    );
    sqlx::query("DELETE FROM config_versions WHERE id='resource-history'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        delete(State(state.clone()), AxumPath((node, resource)))
            .await
            .unwrap(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn wrong_node_resource_pin_rolls_back_the_source_and_existing_edges() {
    let (state, node, resource) = fixture().await;
    let other = create_node(&state, "Other").await;
    let before: String = sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id=$1")
        .bind(&other)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    pin(&mut tx, &state, &other, &resource).await;
    let error = reference_index::flush_node_on(&mut tx, &state.secrets, &other)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().constraint(),
        Some("resource_reference_resource")
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT desired_config_enc FROM nodes WHERE id=$1")
            .bind(&other)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        delete(State(state.clone()), AxumPath((node, resource)))
            .await
            .unwrap(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn a_committed_pin_rejects_a_concurrent_delete_at_the_native_foreign_key() {
    let (state, node, resource) = fixture().await;
    disable_legacy_lock(&state.pool).await;
    let mut source = state.pool.begin().await.unwrap();
    pin(&mut source, &state, &node, &resource).await;
    reference_index::flush_node_on(&mut source, &state.secrets, &node)
        .await
        .unwrap();
    let mut deletion = state.pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *deletion)
        .await
        .unwrap();
    let target = resource.clone();
    let work = tokio::spawn(async move {
        let result = sqlx::query("DELETE FROM config_resources WHERE id=$1")
            .bind(target)
            .execute(&mut *deletion)
            .await;
        deletion.rollback().await.unwrap();
        result
    });
    wait_for_lock(&state.pool, pid).await;
    source.commit().await.unwrap();
    assert_eq!(
        work.await
            .unwrap()
            .unwrap_err()
            .as_database_error()
            .unwrap()
            .constraint(),
        Some("resource_reference_resource")
    );
}

#[tokio::test]
async fn deletion_before_a_late_pin_rejects_and_rolls_back_that_pin() {
    let (state, node, resource) = fixture().await;
    disable_legacy_lock(&state.pool).await;
    let before: String = sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id=$1")
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut deletion = state.pool.begin().await.unwrap();
    sqlx::query("DELETE FROM config_resources WHERE id=$1")
        .bind(&resource)
        .execute(&mut *deletion)
        .await
        .unwrap();
    let mut source = state.pool.begin().await.unwrap();
    pin(&mut source, &state, &node, &resource).await;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *source)
        .await
        .unwrap();
    let secrets = state.secrets.clone();
    let target = node.clone();
    let work = tokio::spawn(async move {
        let result = reference_index::flush_node_on(&mut source, &secrets, &target).await;
        source.rollback().await.unwrap();
        result
    });
    wait_for_lock(&state.pool, pid).await;
    deletion.commit().await.unwrap();
    assert_eq!(
        work.await
            .unwrap()
            .unwrap_err()
            .as_database_error()
            .unwrap()
            .constraint(),
        Some("resource_reference_resource")
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT desired_config_enc FROM nodes WHERE id=$1")
            .bind(node)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn node_api_patch_pins_resources_and_node_deletion_cascades_the_projection() {
    let (state, node, resource) = fixture().await;
    let input = serde_json::from_value(
        json!({"expected_revision":1,"config":{"acl":{"file":format!("resource://{resource}")}}}),
    )
    .unwrap();
    let _ = crate::api::nodes::patch(State(state.clone()), AxumPath(node.clone()), Json(input))
        .await
        .unwrap();
    let pins: Vec<String> = sqlx::query_scalar(
        "SELECT source FROM config_resource_references WHERE resource_id=$1 ORDER BY source",
    )
    .bind(&resource)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(pins, vec!["desired", "history"]);
    assert_eq!(
        delete(State(state.clone()), AxumPath((node.clone(), resource)))
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM config_resource_references")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    reference_index::assert_clean_on(&mut state.pool.acquire().await.unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn upgrade_requeues_clean_sources_and_preserves_original_resource_and_configuration_bytes() {
    let pool = db::test_pool_through_version(2, 47).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    crate::credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    let cipher = secrets
        .encrypt(r#"{"acl":{"file":"resource://legacy-acl"}}"#)
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let ssh = crate::credentials::insert(
        &mut tx,
        &secrets,
        "Retained SSH",
        "ssh_password",
        None,
        &json!({"secret":"retained"}),
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_credential_id,ssh_credential_version,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,deployed_config_enc,created_at,updated_at) VALUES('legacy-node','Legacy','127.0.0.1',22,'root',$1,1,'example.test',443,':443','hash',$2,$2,$3,$3,now(),now())")
        .bind(ssh).bind(secrets.encrypt("old-token").unwrap()).bind(&cipher).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO config_resources(id,node_id,name,resource_kind,content_enc,content_sha256,size_bytes,created_at) VALUES('legacy-acl','legacy-node','retained.acl','acl',$1,'retained-digest',11,now())")
        .bind(secrets.encrypt_bytes(b"direct(all)").unwrap()).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES('legacy-history','legacy-node',1,$1,'old-hash',now())").bind(&cipher).execute(&mut *tx).await.unwrap();
    // A 0047 database may have no dirty sources; 0048 must still discover them.
    sqlx::query("DELETE FROM credential_reference_dirty")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let snapshot = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n)-'traffic_capture_mode' ORDER BY id) FROM nodes n),'history',(SELECT jsonb_agg(to_jsonb(v) ORDER BY id) FROM config_versions v),'resources',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM config_resources r))";
    let before: Value = sqlx::query_scalar(snapshot).fetch_one(&pool).await.unwrap();
    db::run_migrations(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM credential_reference_dirty")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert!(reference_index::assert_ready(&pool).await.is_err());
    reference_index::initialize(&pool, &secrets).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM config_resource_references")
            .fetch_one(&pool)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        sqlx::query_scalar::<_, Value>(snapshot)
            .fetch_one(&pool)
            .await
            .unwrap(),
        before
    );
    reference_index::initialize(&pool, &secrets).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Value>(snapshot)
            .fetch_one(&pool)
            .await
            .unwrap(),
        before
    );
}
