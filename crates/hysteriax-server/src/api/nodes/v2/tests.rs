use super::*;
use reqwest::{Client, Method};

async fn fixture(history: bool, quota: bool) -> (AppState, String) {
    let (state, node) = super::super::transaction_tests::fixture().await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('operator',$1,'Operator',clock_timestamp(),clock_timestamp())")
        .bind(crate::security::token_digest("admin")).execute(&state.pool).await.unwrap();
    if history {
        super::super::transaction_tests::history(&state, &node).await;
        let mut tx = state.pool.begin().await.unwrap();
        sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) SELECT node_id||'-five',node_id,5,config_enc,content_sha256,clock_timestamp() FROM config_versions WHERE node_id=$1 AND revision=4")
            .bind(&node).execute(&mut *tx).await.unwrap();
        crate::credentials::reference_index::flush_node_on(&mut tx, &state.secrets, &node)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    if quota {
        let mut tx = state.pool.begin().await.unwrap();
        crate::node_limits::save(
            &mut tx,
            &node,
            &crate::node_limits::Package {
                quota_bytes: Some(100),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    (state, node)
}

async fn serve(state: AppState, node: &str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/api/v2/nodes/{node}",
        listener.local_addr().unwrap()
    );
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
) -> Saved {
    let response = send(client, method.clone(), &url, Some(key), body.as_ref()).await;
    assert_eq!(response.status(), status);
    let bytes = response.bytes().await.unwrap().to_vec();
    Saved {
        method,
        url,
        key: key.into(),
        body,
        status,
        bytes,
    }
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
    sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn snapshot(state: &AppState) -> Value {
    let tables = [
        "nodes",
        "node_packages",
        "node_alerts",
        "node_limit_epochs",
        "background_work",
        "config_versions",
        "jobs",
        "job_events",
        "audit_records",
        "credential_reference_edges",
        "kick_requests",
        "management_receipts",
        "cache_bucket_versions",
    ];
    let parts:Vec<String>=tables.iter().map(|t|format!("SELECT '{t}' AS kind,coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) AS data FROM {t} t")).collect();
    sqlx::query_scalar(&format!(
        "SELECT jsonb_object_agg(kind,data) FROM ({}) snapshots",
        parts.join(" UNION ALL ")
    ))
    .fetch_one(&state.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn four_actions_share_no_business_execution_on_replay_and_keep_the_original_rollback_target()
{
    let (state, node) = fixture(true, false).await;
    let (base, server) = serve(state.clone(), &node).await;
    let client = client();
    let body = json!({"expected_revision":5});
    let mut receipts = vec![];
    for kind in ["deploy", "sync", "rollback", "ssh-test"] {
        let url = format!("{base}/{kind}");
        let (a, b) = tokio::join!(
            send(&client, Method::POST, &url, Some("shared-key"), Some(&body)),
            send(&client, Method::POST, &url, Some("shared-key"), Some(&body))
        );
        assert_eq!(a.status(), StatusCode::ACCEPTED);
        assert_eq!(b.status(), StatusCode::ACCEPTED);
        let bytes = a.bytes().await.unwrap().to_vec();
        assert_eq!(b.bytes().await.unwrap().as_ref(), bytes);
        receipts.push(Saved {
            method: Method::POST,
            url,
            key: "shared-key".into(),
            body: Some(body.clone()),
            status: StatusCode::ACCEPTED,
            bytes,
        });
    }
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 4);
    assert_eq!(count(&state, "SELECT count(*) FROM job_events").await, 4);
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        4
    );
    assert_eq!(
        count(
            &state,
            "SELECT target_revision FROM jobs WHERE kind='rollback'"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE actor='operator'"
        )
        .await,
        4
    );
    sqlx::query("UPDATE nodes SET state='deleting' WHERE id=$1")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET status='succeeded',stage='succeeded'")
        .execute(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state).await;
    for receipt in receipts {
        replay_saved(&client, &receipt).await;
    }
    assert_eq!(
        send(
            &client,
            Method::POST,
            &format!("{base}/deploy"),
            Some("shared-key"),
            Some(&json!({"expected_revision":99}))
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}

#[tokio::test]
async fn immediate_delete_and_record_removal_notify_only_after_commit_and_replay_after_cascade() {
    for record_only in [false, true] {
        let (state, node) = fixture(false, false).await;
        let (base, server) = serve(state.clone(), &node).await;
        let client = client();
        let mut notifications = state.removed_nodes.subscribe();
        let path = if record_only {
            format!("{base}/record")
        } else {
            base
        };
        let receipt = success(
            &client,
            Method::DELETE,
            format!("{path}?expected_revision=1"),
            "remove",
            None,
            StatusCode::NO_CONTENT,
        )
        .await;
        assert!(receipt.bytes.is_empty());
        assert_eq!(notifications.recv().await.unwrap(), node);
        assert_eq!(count(&state, "SELECT count(*) FROM nodes").await, 0);
        let before = snapshot(&state).await;
        replay_saved(&client, &receipt).await;
        assert_eq!(snapshot(&state).await, before);
        assert!(matches!(
            notifications.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM audit_records WHERE actor='operator'"
            )
            .await,
            1
        );
        server.abort();
    }
}

#[tokio::test]
async fn uninstall_receipt_replays_the_same_job_after_its_node_is_gone() {
    let (state, node) = fixture(false, false).await;
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let (base, server) = serve(state.clone(), &node).await;
    let client = client();
    let url = format!("{base}?expected_revision=1");
    let (a, b) = tokio::join!(
        send(&client, Method::DELETE, &url, Some("uninstall"), None),
        send(&client, Method::DELETE, &url, Some("uninstall"), None)
    );
    assert_eq!(a.status(), StatusCode::ACCEPTED);
    assert_eq!(b.status(), StatusCode::ACCEPTED);
    let bytes = a.bytes().await.unwrap().to_vec();
    assert_eq!(b.bytes().await.unwrap().as_ref(), bytes);
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='uninstall'").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM nodes WHERE state='deleting'").await,
        1
    );
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state).await;
    replay_saved(
        &client,
        &Saved {
            method: Method::DELETE,
            url,
            key: "uninstall".into(),
            body: None,
            status: StatusCode::ACCEPTED,
            bytes,
        },
    )
    .await;
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}

#[tokio::test]
async fn queued_record_cleanup_replay_preserves_existing_progress_and_returns_empty_accepted_body()
{
    let (state, node) = fixture(false, false).await;
    sqlx::query("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,sampled_at) SELECT $1,'history',1,1,clock_timestamp()-i*interval '1 second' FROM generate_series(1,1001)i")
        .bind(&node).execute(&state.pool).await.unwrap();
    let (base, server) = serve(state.clone(), &node).await;
    let client = client();
    let receipt = success(
        &client,
        Method::DELETE,
        format!("{base}/record?expected_revision=1"),
        "cleanup",
        None,
        StatusCode::ACCEPTED,
    )
    .await;
    assert!(receipt.bytes.is_empty());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE kind='record-remove'"
        )
        .await,
        1
    );
    sqlx::query("UPDATE jobs SET result_json=$1 WHERE kind='record-remove'")
        .bind(json!({"stage":"removing_history","result":{"removed_history_rows":512}}))
        .execute(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state).await;
    replay_saved(&client, &receipt).await;
    assert_eq!(snapshot(&state).await, before);
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_network_samples").await,
        1001
    );
    server.abort();
}

