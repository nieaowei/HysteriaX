use super::*;
use reqwest::Client;

struct Case {
    method: Method,
    path: String,
    body: Value,
    status: StatusCode,
    kind: usize,
}
fn draft(name: &str, users: Vec<&str>) -> Value {
    json!({"name":name,"user_ids":users,"node_ids":["node"]})
}
async fn group(state: &AppState, users: Vec<&str>) -> String {
    let body = draft("Fixture group", users);
    let (_, Json(result)) = super::super::create(
        State(state.clone()),
        Json(serde_json::from_value(body).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(result, json!({"revocation_job_ids":[]}));
    super::super::group_named(state, "Fixture group").await["id"]
        .as_str()
        .unwrap()
        .to_owned()
}
async fn fixture(kind: usize) -> (AppState, Case) {
    let state = super::super::tests::fixture().await;
    sqlx::query("DELETE FROM node_assignments")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('group-operator',$1,'Group operator',clock_timestamp(),clock_timestamp())")
        .bind(token_digest("group-admin-v2")).execute(&state.pool).await.unwrap();
    let (method, path, body, status) = match kind {
        3 => {
            let body = json!({"name":"Granted"});
            (
                Method::POST,
                "/authorization-groups".into(),
                body,
                StatusCode::CREATED,
            )
        }
        4 | 5 => {
            let id = group(&state, if kind == 5 { vec!["user"] } else { vec![] }).await;
            let mut body = if kind == 4 {
                json!({"action":"update","expected_revision":1,"add_user_ids":["user"]})
            } else {
                json!({"action":"delete","expected_revision":1})
            };
            body.as_object_mut().unwrap().remove("action");
            (
                if kind == 4 {
                    Method::PATCH
                } else {
                    Method::DELETE
                },
                format!("/authorization-groups/{id}"),
                body,
                StatusCode::OK,
            )
        }
        6 => {
            let id = group(&state, vec![]).await;
            let body = json!({"expected_revision":1,"group_ids":[id]});
            (
                Method::PUT,
                "/users/user/authorization-groups".into(),
                body,
                StatusCode::OK,
            )
        }
        _ => unreachable!(),
    };
    (
        state,
        Case {
            method,
            path,
            body,
            status,
            kind,
        },
    )
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
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap()
}
async fn send(client: &Client, base: &str, case: &Case, key: Option<&str>) -> reqwest::Response {
    let mut request = client
        .request(case.method.clone(), format!("{base}{}", case.path))
        .bearer_auth("group-admin-v2")
        .json(&case.body);
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    request.send().await.unwrap()
}
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn snapshot(state: &AppState) -> Value {
    let tables = [
        "nodes",
        "users",
        "node_assignments",
        "authorization_groups",
        "authorization_group_users",
        "authorization_group_nodes",
        "kick_requests",
        "jobs",
        "job_events",
        "audit_records",
        "credentials",
        "credential_versions",
        "mtls_user_identities",
        "mtls_user_versions",
        "management_receipts",
        "cache_bucket_versions",
        "cache_entity_versions",
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
async fn assert_committed(state: &AppState, case: &Case, body: &Value) {
    assert_eq!(
        count(state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    assert_eq!(
        count(
            state,
            "SELECT count(*) FROM audit_records WHERE actor='group-operator'"
        )
        .await,
        1
    );
    if case.kind != 6 {
        assert_eq!(body.as_object().unwrap().len(), 1);
        assert!(body["revocation_job_ids"].is_array());
    }
    if case.kind == 5 {
        assert_eq!(
            count(state, "SELECT count(*) FROM authorization_groups").await,
            0
        );
        assert_eq!(
            count(state, "SELECT count(*) FROM node_assignments").await,
            0
        );
        assert_eq!(body["revocation_job_ids"].as_array().unwrap().len(), 1);
    } else if case.kind == 3 {
        assert_eq!(
            count(state, "SELECT count(*) FROM authorization_groups").await,
            1
        );
        assert_eq!(
            count(state, "SELECT count(*) FROM authorization_group_users").await,
            0
        );
        assert_eq!(
            count(state, "SELECT count(*) FROM authorization_group_nodes").await,
            0
        );
        assert_eq!(
            count(state, "SELECT count(*) FROM node_assignments").await,
            0
        );
    } else {
        let row=sqlx::query("SELECT credential_hash,credential_enc FROM node_assignments WHERE user_id='user' AND node_id='node'").fetch_one(&state.pool).await.unwrap();
        let secret = state
            .secrets
            .decrypt(&row.get::<String, _>("credential_enc"))
            .unwrap();
        assert_eq!(
            row.get::<String, _>("credential_hash"),
            token_digest(&secret)
        );
        let cipher: String = sqlx::query_scalar("SELECT response_enc FROM management_receipts")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert!(!cipher.contains(&secret));
    }
}

#[tokio::test]
async fn four_changes_commit_once_and_replay_after_source_removal() {
    let client = client();
    for kind in 3..7 {
        let (state, case) = fixture(kind).await;
        let (base, server) = serve(state.clone()).await;
        let (a, b) = tokio::join!(
            send(&client, &base, &case, Some("race")),
            send(&client, &base, &case, Some("race"))
        );
        assert_eq!(a.status(), case.status, "kind {kind}");
        assert_eq!(b.status(), case.status, "kind {kind}");
        let bytes = a.bytes().await.unwrap();
        assert_eq!(b.bytes().await.unwrap(), bytes);
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_committed(&state, &case, &body).await;
        sqlx::raw_sql("DELETE FROM authorization_groups; DELETE FROM node_assignments; DELETE FROM users; DELETE FROM nodes")
            .execute(&state.pool).await.unwrap();
        let before = snapshot(&state).await;
        let response = send(&client, &base, &case, Some("race")).await;
        assert_eq!(response.status(), case.status);
        assert_eq!(response.bytes().await.unwrap(), bytes);
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn four_receipt_failures_roll_back_secrets_revocations_and_cache_then_retry_once() {
    let client = client();
    for code in ["40001", "40P01"] {
        for kind in 3..7 {
            let (state, case) = fixture(kind).await;
            let (base, server) = serve(state.clone()).await;
            sqlx::raw_sql("CREATE FUNCTION reject_group_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER reject_group_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_group_receipt()")
                .execute(&state.pool).await.unwrap();
            let before = snapshot(&state).await;
            assert_eq!(
                send(&client, &base, &case, Some("retry")).await.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "kind {kind}"
            );
            assert_eq!(snapshot(&state).await, before, "kind {kind}");
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SEQUENCE group_attempt; CREATE OR REPLACE FUNCTION reject_group_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('group_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$")))
                .execute(&state.pool).await.unwrap();
            let response = send(&client, &base, &case, Some("retry")).await;
            assert_eq!(response.status(), case.status, "kind {kind}, code {code}");
            let bytes = response.bytes().await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_committed(&state, &case, &body).await;
            assert_eq!(
                count(&state, "SELECT last_value FROM group_attempt").await,
                2
            );
            let before = snapshot(&state).await;
            assert_eq!(
                send(&client, &base, &case, Some("retry"))
                    .await
                    .bytes()
                    .await
                    .unwrap(),
                bytes
            );
            assert_eq!(snapshot(&state).await, before);
            server.abort();
        }
    }
}

#[tokio::test]
async fn four_missing_headers_or_changed_requests_leave_no_second_change() {
    let client = client();
    for kind in 3..7 {
        let (state, mut case) = fixture(kind).await;
        let (base, server) = serve(state.clone()).await;
        let before = snapshot(&state).await;
        assert_eq!(
            send(&client, &base, &case, None).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(snapshot(&state).await, before);
        assert_eq!(
            send(&client, &base, &case, Some("same")).await.status(),
            case.status
        );
        let before = snapshot(&state).await;
        case.body["unexpected"] = json!(true);
        assert_eq!(
            send(&client, &base, &case, Some("same")).await.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn retired_authorization_preview_routes_do_not_write() {
    let (state, _) = fixture(3).await;
    let (base, server) = serve(state.clone()).await;
    let before = snapshot(&state).await;
    for path in [
        "/authorization-groups/preview",
        "/authorization-groups/group/preview",
        "/users/user/authorization-groups/preview",
    ] {
        let response = client()
            .post(format!("{base}{path}"))
            .bearer_auth("group-admin-v2")
            .header("Idempotency-Key", "retired")
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert!(matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ));
    }
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}

#[test]
fn patch_validation_rejects_replacements_conflicts_and_empty_requests() {
    for body in [
        json!({"expected_revision":1,"user_ids":[]}),
        json!({"expected_revision":1,"node_ids":[]}),
        json!({"expected_revision":1,"name":null}),
        json!({"expected_revision":1,"add_user_ids":null}),
    ] {
        assert!(parse::<PatchGroupRequest>(body).is_err());
    }
    for body in [
        json!({"expected_revision":0,"name":"Name"}),
        json!({"expected_revision":1}),
        json!({"expected_revision":1,"add_node_ids":[]}),
        json!({"expected_revision":1,"add_user_ids":["u"],"remove_user_ids":["u"]}),
        json!({"expected_revision":1,"add_node_ids":["n"],"remove_node_ids":["n"]}),
        json!({"expected_revision":1,"remove_user_ids":[" "]}),
    ] {
        assert!(
            parse::<PatchGroupRequest>(body)
                .unwrap()
                .normalize()
                .is_err()
        );
    }
    assert!(parse::<CreateGroupRequest>(json!({"name":"Empty"})).is_ok());
    assert!(
        parse::<CreateGroupRequest>(json!({"name":"Old","user_ids":[],"node_ids":[]})).is_err()
    );
    let patch =
        parse::<PatchGroupRequest>(json!({"expected_revision":1,"add_user_ids":["b","a","a"]}))
            .unwrap()
            .normalize()
            .unwrap();
    assert_eq!(patch.add_user_ids, vec!["a", "b"]);
}

#[tokio::test]
async fn partial_updates_preserve_omitted_relations_and_reject_stale_or_invalid_changes() {
    let (state, mut case) = fixture(4).await;
    let id = case.path.rsplit('/').next().unwrap().to_owned();
    let (base, server) = serve(state.clone()).await;
    let client = client();
    for (key, body, status, revision, users, nodes) in [
        (
            "add-user",
            json!({"expected_revision":1,"add_user_ids":["user","user"]}),
            StatusCode::OK,
            2,
            vec!["user"],
            vec!["node"],
        ),
        (
            "stale",
            json!({"expected_revision":1,"remove_node_ids":["node"]}),
            StatusCode::CONFLICT,
            2,
            vec!["user"],
            vec!["node"],
        ),
        (
            "missing",
            json!({"expected_revision":2,"add_user_ids":["missing"]}),
            StatusCode::NOT_FOUND,
            2,
            vec!["user"],
            vec!["node"],
        ),
        (
            "remove-node",
            json!({"expected_revision":2,"remove_node_ids":["node"]}),
            StatusCode::OK,
            3,
            vec!["user"],
            vec![],
        ),
        (
            "add-node",
            json!({"expected_revision":3,"add_node_ids":["node"]}),
            StatusCode::OK,
            4,
            vec!["user"],
            vec!["node"],
        ),
        (
            "remove-user",
            json!({"expected_revision":4,"remove_user_ids":["user"]}),
            StatusCode::OK,
            5,
            vec![],
            vec!["node"],
        ),
        (
            "duplicate",
            json!({"expected_revision":5,"add_node_ids":["node"]}),
            StatusCode::OK,
            5,
            vec![],
            vec!["node"],
        ),
    ] {
        case.body = body;
        let response = send(&client, &base, &case, Some(key)).await;
        assert_eq!(
            response.status(),
            status,
            "{key}: {}",
            response.text().await.unwrap()
        );
        let stored = super::super::group_named(&state, "Fixture group").await;
        assert_eq!(stored["revision"], revision, "{key}");
        assert_eq!(stored["user_ids"], json!(users), "{key}");
        assert_eq!(stored["node_ids"], json!(nodes), "{key}");
    }
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        0
    );
    let response = client
        .put(format!("{base}/authorization-groups/{id}"))
        .bearer_auth("group-admin-v2")
        .header("Idempotency-Key", "old-put")
        .json(&json!({"expected_revision":5,"name":"Old","user_ids":[],"node_ids":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    server.abort();
}

#[tokio::test]
async fn rename_only_touches_group_metadata_and_replays_without_loading_authorization() {
    let (state, mut case) = fixture(5).await;
    let id = case.path.rsplit('/').next().unwrap().to_owned();
    let users_before: Value = sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(u)) FROM users u")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let assignments_before: Value =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(a)) FROM node_assignments a")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_rename_side_effects() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'rename touched authorization'; END $$; CREATE TRIGGER reject_rename_user BEFORE UPDATE ON users FOR EACH STATEMENT EXECUTE FUNCTION reject_rename_side_effects(); CREATE TRIGGER reject_rename_assignments BEFORE INSERT OR UPDATE OR DELETE ON node_assignments FOR EACH STATEMENT EXECUTE FUNCTION reject_rename_side_effects(); CREATE TRIGGER reject_rename_members BEFORE INSERT OR UPDATE OR DELETE ON authorization_group_users FOR EACH STATEMENT EXECUTE FUNCTION reject_rename_side_effects(); CREATE TRIGGER reject_rename_nodes BEFORE INSERT OR UPDATE OR DELETE ON authorization_group_nodes FOR EACH STATEMENT EXECUTE FUNCTION reject_rename_side_effects();")
        .execute(&state.pool).await.unwrap();
    let (base, server) = serve(state.clone()).await;
    case.method = Method::PATCH;
    case.body = json!({"expected_revision":1,"name":"Renamed"});
    let client = client();
    let response = send(&client, &base, &case, Some("rename")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.bytes().await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!({"revocation_job_ids":[]})
    );
    assert_eq!(
        send(&client, &base, &case, Some("rename"))
            .await
            .bytes()
            .await
            .unwrap(),
        bytes
    );
    let stored = super::super::group_named(&state, "Renamed").await;
    assert_eq!(stored["id"], id);
    assert_eq!(stored["revision"], 2);
    assert_eq!(stored["user_ids"], json!(["user"]));
    assert_eq!(stored["node_ids"], json!(["node"]));
    let users_after: Value = sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(u)) FROM users u")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let assignments_after: Value =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(a)) FROM node_assignments a")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(users_after, users_before);
    assert_eq!(assignments_after, assignments_before);
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    server.abort();
}

#[tokio::test]
async fn concurrent_deltas_require_a_new_revision_instead_of_overwriting_members() {
    let (state, mut first) = fixture(4).await;
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('second-user','Second',now(),now())")
        .execute(&state.pool).await.unwrap();
    let (base, server) = serve(state.clone()).await;
    first.body = json!({"expected_revision":1,"add_user_ids":["user"]});
    let mut second = Case {
        method: Method::PATCH,
        path: first.path.clone(),
        body: json!({"expected_revision":1,"add_user_ids":["second-user"]}),
        status: StatusCode::OK,
        kind: 4,
    };
    let client = client();
    let (a, b) = tokio::join!(
        send(&client, &base, &first, Some("first-admin")),
        send(&client, &base, &second, Some("second-admin"))
    );
    let mut statuses = [a.status().as_u16(), b.status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, [200, 409]);
    let stored = super::super::group_named(&state, "Fixture group").await;
    assert_eq!(stored["revision"], 2);
    assert_eq!(stored["user_ids"].as_array().unwrap().len(), 1);
    second.body = json!({"expected_revision":2,"add_user_ids":["user","second-user"]});
    assert_eq!(
        send(&client, &base, &second, Some("merge-after-reload"))
            .await
            .status(),
        StatusCode::OK
    );
    let stored = super::super::group_named(&state, "Fixture group").await;
    assert_eq!(stored["revision"], 3);
    assert_eq!(stored["user_ids"], json!(["second-user", "user"]));
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        2
    );
    server.abort();
}
