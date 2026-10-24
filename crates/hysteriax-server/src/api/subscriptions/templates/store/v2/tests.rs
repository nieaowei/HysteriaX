use super::*;
use reqwest::{Client, Method};

struct Case {
    method: Method,
    path: String,
    body: Value,
    status: StatusCode,
    kind: usize,
}
async fn state() -> AppState {
    let pool = db::test_pool_with_max_connections(8).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    crate::credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    crate::credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    initialize(&pool, &secrets).await.unwrap();
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('template-operator',$1,'Template operator',clock_timestamp(),clock_timestamp())")
        .bind(crate::security::token_digest("template-admin-v2")).execute(&pool).await.unwrap();
    AppState::new(pool, secrets)
}
async fn fixture(kind: usize) -> (AppState, Case) {
    let state = state().await;
    let document = render::single_seed("mihomo");
    let target = "/subscription-templates/default-mihomo";
    let (method, path, body, status) = match kind {
        0 => (
            Method::POST,
            "/subscription-templates".into(),
            json!({"expected_revision":0,"name":"Created","format":"mihomo"}),
            StatusCode::CREATED,
        ),
        1 => {
            let (_, Json(replacement)) = super::super::create(
                State(state.clone()),
                Json(Create {
                    expected_revision: 0,
                    name: "Replacement".into(),
                    format: "mihomo".into(),
                    document: None,
                }),
            )
            .await
            .unwrap();
            let id = replacement["id"].as_str().unwrap().to_owned();
            let _ = super::super::publish(
                State(state.clone()),
                Path(id.clone()),
                Json(Revision {
                    expected_revision: 1,
                }),
            )
            .await
            .unwrap();
            (
                Method::PATCH,
                target.into(),
                json!({"expected_revision":1,"archived":true,"replacement_id":id,"replacement_revision":2}),
                StatusCode::OK,
            )
        }
        2 => {
            let mut document = document.clone();
            document.common["allow-lan"] = json!(true);
            (
                Method::PUT,
                format!("{target}/draft"),
                json!({"expected_revision":1,"document":document}),
                StatusCode::OK,
            )
        }
        3 => (
            Method::POST,
            format!("{target}/publish"),
            json!({"expected_revision":1}),
            StatusCode::OK,
        ),
        4 => (
            Method::POST,
            format!("{target}/rollback"),
            json!({"expected_revision":1,"version":1}),
            StatusCode::OK,
        ),
        5 => (
            Method::POST,
            "/subscription-templates/source".into(),
            json!({"format":"mihomo","config":{"password":"source-receipt-secret"}}),
            StatusCode::OK,
        ),
        6 => (
            Method::POST,
            "/subscription-templates/import".into(),
            json!({"format":"mihomo","package":{"package_version":1,"format":"mihomo","document":document}}),
            StatusCode::OK,
        ),
        7 => (
            Method::POST,
            format!("{target}/validate"),
            json!({}),
            StatusCode::OK,
        ),
        8 => {
            let name = document.common["proxy-groups"][0]["name"]
                .as_str()
                .unwrap()
                .to_owned();
            (
                Method::POST,
                format!("{target}/refactor"),
                json!({"expected_revision":1,"document":document,"kind":"group","old_name":name,"new_name":"Renamed"}),
                StatusCode::OK,
            )
        }
        9 => {
            sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES('preview-user','Preview',true,clock_timestamp(),clock_timestamp())").execute(&state.pool).await.unwrap();
            let input=crate::credentials::test_node_request(&state,json!({"name":"Published","ssh_host":"8.8.8.8","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"fixture-ssh-secret","public_host":"published.example.test","public_port":443,"listen_addr":":443"})).await;
            let (_, Json(node)) = crate::api::nodes::create(State(state.clone()), Json(input))
                .await
                .unwrap();
            let id = node["node"]["id"].as_str().unwrap();
            let mut tx = state.pool.begin().await.unwrap();
            sqlx::query("UPDATE nodes SET deployed_revision=1,deployed_config_enc=desired_config_enc,mtls_state='ready' WHERE id=$1").bind(id).execute(&mut *tx).await.unwrap();
            crate::credentials::reference_index::flush_node_on(&mut tx, &state.secrets, id)
                .await
                .unwrap();
            sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('preview-user',$1,$2,$3,clock_timestamp())")
                .bind(id).bind(crate::security::token_digest("preview-receipt-secret")).bind(state.secrets.encrypt("preview-receipt-secret").unwrap()).execute(&mut *tx).await.unwrap();
            tx.commit().await.unwrap();
            (
                Method::POST,
                format!("{target}/preview"),
                json!({"user_id":"preview-user","platform":"linux","reveal_secrets":true}),
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
        .bearer_auth("template-admin-v2")
        .json(&case.body);
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    request.send().await.unwrap()
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn snapshot(state: &AppState, exclude_failure_audit: bool) -> Value {
    let tables = [
        "subscription_templates",
        "subscription_template_versions",
        "users",
        "nodes",
        "node_assignments",
        "credentials",
        "credential_versions",
        "management_receipts",
        "audit_records",
        "cache_bucket_versions",
        "cache_entity_versions",
    ];
    let parts:Vec<String>=tables.iter().map(|t| {
        let predicate=match (*t,exclude_failure_audit) {
            ("audit_records",true)=>" WHERE coalesce(detail_json->>'result','')<>'failure'",
            ("cache_bucket_versions",true)=>" WHERE domain<>'audit'",
            _=>"",
        };
        format!("SELECT '{t}' AS kind,coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) AS data FROM {t} t{predicate}")
    }).collect();
    sqlx::query_scalar(&format!(
        "SELECT jsonb_object_agg(kind,data) FROM ({}) snapshots",
        parts.join(" UNION ALL ")
    ))
    .fetch_one(&state.pool)
    .await
    .unwrap()
}
async fn assert_committed(state: &AppState, case: &Case, body: &Value) {
    assert_eq!(
        count(state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    let audits=count(state,"SELECT count(*) FROM audit_records WHERE actor='template-operator' AND detail_json->>'result'='success'").await;
    assert_eq!(
        audits,
        if case.kind == 1 {
            2
        } else if case.kind < 5 {
            1
        } else {
            0
        }
    );
    match case.kind {
        0 => assert_eq!(
            count(state, "SELECT count(*) FROM subscription_templates").await,
            3
        ),
        1 => {
            assert_eq!(body["archived"], true);
            assert_eq!(body["is_default"], false);
            let id: String = sqlx::query_scalar(
                "SELECT id FROM subscription_templates WHERE format='mihomo' AND is_default",
            )
            .fetch_one(&state.pool)
            .await
            .unwrap();
            assert_eq!(id, case.body["replacement_id"].as_str().unwrap());
        }
        2 => {
            let cipher: String = sqlx::query_scalar(
                "SELECT draft_enc FROM subscription_templates WHERE id='default-mihomo'",
            )
            .fetch_one(&state.pool)
            .await
            .unwrap();
            assert_eq!(
                decode(&state.secrets, &cipher).unwrap().common["allow-lan"],
                true
            );
            assert_eq!(body["revision"], 2);
        }
        3 | 4 => {
            assert_eq!(body["published_version"], 2);
            assert_eq!(count(state,"SELECT count(*) FROM subscription_template_versions WHERE template_id='default-mihomo'").await,2);
            if case.kind == 4 {
                let copied:bool=sqlx::query_scalar("SELECT a.document_enc=b.document_enc FROM subscription_template_versions a JOIN subscription_template_versions b ON b.template_id=a.template_id AND b.version=2 WHERE a.template_id='default-mihomo' AND a.version=1").fetch_one(&state.pool).await.unwrap();
                assert!(copied);
            }
        }
        5 => assert_eq!(body["config"]["password"], "source-receipt-secret"),
        6 => assert!(body["document"].is_object()),
        7 => assert!(body["diagnostics"].is_array()),
        8 => assert!(body["document"].to_string().contains("Renamed")),
        9 => assert!(body.to_string().contains("preview-receipt-secret")),
        _ => unreachable!(),
    }
    if matches!(case.kind, 5 | 9) {
        let cipher: String = sqlx::query_scalar("SELECT response_enc FROM management_receipts")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert!(!cipher.contains("source-receipt-secret"));
        assert!(!cipher.contains("preview-receipt-secret"));
    }
}

#[tokio::test]
async fn ten_same_key_racers_replay_exact_results_and_secrets_even_after_all_sources_are_deleted() {
    let client = client();
    for kind in 0..10 {
        let (state, case) = fixture(kind).await;
        let (base, server) = serve(state.clone()).await;
        let (a, b) = tokio::join!(
            send(&client, &base, &case, Some("race")),
            send(&client, &base, &case, Some("race"))
        );
        assert_eq!(a.status(), case.status, "kind {kind}");
        assert_eq!(b.status(), case.status, "kind {kind}");
        assert_eq!(a.headers()["cache-control"], "no-store");
        let bytes = a.bytes().await.unwrap();
        assert_eq!(b.bytes().await.unwrap(), bytes);
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_committed(&state, &case, &body).await;
        sqlx::raw_sql("DELETE FROM subscription_template_versions; DELETE FROM subscription_templates; DELETE FROM users; DELETE FROM nodes")
            .execute(&state.pool)
            .await
            .unwrap();
        let before = snapshot(&state, false).await;
        let response = send(&client, &base, &case, Some("race")).await;
        assert_eq!(response.status(), case.status);
        assert_eq!(response.bytes().await.unwrap(), bytes);
        assert_eq!(snapshot(&state, false).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn ten_receipt_failures_roll_back_all_business_and_native_conflicts_commit_one_result() {
    let client = client();
    for code in ["40001", "40P01"] {
        for kind in 0..10 {
            let (state, case) = fixture(kind).await;
            let (base, server) = serve(state.clone()).await;
            sqlx::raw_sql("CREATE FUNCTION reject_template_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER reject_template_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_template_receipt()")
                .execute(&state.pool).await.unwrap();
            let before = snapshot(&state, true).await;
            let audit_version = count(
                &state,
                "SELECT sum(generation)::bigint FROM cache_bucket_versions WHERE domain='audit'",
            )
            .await;
            assert_eq!(
                send(&client, &base, &case, Some("retry")).await.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "kind {kind}"
            );
            assert_eq!(snapshot(&state, true).await, before, "kind {kind}");
            let failed = if kind < 5 { 1 } else { 0 };
            assert_eq!(count(&state,"SELECT count(*) FROM audit_records WHERE actor='template-operator' AND detail_json->>'result'='failure'").await,failed);
            assert_eq!(
                count(
                    &state,
                    "SELECT sum(generation)::bigint FROM cache_bucket_versions WHERE domain='audit'"
                )
                .await,
                audit_version + failed
            );
            sqlx::raw_sql(&format!("CREATE SEQUENCE template_attempt; CREATE OR REPLACE FUNCTION reject_template_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('template_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$"))
                .execute(&state.pool).await.unwrap();
            let response = send(&client, &base, &case, Some("retry")).await;
            assert_eq!(response.status(), case.status, "kind {kind}, code {code}");
            let bytes = response.bytes().await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_committed(&state, &case, &body).await;
            assert_eq!(
                count(&state, "SELECT last_value FROM template_attempt").await,
                2
            );
            let before = snapshot(&state, false).await;
            assert_eq!(
                send(&client, &base, &case, Some("retry"))
                    .await
                    .bytes()
                    .await
                    .unwrap(),
                bytes
            );
            assert_eq!(snapshot(&state, false).await, before);
            server.abort();
        }
    }
}

#[tokio::test]
async fn ten_missing_keys_and_conflicting_requests_cannot_change_or_recompute_a_result() {
    let client = client();
    for kind in 0..10 {
        let (state, mut case) = fixture(kind).await;
        let (base, server) = serve(state.clone()).await;
        let before = snapshot(&state, false).await;
        assert_eq!(
            send(&client, &base, &case, None).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(snapshot(&state, false).await, before);
        assert_eq!(
            send(&client, &base, &case, Some("same")).await.status(),
            case.status
        );
        let before = snapshot(&state, false).await;
        case.body["unexpected"] = json!(true);
        assert_eq!(
            send(&client, &base, &case, Some("same")).await.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(snapshot(&state, false).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn replay_preserves_the_original_preview_but_new_requests_recheck_current_user_authority() {
    let (state, case) = fixture(9).await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let response = send(&client, &base, &case, Some("preview")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.bytes().await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("preview-receipt-secret"));
    sqlx::query("UPDATE users SET enabled=false,revision=revision+1 WHERE id='preview-user'")
        .execute(&state.pool)
        .await
        .unwrap();
    let before = snapshot(&state, false).await;
    assert_eq!(
        send(&client, &base, &case, Some("preview"))
            .await
            .bytes()
            .await
            .unwrap(),
        bytes
    );
    assert_eq!(snapshot(&state, false).await, before);
    assert_eq!(
        send(&client, &base, &case, Some("fresh-preview"))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(snapshot(&state, false).await, before);
    server.abort();
}
