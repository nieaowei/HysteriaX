use super::*;
use reqwest::{Client, Method};

#[tokio::test]
async fn an_orphaned_dns_operation_cannot_be_promoted_into_a_management_success_receipt() {
    let (state, case) = fixture(3).await;
    let id = case.path.split('/').nth(2).unwrap().to_owned();
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", "orphan".parse().unwrap());
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        "operator",
        "dns.connections.verify",
        &headers,
        json!({"method":"POST","uri":format!("/api/v2/dns{}",case.path),"id":id,"body":case.body}),
    )
    .unwrap();
    let (status, Json(_)) = super::super::verify_connection(
        State(state.clone()),
        Path(id),
        Json(ActionInput {
            expected_revision: 1,
            idempotency_key: identity.operation_key(&state.secrets).unwrap(),
        }),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED);
    let before = snapshot(&state).await;
    let (base, server) = serve(state.clone()).await;
    assert_eq!(
        send(&client(), &base, &case, "admin", Some("orphan"))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state).await, before);
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        0
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
    server.abort();
}

struct Case {
    method: Method,
    path: String,
    body: Value,
    status: StatusCode,
}
async fn fixture(kind: usize) -> (AppState, Case) {
    let (state, _, connection, _) = crate::dns::tests::fixture().await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('operator',$1,'Operator',clock_timestamp(),clock_timestamp()),('other',$2,'Other',clock_timestamp(),clock_timestamp())")
        .bind(crate::security::token_digest("admin")).bind(crate::security::token_digest("other")).execute(&state.pool).await.unwrap();
    let credential: String =
        sqlx::query_scalar("SELECT credential_id FROM dns_connections WHERE id=$1")
            .bind(&connection)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let create = json!({"name":"New connection","credential_id":credential,"credential_version":1});
    let case = match kind {
        0 => Case {
            method: Method::POST,
            path: "/connections".into(),
            body: create,
            status: StatusCode::CREATED,
        },
        1 => Case {
            method: Method::PATCH,
            path: format!("/connections/{connection}"),
            body: json!({"expected_revision":1,"name":"Renamed"}),
            status: StatusCode::OK,
        },
        2 => {
            let (_, Json(empty)) = super::super::create_connection(
                State(state.clone()),
                Json(serde_json::from_value(create).unwrap()),
            )
            .await
            .unwrap();
            Case {
                method: Method::DELETE,
                path: format!(
                    "/connections/{}?expected_revision=1",
                    empty["id"].as_str().unwrap()
                ),
                body: json!({}),
                status: StatusCode::NO_CONTENT,
            }
        }
        3 | 4 => Case {
            method: Method::POST,
            path: format!(
                "/connections/{connection}/{}",
                if kind == 3 { "verify" } else { "refresh" }
            ),
            body: json!({"expected_revision":1}),
            status: StatusCode::ACCEPTED,
        },
        5 => Case {
            method: Method::PATCH,
            path: "/zones/zone".into(),
            body: json!({"expected_revision":1,"enabled":false}),
            status: StatusCode::OK,
        },
        6 => Case {
            method: Method::POST,
            path: "/zones/zone/refresh".into(),
            body: json!({"expected_revision":1}),
            status: StatusCode::ACCEPTED,
        },
        _ => unreachable!(),
    };
    (state, case)
}
pub(super) async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v2/dns", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, crate::api::router(state))
            .await
            .unwrap()
    });
    (base, task)
}
pub(super) fn client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
}
async fn send(
    client: &Client,
    base: &str,
    case: &Case,
    actor: &str,
    key: Option<&str>,
) -> reqwest::Response {
    let mut request = client
        .request(case.method.clone(), format!("{base}{}", case.path))
        .bearer_auth(actor)
        .json(&case.body);
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    request.send().await.unwrap()
}
pub(super) async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
pub(super) async fn snapshot(state: &AppState) -> Value {
    let tables = [
        "nodes",
        "node_packages",
        "config_versions",
        "dns_records",
        "dns_bindings",
        "credentials",
        "credential_versions",
        "credential_reference_edges",
        "dns_connections",
        "dns_zones",
        "dns_operations",
        "jobs",
        "job_events",
        "audit_records",
        "management_receipts",
        "cache_bucket_versions",
    ];
    let parts: Vec<String> = tables.iter().map(|t| format!("SELECT '{t}' AS kind,coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) AS data FROM {t} t")).collect();
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT jsonb_object_agg(kind,data) FROM ({}) snapshots",
        parts.join(" UNION ALL ")
    )))
    .fetch_one(&state.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn seven_operations_converge_under_same_key_races_and_replay_after_the_source_is_gone() {
    let client = client();
    for kind in 0..7 {
        let (state, case) = fixture(kind).await;
        let (base, server) = serve(state.clone()).await;
        let (a, b) = tokio::join!(
            send(&client, &base, &case, "admin", Some("race")),
            send(&client, &base, &case, "admin", Some("race"))
        );
        assert_eq!(a.status(), case.status, "kind {kind}");
        assert_eq!(b.status(), case.status, "kind {kind}");
        let bytes = a.bytes().await.unwrap();
        assert_eq!(b.bytes().await.unwrap(), bytes);
        assert_eq!(
            count(&state, "SELECT count(*) FROM management_receipts").await,
            1
        );
        if matches!(kind, 3 | 4 | 6) {
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                count(&state, "SELECT count(*) FROM dns_operations").await,
                1
            );
            assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
            assert_eq!(count(&state, "SELECT count(*) FROM job_events").await, 1);
            let job: String = sqlx::query_scalar("SELECT id FROM jobs")
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(value["job_id"], job);
        } else {
            assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
            assert_eq!(
                count(
                    &state,
                    "SELECT count(*) FROM audit_records WHERE actor='operator'"
                )
                .await,
                1
            );
        }
        sqlx::raw_sql("DELETE FROM dns_operations; DELETE FROM jobs; DELETE FROM dns_zones; DELETE FROM dns_connections; UPDATE credentials SET archived=true")
            .execute(&state.pool).await.unwrap();
        let before = snapshot(&state).await;
        let replay = send(&client, &base, &case, "admin", Some("race")).await;
        assert_eq!(replay.status(), case.status);
        assert_eq!(replay.bytes().await.unwrap(), bytes);
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn seven_receipt_failures_roll_back_all_business_and_native_errors_commit_once() {
    let client = client();
    for code in ["40001", "40P01"] {
        for kind in 0..7 {
            let (state, case) = fixture(kind).await;
            let (base, server) = serve(state.clone()).await;
            sqlx::raw_sql("CREATE FUNCTION reject_dns_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER reject_dns_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_dns_receipt()")
                .execute(&state.pool).await.unwrap();
            let before = snapshot(&state).await;
            assert_eq!(
                send(&client, &base, &case, "admin", Some("retry"))
                    .await
                    .status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "kind {kind}"
            );
            assert_eq!(snapshot(&state).await, before, "kind {kind}");
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SEQUENCE dns_attempt; CREATE OR REPLACE FUNCTION reject_dns_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('dns_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$")))
                .execute(&state.pool).await.unwrap();
            let response = send(&client, &base, &case, "admin", Some("retry")).await;
            assert_eq!(response.status(), case.status, "kind {kind}, code {code}");
            let bytes = response.bytes().await.unwrap();
            assert_eq!(count(&state, "SELECT last_value FROM dns_attempt").await, 2);
            assert_eq!(
                count(&state, "SELECT count(*) FROM management_receipts").await,
                1
            );
            if matches!(kind, 3 | 4 | 6) {
                assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
                assert_eq!(
                    count(&state, "SELECT count(*) FROM dns_operations").await,
                    1
                );
                assert_eq!(count(&state, "SELECT count(*) FROM job_events").await, 1);
            }
            let before = snapshot(&state).await;
            assert_eq!(
                send(&client, &base, &case, "admin", Some("retry"))
                    .await
                    .bytes()
                    .await
                    .unwrap(),
                bytes
            );
            assert_eq!(snapshot(&state).await, before);
            assert_eq!(count(&state, "SELECT last_value FROM dns_attempt").await, 2);
            server.abort();
        }
    }
}

#[tokio::test]
async fn every_operation_rejects_missing_headers_and_changed_requests_without_consuming_work() {
    let client = client();
    for kind in 0..7 {
        let (state, mut case) = fixture(kind).await;
        let (base, server) = serve(state.clone()).await;
        let before = snapshot(&state).await;
        assert_eq!(
            send(&client, &base, &case, "admin", None).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(snapshot(&state).await, before);
        assert_eq!(
            send(&client, &base, &case, "admin", Some("same"))
                .await
                .status(),
            case.status
        );
        let before = snapshot(&state).await;
        case.body["unexpected"] = json!(true);
        assert_eq!(
            send(&client, &base, &case, "admin", Some("same"))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn child_operations_ignore_legacy_body_keys_and_are_isolated_by_actor_and_action() {
    let client = client();
    let (state, mut verify) = fixture(3).await;
    let (base, server) = serve(state.clone()).await;
    verify.body["idempotency_key"] = json!("shared-legacy-key");
    let mut refresh = Case {
        method: Method::POST,
        path: verify.path.replace("/verify", "/refresh"),
        body: verify.body.clone(),
        status: StatusCode::ACCEPTED,
    };
    for actor in ["admin", "other"] {
        for case in [&verify, &refresh] {
            assert_eq!(
                send(&client, &base, case, actor, Some("shared-header"))
                    .await
                    .status(),
                case.status
            );
        }
    }
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        4
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 4);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM dns_operations WHERE idempotency_key LIKE 'v2:%'"
        )
        .await,
        4
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM dns_operations WHERE idempotency_key='shared-legacy-key'"
        )
        .await,
        0
    );
    refresh.path = verify.path.clone();
    refresh.body["expected_revision"] = json!(2);
    let before = snapshot(&state).await;
    assert_eq!(
        send(&client, &base, &refresh, "admin", Some("shared-header"))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}

#[tokio::test]
async fn current_bindings_history_and_credential_archival_still_reject_new_operations() {
    let client = client();
    let (state, mut case) = fixture(0).await;
    let ssh: String = sqlx::query_scalar("SELECT id FROM credentials WHERE kind='ssh_password'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let node = crate::dns::tests::node(&state, &ssh).await;
    sqlx::query("INSERT INTO dns_bindings(node_id,zone_id,hostname,record_ids) VALUES($1,'zone','bound.example.test','[]'::jsonb)").bind(&node).execute(&state.pool).await.unwrap();
    let (base, server) = serve(state.clone()).await;
    let zone = Case {
        method: Method::PATCH,
        path: "/zones/zone".into(),
        body: json!({"expected_revision":1,"enabled":false}),
        status: StatusCode::OK,
    };
    let connection: String = sqlx::query_scalar("SELECT id FROM dns_connections")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let delete = Case {
        method: Method::DELETE,
        path: format!("/connections/{connection}?expected_revision=1"),
        body: json!({}),
        status: StatusCode::NO_CONTENT,
    };
    let before = snapshot(&state).await;
    for request in [&zone, &delete] {
        assert_eq!(
            send(&client, &base, request, "admin", Some("blocked"))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(snapshot(&state).await, before);
    }
    sqlx::query("UPDATE credentials SET archived=true WHERE kind='dns'")
        .execute(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state).await;
    assert_eq!(
        send(&client, &base, &case, "admin", Some("blocked"))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(snapshot(&state).await, before);
    case.body["name"] = json!(" ");
    assert_eq!(
        send(&client, &base, &case, "admin", Some("blocked"))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}
