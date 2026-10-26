use super::*;
use reqwest::{Client, Method};

async fn fixture(with_node: bool) -> (AppState, String) {
    let (state, id) = super::super::transaction_tests::fixture().await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('admin',$1,'Admin',clock_timestamp(),clock_timestamp())")
        .bind(crate::security::token_digest("admin")).execute(&state.pool).await.unwrap();
    if with_node {
        super::super::transaction_tests::node(&state, &id, "Target").await;
    }
    (state, id)
}

async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v2", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, crate::api::router(state))
            .await
            .unwrap()
    });
    (base, task)
}

fn client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
}

fn creation(label: &str) -> Value {
    json!({"name":label,"kind":"api_token","payload":{"token":"first-secret"}})
}
fn patch_body(revision: i64) -> Value {
    json!({"expected_revision":revision,"name":"Updated","archived":false})
}
fn publication(revision: i64) -> Value {
    json!({"expected_revision":revision,"payload":{"secret":"published-secret"}})
}

async fn send(
    client: &Client,
    method: Method,
    url: &str,
    key: Option<&str>,
    body: Option<&Value>,
) -> reqwest::Response {
    let mut request = client.request(method, url).bearer_auth("admin");
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    if let Some(body) = body {
        request = request.json(body);
    }
    request.send().await.unwrap()
}

