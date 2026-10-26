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
    json!({"action":"create","name":name,"user_ids":users,"node_ids":["node"]})
}
async fn group(state: &AppState, users: Vec<&str>) -> String {
    let mut body = draft("Fixture group", users);
    let Json(preview) = super::super::preview_create(
        State(state.clone()),
        Json(serde_json::from_value(body.clone()).unwrap()),
    )
    .await
    .unwrap();
    body.as_object_mut().unwrap().remove("action");
    body["preview_token"] = preview["preview_token"].clone();
    let (_, Json(result)) = super::super::create(
        State(state.clone()),
        Json(serde_json::from_value(body).unwrap()),
    )
    .await
    .unwrap();
    result["group"]["id"].as_str().unwrap().to_owned()
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
        0 => (
            Method::POST,
            "/authorization-groups/preview".into(),
            draft("Previewed", vec!["user"]),
            StatusCode::OK,
        ),
        1 | 2 => {
            let id = group(&state, vec![]).await;
            if kind == 1 {
                (
                    Method::POST,
                    format!("/authorization-groups/{id}/preview"),
                    json!({"action":"update","expected_revision":1,"name":"Previewed","user_ids":["user"],"node_ids":["node"]}),
                    StatusCode::OK,
                )
            } else {
                (
                    Method::POST,
                    "/users/user/authorization-groups/preview".into(),
                    json!({"expected_revision":1,"group_ids":[id]}),
                    StatusCode::OK,
                )
            }
        }
        3 => {
            let mut body = draft("Granted", vec!["user"]);
            let Json(preview) = super::super::preview_create(
                State(state.clone()),
                Json(serde_json::from_value(body.clone()).unwrap()),
            )
            .await
            .unwrap();
            body.as_object_mut().unwrap().remove("action");
            body["preview_token"] = preview["preview_token"].clone();
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
                json!({"action":"update","expected_revision":1,"name":"Granted","user_ids":["user"],"node_ids":["node"]})
            } else {
                json!({"action":"delete","expected_revision":1})
            };
            let Json(preview) = super::super::preview_group(
                State(state.clone()),
                Path(id.clone()),
                Json(serde_json::from_value(body.clone()).unwrap()),
            )
            .await
            .unwrap();
            body.as_object_mut().unwrap().remove("action");
            body["preview_token"] = preview["preview_token"].clone();
            (
                if kind == 4 {
                    Method::PUT
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
            let mut body = json!({"expected_revision":1,"group_ids":[id]});
            let Json(preview) = super::super::preview_memberships(
                State(state.clone()),
                Path("user".into()),
                Json(serde_json::from_value(body.clone()).unwrap()),
            )
            .await
            .unwrap();
            body["preview_token"] = preview["preview_token"].clone();
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
        "authorization_group_previews",
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
    if case.kind < 3 {
        let token = body["preview_token"].as_str().unwrap();
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM authorization_group_previews WHERE token=$1 AND consumed_at IS NULL AND expires_at-created_at=interval '10 minutes')").bind(token).fetch_one(&state.pool).await.unwrap();
        assert!(exists);
        assert_eq!(
            count(state, "SELECT count(*) FROM node_assignments").await,
            0
        );
        assert_eq!(
            count(
                state,
                "SELECT count(*) FROM audit_records WHERE actor='group-operator'"
            )
            .await,
            0
        );
        let cipher: String = sqlx::query_scalar("SELECT response_enc FROM management_receipts")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert!(!cipher.contains(token));
    } else {
        assert_eq!(
            count(
                state,
                "SELECT count(*) FROM audit_records WHERE actor='group-operator'"
            )
            .await,
            1
        );
        if case.kind == 5 {
            assert_eq!(
                count(state, "SELECT count(*) FROM authorization_groups").await,
                0
            );
            assert_eq!(
                count(state, "SELECT count(*) FROM node_assignments").await,
                0
            );
            assert_eq!(body["removals_count"], 1);
            assert_eq!(body["revocation_job_ids"].as_array().unwrap().len(), 1);
        } else {
            let secret = body["created_credentials"][0]["hy2_credential"]
                .as_str()
                .unwrap();
            let row=sqlx::query("SELECT credential_hash,credential_enc FROM node_assignments WHERE user_id='user' AND node_id='node'").fetch_one(&state.pool).await.unwrap();
            assert_eq!(
                row.get::<String, _>("credential_hash"),
                token_digest(secret)
            );
            assert_eq!(
                state
                    .secrets
                    .decrypt(&row.get::<String, _>("credential_enc"))
                    .unwrap(),
                secret
            );
            let cipher: String = sqlx::query_scalar("SELECT response_enc FROM management_receipts")
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert!(!cipher.contains(secret));
        }
    }
}

#[tokio::test]
async fn seven_races_commit_one_preview_or_change_and_replay_after_consumption_or_source_removal() {
    let client = client();
    for kind in 0..7 {
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
        sqlx::raw_sql("DELETE FROM authorization_group_previews; DELETE FROM authorization_groups; DELETE FROM node_assignments; DELETE FROM users; DELETE FROM nodes")
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
async fn seven_receipt_failures_roll_back_preview_consumption_secrets_revocations_and_cache_then_retry_once()
 {
    let client = client();
    for code in ["40001", "40P01"] {
        for kind in 0..7 {
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
async fn seven_missing_headers_or_changed_requests_leave_no_second_preview_or_change() {
    let client = client();
    for kind in 0..7 {
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
async fn preview_garbage_collection_is_atomic_and_replay_does_not_renew_expired_tokens() {
    let (state, case) = fixture(0).await;
    let Json(seed) = super::super::preview_create(
        State(state.clone()),
        Json(serde_json::from_value(case.body.clone()).unwrap()),
    )
    .await
    .unwrap();
    sqlx::query("INSERT INTO authorization_group_previews(token,action,request_json,guard_json,result_json,created_at,expires_at) SELECT 'expired-'||i,action,request_json,guard_json,result_json,created_at,clock_timestamp()-interval '1 hour' FROM authorization_group_previews CROSS JOIN generate_series(1,300) i WHERE token=$1")
        .bind(seed["preview_token"].as_str().unwrap()).execute(&state.pool).await.unwrap();
    let (base, server) = serve(state.clone()).await;
    let client = client();
    sqlx::raw_sql("CREATE FUNCTION reject_preview_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER reject_preview_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_preview_receipt()")
        .execute(&state.pool).await.unwrap();
    let before = snapshot(&state).await;
    assert_eq!(
        send(&client, &base, &case, Some("gc")).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(snapshot(&state).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_preview_receipt ON management_receipts")
        .execute(&state.pool)
        .await
        .unwrap();
    let response = send(&client, &base, &case, Some("gc")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.bytes().await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE expires_at<clock_timestamp()"
        )
        .await,
        44
    );
    sqlx::query("UPDATE authorization_group_previews SET expires_at=clock_timestamp()-interval '1 second' WHERE token=$1")
        .bind(body["preview_token"].as_str().unwrap()).execute(&state.pool).await.unwrap();
    let before = snapshot(&state).await;
    assert_eq!(
        send(&client, &base, &case, Some("gc"))
            .await
            .bytes()
            .await
            .unwrap(),
        bytes
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}
