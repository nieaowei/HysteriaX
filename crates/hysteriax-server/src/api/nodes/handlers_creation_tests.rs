use super::*;
use reqwest::{Client, Method};
use sqlx::Row;

async fn fixture() -> (AppState, String) {
    let (state, ssh, _, _) = crate::dns::tests::fixture().await;
    crate::credentials::reference_index::initialize(&state.pool, &state.secrets)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('operator',$1,'Operator',clock_timestamp(),clock_timestamp()),('other',$2,'Other',clock_timestamp(),clock_timestamp())")
        .bind(crate::security::token_digest("admin")).bind(crate::security::token_digest("other")).execute(&state.pool).await.unwrap();
    (state, ssh)
}
fn body(ssh: &str, host: Option<&str>) -> Value {
    let allocation =
        host.map(|host| json!({"zone_id":"zone","mode":"manual","hostname":host,"ipv4":"8.8.8.8"}));
    json!({"name":"Node","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_credential_id":ssh,"ssh_credential_version":1,"public_host":"node.example.test","public_port":443,"listen_addr":":443","config":{},"dns_allocation":allocation})
}
async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v2/nodes", listener.local_addr().unwrap());
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
async fn send(
    client: &Client,
    method: Method,
    url: &str,
    actor: &str,
    key: &str,
    body: &Value,
) -> reqwest::Response {
    client
        .request(method, url)
        .bearer_auth(actor)
        .header("idempotency-key", key)
        .json(body)
        .send()
        .await
        .unwrap()
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn snapshot(state: &AppState) -> Value {
    let tables = [
        "nodes",
        "node_packages",
        "config_versions",
        "credential_reference_edges",
        "dns_records",
        "dns_bindings",
        "dns_operations",
        "jobs",
        "job_events",
        "audit_records",
        "management_receipts",
        "cache_bucket_versions",
    ];
    let parts:Vec<String>=tables.iter().map(|t|format!("SELECT '{t}' AS kind,coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) AS data FROM {t} t")).collect();
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT jsonb_object_agg(kind,data) FROM ({}) snapshots",
        parts.join(" UNION ALL ")
    )))
    .fetch_one(&state.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn creation_and_patch_racers_return_identical_receipts_and_replay_after_node_deletion() {
    let (state, ssh) = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let input = body(&ssh, None);
    let (a, b) = tokio::join!(
        send(&client, Method::POST, &base, "admin", "create", &input),
        send(&client, Method::POST, &base, "admin", "create", &input)
    );
    assert_eq!(a.status(), StatusCode::CREATED);
    assert_eq!(b.status(), StatusCode::CREATED);
    let created = a.bytes().await.unwrap();
    assert_eq!(b.bytes().await.unwrap(), created);
    let value: Value = serde_json::from_slice(&created).unwrap();
    let id = value["node"]["id"].as_str().unwrap();
    let token = value["node_auth_token"].as_str().unwrap();
    let row = sqlx::query("SELECT node_token_hash,node_token_enc FROM nodes WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        row.get::<String, _>("node_token_hash"),
        crate::security::token_digest(token)
    );
    assert_eq!(
        state
            .secrets
            .decrypt(&row.get::<String, _>("node_token_enc"))
            .unwrap(),
        token
    );
    let receipt: String = sqlx::query_scalar("SELECT response_enc FROM management_receipts")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(!receipt.contains(token));
    let update = json!({"expected_revision":1,"traffic_stats_port":12000});
    let url = format!("{base}/{id}");
    let (a, b) = tokio::join!(
        send(&client, Method::PATCH, &url, "admin", "patch", &update),
        send(&client, Method::PATCH, &url, "admin", "patch", &update)
    );
    assert_eq!(a.status(), StatusCode::OK);
    assert_eq!(b.status(), StatusCode::OK);
    let patched = a.bytes().await.unwrap();
    assert_eq!(b.bytes().await.unwrap(), patched);
    assert_eq!(count(&state, "SELECT count(*) FROM nodes").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='sync'").await,
        1
    );
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state).await;
    assert_eq!(
        send(&client, Method::POST, &base, "admin", "create", &input)
            .await
            .bytes()
            .await
            .unwrap(),
        created
    );
    assert_eq!(
        send(&client, Method::PATCH, &url, "admin", "patch", &update)
            .await
            .bytes()
            .await
            .unwrap(),
        patched
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}

#[tokio::test]
async fn dns_child_keys_are_scoped_internally_and_legacy_body_keys_cannot_collide_across_admins() {
    let (state, ssh) = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    for (actor, host) in [("admin", "one.example.test"), ("other", "two.example.test")] {
        let mut input = body(&ssh, Some(host));
        input["dns_allocation"]["idempotency_key"] = json!("legacy-shared-key");
        let first = send(&client, Method::POST, &base, actor, "same-key", &input).await;
        assert_eq!(first.status(), StatusCode::CREATED);
        let bytes = first.bytes().await.unwrap();
        let before = snapshot(&state).await;
        assert_eq!(
            send(&client, Method::POST, &base, actor, "same-key", &input)
                .await
                .bytes()
                .await
                .unwrap(),
            bytes
        );
        assert_eq!(snapshot(&state).await, before);
    }
    assert_eq!(count(&state, "SELECT count(*) FROM nodes").await, 2);
    assert_eq!(count(&state, "SELECT count(*) FROM dns_bindings").await, 2);
    assert_eq!(count(&state,"SELECT count(*) FROM dns_operations WHERE action='allocation' AND idempotency_key LIKE 'v2:%'").await,2);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM dns_operations WHERE idempotency_key='legacy-shared-key'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        2
    );
    server.abort();
}

