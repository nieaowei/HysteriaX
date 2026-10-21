use super::*;
async fn fixture() -> (AppState, String) {
    let pool = db::test_pool_with_max_connections(4).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    vault::migration::migrate(&pool, &secrets).await.unwrap();
    vault::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    let state = AppState::new(pool, secrets);
    let (_, Json(created)) = create(State(state.clone()), actor(), Json(input()))
        .await
        .unwrap();
    (state, created["id"].as_str().unwrap().into())
}
fn actor() -> Extension<AdminActor> {
    Extension(AdminActor {
        id: "transaction-test".into(),
    })
}
fn input() -> CreateCredential {
    CreateCredential {
        name: "Password".into(),
        kind: "ssh_password".into(),
        reminder_at: None,
        payload: json!({"secret":"original"}),
    }
}
async fn node(state: &AppState, id: &str, name: &str) -> String {
    let input=serde_json::from_value(json!({"name":name,"ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_credential_id":id,"ssh_credential_version":1,"public_host":"example.test","public_port":443,"listen_addr":":443"})).unwrap();
    let (_, Json(created)) = crate::api::nodes::create(State(state.clone()), Json(input))
        .await
        .unwrap();
    created["node"]["id"].as_str().unwrap().into()
}
async fn publish_once(state: &AppState, id: &str) -> Result<(StatusCode, Json<Value>), ApiError> {
    publish(
        State(state.clone()),
        actor(),
        Path(id.into()),
        Json(PublishVersion {
            expected_revision: 1,
            payload: json!({"secret":"published"}),
        }),
    )
    .await
}
async fn disable_legacy_lock(state: &AppState) {
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
}
async fn reject_audit(state: &AppState, action: &str) {
    assert!(
        action
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'_')
    );
    sqlx::raw_sql(&format!("CREATE FUNCTION reject_vault_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='{action}' THEN RAISE EXCEPTION 'injected audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_vault_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_vault_audit();")).execute(&state.pool).await.unwrap();
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
#[tokio::test]
async fn creation_audit_failure_rolls_back_both_parent_and_encrypted_version() {
    let (state, _) = fixture().await;
    reject_audit(&state, "credential.created").await;
    assert!(
        create(State(state.clone()), actor(), Json(input()))
            .await
            .is_err()
    );
    assert_eq!(count(&state, "SELECT count(*) FROM credentials").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_versions").await,
        1
    );
}
#[tokio::test]
async fn concurrent_edits_commit_one_revision_without_either_old_global_lock() {
    let (state, id) = fixture().await;
    disable_legacy_lock(&state).await;
    let holder = db::begin_write(&state.pool).await.unwrap();
    let edit = || {
        Json(PatchCredential {
            expected_revision: 1,
            name: "Edited".into(),
            archived: false,
            reminder_at: None,
        })
    };
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(
            patch(State(state.clone()), actor(), Path(id.clone()), edit()),
            patch(State(state.clone()), actor(), Path(id.clone()), edit())
        )
    })
    .await
    .unwrap();
    holder.rollback().await.unwrap();
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
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='credential.updated'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn publication_audit_failure_rolls_back_versions_batches_jobs_and_events() {
    let (state, id) = fixture().await;
    node(&state, &id, "Target").await;
    reject_audit(&state, "credential.version_published").await;
    assert!(publish_once(&state, &id).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_versions").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        0
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    assert_eq!(count(&state, "SELECT count(*) FROM job_events").await, 0);
    assert_eq!(
        count(&state, "SELECT latest_version FROM credentials").await,
        1
    );
}
#[tokio::test]
async fn a_retried_publication_commits_one_version_and_one_complete_batch() {
    let (state, id) = fixture().await;
    let target = node(&state, &id, "Target").await;
    sqlx::raw_sql("CREATE SEQUENCE vault_publish_attempt; CREATE FUNCTION retry_vault_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='credential.version_published' AND nextval('vault_publish_attempt')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_vault_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_vault_audit();").execute(&state.pool).await.unwrap();
    let (_, Json(response)) = publish_once(&state, &id).await.unwrap();
    assert_eq!(response["version"], 2);
    assert_eq!(response["revision"], 2);
    assert_eq!(response["affected_count"], 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_versions").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batch_items").await,
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='credential.version_published'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM vault_publish_attempt").await,
        2
    );
    let node: String = sqlx::query_scalar("SELECT node_id FROM credential_batch_items")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(node, target);
    let cipher: String = sqlx::query_scalar(
        "SELECT payload_enc FROM credential_versions WHERE credential_id=$1 AND version=2",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&state.secrets.decrypt(&cipher).unwrap()).unwrap(),
        json!({"secret":"published"})
    );
}
#[tokio::test]
async fn publication_skips_deployed_and_history_only_references_and_unrelated_node_locks() {
    let (state, id) = fixture().await;
    let target = node(&state, &id, "Target").await;
    let (_, Json(other)) = create(State(state.clone()), actor(), Json(input()))
        .await
        .unwrap();
    let other = node(&state, other["id"].as_str().unwrap(), "Other").await;
    let mut tx = state.pool.begin().await.unwrap();
    let config = state
        .secrets
        .encrypt(&json!({"reference":format!("credential://{id}/1/secret")}).to_string())
        .unwrap();
    sqlx::query("UPDATE nodes SET deployed_config_enc=$1 WHERE id=$2")
        .bind(&config)
        .bind(&other)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES('history-only',$1,2,$2,'retained',now())").bind(&other).bind(config).execute(&mut *tx).await.unwrap();
    vault::reference_index::flush_node_on(&mut tx, &state.secrets, &other)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    disable_legacy_lock(&state).await;
    let mut blocker = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(other)
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let (_, Json(response)) =
        tokio::time::timeout(std::time::Duration::from_secs(2), publish_once(&state, &id))
            .await
            .unwrap()
            .unwrap();
    blocker.rollback().await.unwrap();
    assert_eq!(response["affected_count"], 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT node_id FROM credential_batch_items")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        target
    );
}
#[tokio::test]
async fn concurrent_batch_retries_enqueue_one_replacement_job() {
    let (state, id) = fixture().await;
    node(&state, &id, "Target").await;
    let (_, Json(published)) = publish_once(&state, &id).await.unwrap();
    let batch = published["batch_id"].as_str().unwrap();
    sqlx::query("UPDATE jobs SET status='failed',stage='failed'")
        .execute(&state.pool)
        .await
        .unwrap();
    disable_legacy_lock(&state).await;
    let (a, b) = tokio::join!(
        retry(State(state.clone()), actor(), Path(batch.into())),
        retry(State(state.clone()), actor(), Path(batch.into()))
    );
    let (_, Json(a)) = a.unwrap();
    let (_, Json(b)) = b.unwrap();
    assert_eq!(
        a["affected_count"].as_u64().unwrap() + b["affected_count"].as_u64().unwrap(),
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 2);
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='queued'").await,
        1
    );
}
#[tokio::test]
async fn retry_audit_failure_preserves_the_previous_job_and_batch_item() {
    let (state, id) = fixture().await;
    node(&state, &id, "Target").await;
    let (_, Json(published)) = publish_once(&state, &id).await.unwrap();
    let batch = published["batch_id"].as_str().unwrap();
    sqlx::query("UPDATE jobs SET status='failed',stage='failed'")
        .execute(&state.pool)
        .await
        .unwrap();
    let old: String = sqlx::query_scalar("SELECT job_id FROM credential_batch_items")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    reject_audit(&state, "credential.batch_retried").await;
    assert!(
        retry(State(state.clone()), actor(), Path(batch.into()))
            .await
            .is_err()
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT job_id FROM credential_batch_items")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        old
    );
}
