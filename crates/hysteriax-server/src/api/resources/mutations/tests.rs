use super::*;

async fn fixture() -> (AppState, String, String) {
    let (state, node, resource) = super::super::transaction_tests::fixture().await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('admin',$1,'Admin',clock_timestamp(),clock_timestamp())")
        .bind(crate::security::token_digest("admin")).execute(&state.pool).await.unwrap();
    (state, node, resource)
}

async fn serve(state: AppState, node: &str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/api/v2/nodes/{node}/resources",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, crate::api::router(state))
            .await
            .unwrap()
    });
    (base, task)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
}

fn upload(name: &str) -> Value {
    json!({"name":name,"resource_kind":"geoip","content_base64":STANDARD.encode([0_u8,255,10,13])})
}

async fn post(client: &reqwest::Client, base: &str, key: &str, name: &str) -> reqwest::Response {
    client
        .post(base)
        .bearer_auth("admin")
        .header("idempotency-key", key)
        .json(&upload(name))
        .send()
        .await
        .unwrap()
}

async fn delete(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    resource: &str,
) -> reqwest::Response {
    client
        .delete(format!("{base}/{resource}"))
        .bearer_auth("admin")
        .header("idempotency-key", key)
        .send()
        .await
        .unwrap()
}

async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

async fn snapshot(state: &AppState) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('resources',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM config_resources t),'nodes',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM nodes t),'pins',(SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text) FROM config_resource_references t),'audits',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM audit_records t),'receipts',(SELECT jsonb_agg(to_jsonb(t) ORDER BY actor_id,operation,idempotency_key) FROM management_receipts t),'buckets',(SELECT jsonb_agg(to_jsonb(t) ORDER BY domain,bucket) FROM cache_bucket_versions t))")
        .fetch_one(&state.pool).await.unwrap()
}

#[tokio::test]
async fn concurrent_upload_replays_one_binary_resource_even_after_node_removal_starts() {
    let (state, node, _) = fixture().await;
    let (base, server) = serve(state.clone(), &node).await;
    let client = client();
    let (a, b) = tokio::join!(
        post(&client, &base, "same", "data.bin"),
        post(&client, &base, "same", "data.bin")
    );
    assert_eq!(a.status(), StatusCode::CREATED);
    assert_eq!(b.status(), StatusCode::CREATED);
    let first = a.bytes().await.unwrap();
    assert_eq!(b.bytes().await.unwrap(), first);
    let response: Value = serde_json::from_slice(&first).unwrap();
    let id = response["id"].as_str().unwrap();
    let (cipher, digest): (String, String) =
        sqlx::query_as("SELECT content_enc,content_sha256 FROM config_resources WHERE id=$1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        state.secrets.decrypt_bytes(&cipher).unwrap(),
        [0_u8, 255, 10, 13]
    );
    assert_eq!(
        digest,
        hex::encode(sha2::Sha256::digest([0_u8, 255, 10, 13]))
    );
    assert_eq!(response["content_sha256"], digest);
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM config_resources WHERE name='data.bin'"
        )
        .await,
        1
    );
    sqlx::query("UPDATE nodes SET state='deleting' WHERE id=$1")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state).await;
    let replay = post(&client, &base, "same", "data.bin").await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(replay.bytes().await.unwrap(), first);
    assert_eq!(snapshot(&state).await, before);
    assert_eq!(
        post(&client, &base, "same", "other.bin").await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        post(&client, &base, "new-key", "other.bin").await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}

#[tokio::test]
async fn deletion_replays_after_resource_and_node_cascade_and_changed_body_conflicts() {
    let (state, node, resource) = fixture().await;
    let (base, server) = serve(state.clone(), &node).await;
    let client = client();
    let first = delete(&client, &base, "delete", &resource).await;
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    assert!(first.bytes().await.unwrap().is_empty());
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state).await;
    let replay = delete(&client, &base, "delete", &resource).await;
    assert_eq!(replay.status(), StatusCode::NO_CONTENT);
    assert!(replay.bytes().await.unwrap().is_empty());
    assert_eq!(snapshot(&state).await, before);
    assert_eq!(
        client
            .delete(format!("{base}/{resource}"))
            .bearer_auth("admin")
            .header("idempotency-key", "delete")
            .body("different")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}