struct Saved {
    method: Method,
    url: String,
    key: String,
    body: Option<Value>,
    status: StatusCode,
    bytes: Vec<u8>,
}
async fn success(
    client: &Client,
    method: Method,
    url: String,
    key: &str,
    body: Option<Value>,
    status: StatusCode,
) -> (Value, Saved) {
    let response = send(client, method.clone(), &url, Some(key), body.as_ref()).await;
    assert_eq!(response.status(), status);
    let bytes = response.bytes().await.unwrap().to_vec();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (
        value,
        Saved {
            method,
            url,
            key: key.into(),
            body,
            status,
            bytes,
        },
    )
}
async fn replay_saved(client: &Client, saved: &Saved) {
    let response = send(
        client,
        saved.method.clone(),
        &saved.url,
        Some(&saved.key),
        saved.body.as_ref(),
    )
    .await;
    assert_eq!(response.status(), saved.status);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.bytes().await.unwrap().as_ref(), saved.bytes);
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn snapshot(state: &AppState) -> Value {
    let tables = [
        "credentials",
        "credential_versions",
        "credential_batches",
        "credential_batch_items",
        "dns_credential_batch_items",
        "nodes",
        "jobs",
        "job_events",
        "audit_records",
        "credential_reference_edges",
        "management_receipts",
        "cache_bucket_versions",
    ];
    let parts: Vec<String> = tables.iter().map(|t|format!("SELECT '{t}' AS kind,coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) AS data FROM {t} t")).collect();
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT jsonb_object_agg(kind,data) FROM ({}) snapshots",
        parts.join(" UNION ALL ")
    )))
    .fetch_one(&state.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn all_five_credential_mutations_replay_after_the_credential_and_batch_are_deleted() {
    let (state, _) = fixture(false).await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let mut saved = vec![];
    let (created, receipt) = success(
        &client,
        Method::POST,
        format!("{base}/credentials"),
        "create",
        Some(creation("Token")),
        StatusCode::CREATED,
    )
    .await;
    saved.push(receipt);
    let id = created["id"].as_str().unwrap();
    let cipher: String =
        sqlx::query_scalar("SELECT payload_enc FROM credential_versions WHERE credential_id=$1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&state.secrets.decrypt(&cipher).unwrap()).unwrap(),
        json!({"token":"first-secret"})
    );
    let detail = send(
        &client,
        Method::GET,
        &format!("{base}/credentials/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(detail.status(), StatusCode::OK);
    assert!(!detail.text().await.unwrap().contains("first-secret"));
    let (_, receipt) = success(
        &client,
        Method::PATCH,
        format!("{base}/credentials/{id}"),
        "patch",
        Some(patch_body(1)),
        StatusCode::OK,
    )
    .await;
    saved.push(receipt);
    let (published, receipt) = success(
        &client,
        Method::POST,
        format!("{base}/credentials/{id}/versions"),
        "publish",
        Some(json!({"expected_revision":2,"payload":{"token":"second-secret"}})),
        StatusCode::ACCEPTED,
    )
    .await;
    saved.push(receipt);
    let batch = published["batch_id"].as_str().unwrap();
    let (_, receipt) = success(
        &client,
        Method::POST,
        format!("{base}/credential-batches/{batch}/retry"),
        "retry",
        None,
        StatusCode::ACCEPTED,
    )
    .await;
    saved.push(receipt);
    let (_, receipt) = success(
        &client,
        Method::DELETE,
        format!("{base}/credentials/{id}?expected_revision=3"),
        "delete",
        None,
        StatusCode::NO_CONTENT,
    )
    .await;
    saved.push(receipt);
    assert_eq!(count(&state, "SELECT count(*) FROM credentials").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        5
    );
    let before = snapshot(&state).await;
    for receipt in saved {
        replay_saved(&client, &receipt).await;
    }
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}

#[tokio::test]
async fn concurrent_publication_has_one_version_batch_job_and_event_and_replays_after_archival() {
    let (state, id) = fixture(true).await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let url = format!("{base}/credentials/{id}/versions");
    let body = publication(1);
    let (a, b) = tokio::join!(
        send(&client, Method::POST, &url, Some("publish"), Some(&body)),
        send(&client, Method::POST, &url, Some("publish"), Some(&body))
    );
    assert_eq!(a.status(), StatusCode::ACCEPTED);
    assert_eq!(b.status(), StatusCode::ACCEPTED);
    let bytes = a.bytes().await.unwrap().to_vec();
    assert_eq!(b.bytes().await.unwrap().as_ref(), bytes);
    let response: Value = serde_json::from_slice(&bytes).unwrap();
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
    assert_eq!(count(&state, "SELECT count(*) FROM job_events").await, 1);
    let cipher: String = sqlx::query_scalar(
        "SELECT payload_enc FROM credential_versions WHERE credential_id=$1 AND version=2",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&state.secrets.decrypt(&cipher).unwrap()).unwrap(),
        body["payload"]
    );
    sqlx::query("UPDATE credentials SET archived=TRUE WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state).await;
    replay_saved(
        &client,
        &Saved {
            method: Method::POST,
            url: url.clone(),
            key: "publish".into(),
            body: Some(body),
            status: StatusCode::ACCEPTED,
            bytes,
        },
    )
    .await;
    assert_eq!(
        send(
            &client,
            Method::POST,
            &url,
            Some("publish"),
            Some(&publication(99))
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}

#[tokio::test]
async fn retried_batch_replay_does_not_enqueue_again_after_the_replacement_job_fails() {
    let (state, id) = fixture(true).await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let (published, _) = success(
        &client,
        Method::POST,
        format!("{base}/credentials/{id}/versions"),
        "publish",
        Some(publication(1)),
        StatusCode::ACCEPTED,
    )
    .await;
    let batch = published["batch_id"].as_str().unwrap();
    sqlx::query("UPDATE jobs SET status='failed',stage='failed'")
        .execute(&state.pool)
        .await
        .unwrap();
    let url = format!("{base}/credential-batches/{batch}/retry");
    let (result, receipt) = success(
        &client,
        Method::POST,
        url.clone(),
        "retry",
        None,
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(result["affected_count"], 1);
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 2);
    sqlx::query("UPDATE jobs SET status='failed',stage='failed'")
        .execute(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state).await;
    replay_saved(&client, &receipt).await;
    assert_eq!(
        client
            .post(&url)
            .bearer_auth("admin")
            .header("idempotency-key", "retry")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state).await, before);
    let _ = success(
        &client,
        Method::POST,
        url,
        "new-retry",
        None,
        StatusCode::ACCEPTED,
    )
    .await;
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 3);
    server.abort();
}

#[tokio::test]
async fn receipt_failures_roll_back_every_mutation_and_native_errors_commit_each_once() {
    for code in ["40001", "40P01"] {
        let (state, id) = fixture(true).await;
        let (base, server) = serve(state.clone()).await;
        let client = client();
        let (unused, _) = success(
            &client,
            Method::POST,
            format!("{base}/credentials"),
            "setup-create",
            Some(creation("Unused")),
            StatusCode::CREATED,
        )
        .await;
        let unused = unused["id"].as_str().unwrap();
        let (published, _) = success(
            &client,
            Method::POST,
            format!("{base}/credentials/{id}/versions"),
            "setup-publish",
            Some(publication(1)),
            StatusCode::ACCEPTED,
        )
        .await;
        let batch = published["batch_id"].as_str().unwrap();
        sqlx::query("UPDATE jobs SET status='failed',stage='failed'")
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::raw_sql("CREATE FUNCTION fail_credential_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER fail_credential_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION fail_credential_receipt()")
            .execute(&state.pool).await.unwrap();
        let cases = vec![
            (
                Method::POST,
                format!("{base}/credentials"),
                "create",
                Some(creation("Matrix")),
                StatusCode::CREATED,
            ),
            (
                Method::PATCH,
                format!("{base}/credentials/{id}"),
                "patch",
                Some(patch_body(2)),
                StatusCode::OK,
            ),
            (
                Method::POST,
                format!("{base}/credential-batches/{batch}/retry"),
                "retry",
                None,
                StatusCode::ACCEPTED,
            ),
            (
                Method::POST,
                format!("{base}/credentials/{id}/versions"),
                "publish",
                Some(publication(2)),
                StatusCode::ACCEPTED,
            ),
            (
                Method::DELETE,
                format!("{base}/credentials/{unused}?expected_revision=1"),
                "delete",
                None,
                StatusCode::NO_CONTENT,
            ),
        ];
        let before = snapshot(&state).await;
        for (method, url, key, body, _) in &cases {
            assert_eq!(
                send(&client, method.clone(), url, Some(key), body.as_ref())
                    .await
                    .status(),
                StatusCode::INTERNAL_SERVER_ERROR
            );
            assert_eq!(snapshot(&state).await, before);
        }
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TRIGGER fail_credential_receipt ON management_receipts; CREATE SEQUENCE credential_receipt_attempt; CREATE FUNCTION retry_credential_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF mod(nextval('credential_receipt_attempt'),2)=1 THEN RAISE EXCEPTION 'native conflict' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_credential_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION retry_credential_receipt()")))
            .execute(&state.pool).await.unwrap();
        for (method, url, key, mut body, status) in cases {
            if key == "publish" {
                body = Some(publication(3));
            }
            let (_, receipt) = success(&client, method, url, key, body, status).await;
            replay_saved(&client, &receipt).await;
        }
        assert_eq!(
            count(&state, "SELECT last_value FROM credential_receipt_attempt").await,
            10
        );
        assert_eq!(
            count(&state, "SELECT count(*) FROM management_receipts").await,
            7
        );
        assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 3);
        server.abort();
    }
}

#[tokio::test]
async fn missing_keys_revision_conflicts_and_live_references_cannot_consume_receipts() {
    let (state, id) = fixture(true).await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let url = format!("{base}/credentials/{id}");
    let cases = vec![
        (
            Method::POST,
            format!("{base}/credentials"),
            Some(creation("Missing")),
        ),
        (Method::PATCH, url.clone(), Some(patch_body(1))),
        (
            Method::POST,
            format!("{url}/versions"),
            Some(publication(1)),
        ),
        (Method::DELETE, format!("{url}?expected_revision=1"), None),
        (
            Method::POST,
            format!("{base}/credential-batches/missing/retry"),
            None,
        ),
    ];
    let before = snapshot(&state).await;
    for (method, url, body) in cases {
        assert_eq!(
            send(&client, method, &url, None, body.as_ref())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        send(
            &client,
            Method::PATCH,
            &url,
            Some("stale-edit"),
            Some(&patch_body(99))
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        send(
            &client,
            Method::POST,
            &format!("{url}/versions"),
            Some("stale-publish"),
            Some(&publication(99))
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        send(
            &client,
            Method::DELETE,
            &format!("{url}?expected_revision=1"),
            Some("referenced"),
            None
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}