#[tokio::test]
async fn usage_replay_keeps_one_generation_history_and_restriction_work_and_preserves_later_usage()
{
    let (state, node) = fixture(false, true).await;
    let generation = count(&state, "SELECT generation FROM node_packages").await;
    let (base, server) = serve(state.clone(), &node).await;
    let client = client();
    let body = json!({"expected_revision":1,"usage_bytes":150,"reset":true});
    let url = format!("{base}/usage");
    let (a, b) = tokio::join!(
        send(&client, Method::PUT, &url, Some("usage"), Some(&body)),
        send(&client, Method::PUT, &url, Some("usage"), Some(&body))
    );
    assert_eq!(a.status(), StatusCode::OK);
    assert_eq!(b.status(), StatusCode::OK);
    let bytes = a.bytes().await.unwrap().to_vec();
    assert_eq!(b.bytes().await.unwrap().as_ref(), bytes);
    assert_eq!(
        count(&state, "SELECT generation FROM node_packages").await,
        generation + 1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='node-limit-dispatch'"
        )
        .await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM audit_records WHERE action='node.usage_reset' AND actor='operator'").await,1);
    sqlx::query("UPDATE node_packages SET usage_bytes=175")
        .execute(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state).await;
    replay_saved(
        &client,
        &Saved {
            method: Method::PUT,
            url,
            key: "usage".into(),
            body: Some(body),
            status: StatusCode::OK,
            bytes,
        },
    )
    .await;
    assert_eq!(snapshot(&state).await, before);
    assert_eq!(
        count(&state, "SELECT usage_bytes FROM node_packages").await,
        175
    );
    server.abort();
}