#[tokio::test]
async fn receipt_failure_rolls_back_dns_creation_and_patch_then_native_retry_commits_once() {
    for code in ["40001", "40P01"] {
        let (state, ssh) = fixture().await;
        let (base, server) = serve(state.clone()).await;
        let client = client();
        let input = body(&ssh, Some("atomic.example.test"));
        sqlx::raw_sql("CREATE FUNCTION fail_creation_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER fail_creation_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION fail_creation_receipt()")
            .execute(&state.pool).await.unwrap();
        let before = snapshot(&state).await;
        assert_eq!(
            send(&client, Method::POST, &base, "admin", "create", &input)
                .await
                .status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(snapshot(&state).await, before);
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP TRIGGER fail_creation_receipt ON management_receipts; CREATE SEQUENCE creation_attempt; CREATE FUNCTION retry_creation_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF mod(nextval('creation_attempt'),2)=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_creation_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION retry_creation_receipt()")))
            .execute(&state.pool).await.unwrap();
        let response = send(&client, Method::POST, &base, "admin", "create", &input).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let created = response.bytes().await.unwrap();
        let value: Value = serde_json::from_slice(&created).unwrap();
        let id = value["node"]["id"].as_str().unwrap();
        let url = format!("{base}/{id}");
        let update = json!({"expected_revision":1,"traffic_stats_port":12000});
        sqlx::raw_sql("DROP TRIGGER retry_creation_receipt ON management_receipts; CREATE TRIGGER fail_creation_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION fail_creation_receipt()")
            .execute(&state.pool).await.unwrap();
        let before = snapshot(&state).await;
        assert_eq!(
            send(&client, Method::PATCH, &url, "admin", "patch", &update)
                .await
                .status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(snapshot(&state).await, before);
        sqlx::raw_sql("DROP TRIGGER fail_creation_receipt ON management_receipts; CREATE TRIGGER retry_creation_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION retry_creation_receipt()")
            .execute(&state.pool).await.unwrap();
        assert_eq!(
            send(&client, Method::PATCH, &url, "admin", "patch", &update)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            send(&client, Method::POST, &base, "admin", "create", &input)
                .await
                .bytes()
                .await
                .unwrap(),
            created
        );
        assert_eq!(
            count(&state, "SELECT last_value FROM creation_attempt").await,
            4
        );
        assert_eq!(count(&state, "SELECT count(*) FROM nodes").await, 1);
        assert_eq!(
            count(&state, "SELECT count(*) FROM config_versions").await,
            2
        );
        assert_eq!(
            count(&state, "SELECT count(*) FROM management_receipts").await,
            2
        );
        assert_eq!(count(&state, "SELECT count(*) FROM dns_bindings").await, 1);
        server.abort();
    }
}

#[tokio::test]
async fn missing_header_and_changed_request_do_not_create_or_edit_a_node() {
    let (state, ssh) = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let input = body(&ssh, None);
    let before = snapshot(&state).await;
    assert_eq!(
        client
            .post(&base)
            .bearer_auth("admin")
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(snapshot(&state).await, before);
    let response = send(&client, Method::POST, &base, "admin", "create", &input).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let value: Value = response.json().await.unwrap();
    let id = value["node"]["id"].as_str().unwrap();
    let url = format!("{base}/{id}");
    let before = snapshot(&state).await;
    assert_eq!(
        client
            .patch(&url)
            .bearer_auth("admin")
            .json(&json!({"expected_revision":1,"name":"new"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut changed = input;
    changed["name"] = json!("different");
    assert_eq!(
        send(&client, Method::POST, &base, "admin", "create", &changed)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}