#[tokio::test]
async fn referenced_resource_stays_protected_and_failed_key_can_retry_after_all_pins_are_removed() {
    let (state, node, resource) = fixture().await;
    let (base, server) = serve(state.clone(), &node).await;
    let client = client();
    let input = serde_json::from_value(
        json!({"expected_revision":1,"config":{"acl":{"file":format!("resource://{resource}")}}}),
    )
    .unwrap();
    let _ = crate::api::nodes::patch(
        State(state.clone()),
        axum::extract::Path(node.clone()),
        Json(input),
    )
    .await
    .unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_resource_references").await,
        2
    );
    let before = snapshot(&state).await;
    assert_eq!(
        delete(&client, &base, "referenced", &resource)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state).await, before);
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        0
    );
    let input = serde_json::from_value(json!({"expected_revision":2,"config":{}})).unwrap();
    let _ = crate::api::nodes::patch(
        State(state.clone()),
        axum::extract::Path(node.clone()),
        Json(input),
    )
    .await
    .unwrap();
    sqlx::query("DELETE FROM config_versions WHERE node_id=$1 AND revision=2")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_resource_references").await,
        0
    );
    assert_eq!(
        delete(&client, &base, "referenced", &resource)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    server.abort();
}

#[tokio::test]
async fn receipt_failure_rolls_back_resources_audit_and_cache_then_native_retry_commits_once() {
    for code in ["40001", "40P01"] {
        let (state, node, resource) = fixture().await;
        let (base, server) = serve(state.clone(), &node).await;
        let client = client();
        sqlx::raw_sql("CREATE FUNCTION reject_resource_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER reject_resource_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_resource_receipt()")
            .execute(&state.pool).await.unwrap();
        let before = snapshot(&state).await;
        assert_eq!(
            post(&client, &base, "upload", "new.bin").await.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(snapshot(&state).await, before);
        assert_eq!(
            delete(&client, &base, "delete", &resource).await.status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(snapshot(&state).await, before);
        sqlx::raw_sql(&format!("DROP TRIGGER reject_resource_receipt ON management_receipts; CREATE SEQUENCE resource_receipt_attempt; CREATE FUNCTION retry_resource_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF mod(nextval('resource_receipt_attempt'),2)=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_resource_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION retry_resource_receipt()"))
            .execute(&state.pool).await.unwrap();
        assert_eq!(
            post(&client, &base, "upload", "new.bin").await.status(),
            StatusCode::CREATED
        );
        assert_eq!(
            delete(&client, &base, "delete", &resource).await.status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            count(&state, "SELECT count(*) FROM config_resources").await,
            1
        );
        assert_eq!(
            count(&state, "SELECT count(*) FROM management_receipts").await,
            2
        );
        assert_eq!(
            count(&state, "SELECT last_value FROM resource_receipt_attempt").await,
            4
        );
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM audit_records WHERE action='node.resource_uploaded'"
            )
            .await,
            2
        );
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM audit_records WHERE action='node.resource_deleted'"
            )
            .await,
            1
        );
        server.abort();
    }
}

#[tokio::test]
async fn distinct_keys_cannot_duplicate_a_name_and_missing_keys_or_invalid_payloads_do_not_write() {
    let (state, node, _) = fixture().await;
    let (base, server) = serve(state.clone(), &node).await;
    let client = client();
    let before = snapshot(&state).await;
    assert_eq!(
        client
            .post(&base)
            .bearer_auth("admin")
            .json(&upload("new.bin"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(&client, &base, "bad", "../invalid").await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(snapshot(&state).await, before);
    let (a, b) = tokio::join!(
        post(&client, &base, "one", "same.bin"),
        post(&client, &base, "two", "same.bin")
    );
    let statuses = [a.status(), b.status()];
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CREATED)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM config_resources WHERE name='same.bin'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    server.abort();
}