#[tokio::test]
async fn receipt_failure_rolls_back_all_seven_mutations_and_native_conflicts_keep_one_commit_each()
{
    for code in ["40001", "40P01"] {
        let (state, node) = fixture(true, true).await;
        let (base, server) = serve(state.clone(), &node).await;
        let client = client();
        let mut notifications = state.removed_nodes.subscribe();
        let mut cases = vec![];
        for kind in ["deploy", "sync", "rollback", "ssh-test"] {
            cases.push((
                Method::POST,
                format!("{base}/{kind}"),
                kind,
                Some(json!({"expected_revision":5})),
                StatusCode::ACCEPTED,
            ));
        }
        cases.push((
            Method::PUT,
            format!("{base}/usage"),
            "usage",
            Some(json!({"expected_revision":5,"usage_bytes":150})),
            StatusCode::OK,
        ));
        cases.push((
            Method::DELETE,
            format!("{base}?expected_revision=5"),
            "delete",
            None,
            StatusCode::ACCEPTED,
        ));
        cases.push((
            Method::DELETE,
            format!("{base}/record?expected_revision=5"),
            "record",
            None,
            StatusCode::NO_CONTENT,
        ));
        sqlx::raw_sql("CREATE FUNCTION fail_node_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER fail_node_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION fail_node_receipt()")
            .execute(&state.pool).await.unwrap();
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
        assert!(matches!(
            notifications.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        sqlx::raw_sql(&format!("DROP TRIGGER fail_node_receipt ON management_receipts; CREATE SEQUENCE node_receipt_attempt; CREATE FUNCTION retry_node_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF mod(nextval('node_receipt_attempt'),2)=1 THEN RAISE EXCEPTION 'native conflict' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_node_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION retry_node_receipt()"))
            .execute(&state.pool).await.unwrap();
        let mut receipts = vec![];
        for (method, mut url, key, body, status) in cases {
            if key == "delete" || key == "record" {
                url = url.replace("expected_revision=5", "expected_revision=6");
            }
            receipts.push(success(&client, method, url, key, body, status).await);
        }
        assert_eq!(notifications.recv().await.unwrap(), node);
        assert_eq!(
            count(&state, "SELECT last_value FROM node_receipt_attempt").await,
            14
        );
        assert_eq!(
            count(&state, "SELECT count(*) FROM management_receipts").await,
            7
        );
        assert_eq!(count(&state, "SELECT count(*) FROM nodes").await, 0);
        let before = snapshot(&state).await;
        for receipt in receipts {
            replay_saved(&client, &receipt).await;
        }
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn missing_keys_and_stale_node_revision_leave_no_work_or_receipt() {
    let (state, node) = fixture(true, false).await;
    let (base, server) = serve(state.clone(), &node).await;
    let client = client();
    let before = snapshot(&state).await;
    for kind in ["deploy", "sync", "rollback", "ssh-test"] {
        let url = format!("{base}/{kind}");
        assert_eq!(
            send(
                &client,
                Method::POST,
                &url,
                None,
                Some(&json!({"expected_revision":5}))
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send(
                &client,
                Method::POST,
                &url,
                Some("stale"),
                Some(&json!({"expected_revision":99}))
            )
            .await
            .status(),
            StatusCode::CONFLICT
        );
    }
    for path in [&base, &format!("{base}/record")] {
        assert_eq!(
            send(
                &client,
                Method::DELETE,
                &format!("{path}?expected_revision=5"),
                None,
                None
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send(
                &client,
                Method::DELETE,
                &format!("{path}?expected_revision=99"),
                Some("stale"),
                None
            )
            .await
            .status(),
            StatusCode::CONFLICT
        );
    }
    assert_eq!(
        send(
            &client,
            Method::PUT,
            &format!("{base}/usage"),
            None,
            Some(&json!({"expected_revision":5,"usage_bytes":150}))
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}
