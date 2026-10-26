use super::*;
use reqwest::{Client, Method};

struct Case {
    method: Method,
    path: String,
    body: Value,
    status: StatusCode,
    ca: Option<String>,
}
async fn state() -> AppState {
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    let pool = db::test_pool_with_max_connections(8).await;
    credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('operator',$1,'Operator',clock_timestamp(),clock_timestamp()),('other',$2,'Other',clock_timestamp(),clock_timestamp())")
        .bind(crate::security::token_digest("admin")).bind(crate::security::token_digest("other")).execute(&pool).await.unwrap();
    AppState::new(pool, secrets)
}
fn creation() -> Value {
    json!({"name":"V2 CA","source":"generated"})
}
async fn fixture(kind: usize) -> (AppState, Case) {
    let state = state().await;
    if kind == 0 {
        return (
            state,
            Case {
                method: Method::POST,
                path: "/credentials/ca-identities".into(),
                body: creation(),
                status: StatusCode::CREATED,
                ca: None,
            },
        );
    }
    let actor = AdminActor {
        id: "fixture".into(),
    };
    let (_, Json(created)) = super::super::create(
        State(state.clone()),
        Extension(actor),
        Json(CreateAuthority {
            name: "Fixture CA".into(),
            source: Source::Generated,
            certificate: None,
            private_key: None,
            policy: Policy::default(),
            idempotency_key: "fixture-create".into(),
        }),
    )
    .await
    .unwrap();
    let ca = created["id"].as_str().unwrap().to_owned();
    if kind == 1 {
        return (
            state,
            Case {
                method: Method::PATCH,
                path: format!("/credentials/{ca}/ca-policy"),
                body: json!({"expected_revision":1,"policy":{"client_validity_days":30,"renew_before_hours":24}}),
                status: StatusCode::OK,
                ca: Some(ca),
            },
        );
    }
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES('alice','Alice',true,clock_timestamp(),clock_timestamp())").execute(&state.pool).await.unwrap();
    let candidate =
        credentials::identities::prepare_user(&state.pool, &state.secrets, "alice", &ca, 1, false)
            .await
            .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let (identity, _) = credentials::identities::commit_user(&mut tx, &state.secrets, candidate)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let case = if kind == 2 {
        Case {
            method: Method::POST,
            path: format!("/credentials/{ca}/ca-versions"),
            body: json!({"expected_revision":1}),
            status: StatusCode::ACCEPTED,
            ca: Some(ca),
        }
    } else {
        let revision: i64 =
            sqlx::query_scalar("SELECT revision FROM mtls_user_identities WHERE id=$1")
                .bind(&identity)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        Case {
            method: Method::POST,
            path: "/users/alice/managed-certificates/reissue".into(),
            body: json!({"ca_credential_id":ca,"expected_revisions":{ca.clone():revision}}),
            status: StatusCode::OK,
            ca: Some(ca),
        }
    };
    (state, case)
}
async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v2", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, crate::api::router(state))
            .await
            .unwrap()
    });
    (base, server)
}
fn client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(30))
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
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn snapshot(state: &AppState, exclude_failure_audit: bool) -> Value {
    let tables = [
        "users",
        "credentials",
        "credential_versions",
        "mtls_authorities",
        "mtls_ca_generations",
        "mtls_rotations",
        "mtls_trust_cleanup",
        "mtls_user_identities",
        "mtls_user_versions",
        "mtls_operations",
        "credential_batches",
        "credential_batch_items",
        "background_work",
        "jobs",
        "job_events",
        "credential_reference_edges",
        "cache_bucket_versions",
        "cache_entity_versions",
        "audit_records",
    ];
    let parts:Vec<String>=tables.iter().map(|t| {
        // Failed request auditing deliberately commits separately from the business.
        let predicate=match (*t,exclude_failure_audit) {
            ("audit_records",true)=>" WHERE action NOT LIKE '%request_failed'",
            ("cache_bucket_versions",true)=>" WHERE domain<>'audit'",
            _=>"",
        };
        format!("SELECT '{t}' AS kind,coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) AS data FROM {t} t{predicate}")
    }).collect();
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT jsonb_object_agg(kind,data) FROM ({}) snapshots",
        parts.join(" UNION ALL ")
    )))
    .fetch_one(&state.pool)
    .await
    .unwrap()
}
async fn assert_committed(state: &AppState, kind: usize) {
    assert_eq!(
        count(state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    assert_eq!(
        count(
            state,
            "SELECT count(*) FROM mtls_operations WHERE actor_id='operator'"
        )
        .await,
        1
    );
    assert_eq!(count(state,"SELECT count(*) FROM audit_records WHERE actor='operator' AND action NOT LIKE '%request_failed'").await,1);
    match kind {
        0 => {
            assert_eq!(
                count(
                    state,
                    "SELECT count(*) FROM credentials WHERE kind='ca_identity'"
                )
                .await,
                1
            );
            assert_eq!(
                count(state, "SELECT count(*) FROM mtls_ca_generations").await,
                1
            );
        }
        1 => {
            assert_eq!(
                count(state, "SELECT policy_revision FROM mtls_authorities").await,
                2
            );
            assert_eq!(
                count(
                    state,
                    "SELECT latest_version FROM credentials WHERE kind='ca_identity'"
                )
                .await,
                1
            );
            assert_eq!(count(state, "SELECT count(*) FROM mtls_rotations").await, 0);
        }
        2 => {
            assert_eq!(
                count(
                    state,
                    "SELECT latest_version FROM credentials WHERE kind='ca_identity'"
                )
                .await,
                2
            );
            assert_eq!(
                count(state, "SELECT published_version FROM mtls_authorities").await,
                1
            );
            assert_eq!(
                count(
                    state,
                    "SELECT count(*) FROM mtls_rotations WHERE phase='trust'"
                )
                .await,
                1
            );
            assert_eq!(
                count(state, "SELECT count(*) FROM credential_batches").await,
                1
            );
        }
        3 => {
            assert_eq!(
                count(state, "SELECT count(*) FROM mtls_user_versions").await,
                2
            );
            assert_eq!(
                count(state, "SELECT revision FROM users WHERE id='alice'").await,
                1
            );
            assert_eq!(count(state, "SELECT count(*) FROM jobs").await, 0);
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn four_same_key_racers_commit_once_and_restore_the_exact_response_without_legacy_proof_or_active_sources()
 {
    let client = client();
    for kind in 0..4 {
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
        assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE KEY"));
        assert_committed(&state, kind).await;
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        sqlx::query("DELETE FROM mtls_operations")
            .execute(&state.pool)
            .await
            .unwrap();
        if kind < 2 {
            assert_eq!(
                crate::api::credentials::delete(
                    State(state.clone()),
                    Extension(AdminActor {
                        id: "operator".into()
                    }),
                    Path(value["id"].as_str().unwrap().to_owned()),
                    axum::extract::Query(crate::api::nodes::RevisionQuery {
                        expected_revision: Some(1)
                    }),
                )
                .await
                .unwrap(),
                StatusCode::NO_CONTENT
            );
        } else {
            sqlx::raw_sql("UPDATE credentials SET archived=true; UPDATE mtls_user_identities SET archived=true").execute(&state.pool).await.unwrap();
        }
        let before = snapshot(&state, false).await;
        let response = send(&client, &base, &case, "admin", Some("race")).await;
        assert_eq!(response.status(), case.status);
        assert_eq!(response.bytes().await.unwrap(), bytes);
        assert_eq!(snapshot(&state, false).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn four_receipt_failures_leave_only_redacted_failure_audits_and_native_retries_commit_once() {
    let client = client();
    for code in ["40001", "40P01"] {
        for kind in 0..4 {
            let (state, case) = fixture(kind).await;
            let (base, server) = serve(state.clone()).await;
            sqlx::raw_sql("CREATE FUNCTION reject_mtls_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER reject_mtls_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_mtls_receipt()")
                .execute(&state.pool).await.unwrap();
            let before = snapshot(&state, true).await;
            let audit_version = count(
                &state,
                "SELECT sum(generation)::bigint FROM cache_bucket_versions WHERE domain='audit'",
            )
            .await;
            assert_eq!(
                send(&client, &base, &case, "admin", Some("retry"))
                    .await
                    .status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "kind {kind}"
            );
            assert_eq!(snapshot(&state, true).await, before, "kind {kind}");
            assert_eq!(count(&state,"SELECT count(*) FROM audit_records WHERE actor='operator' AND action LIKE '%request_failed'").await,1);
            assert_eq!(
                count(
                    &state,
                    "SELECT sum(generation)::bigint FROM cache_bucket_versions WHERE domain='audit'"
                )
                .await,
                audit_version + 1
            );
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SEQUENCE mtls_attempt; CREATE OR REPLACE FUNCTION reject_mtls_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('mtls_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$")))
                .execute(&state.pool).await.unwrap();
            let response = send(&client, &base, &case, "admin", Some("retry")).await;
            assert_eq!(response.status(), case.status, "kind {kind}, code {code}");
            let bytes = response.bytes().await.unwrap();
            assert_committed(&state, kind).await;
            assert_eq!(
                count(&state, "SELECT last_value FROM mtls_attempt").await,
                2
            );
            let before = snapshot(&state, false).await;
            assert_eq!(
                send(&client, &base, &case, "admin", Some("retry"))
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
async fn missing_keys_do_not_prepare_material_and_changed_requests_cannot_replace_a_committed_result()
 {
    let client = client();
    for kind in 0..4 {
        let (state, mut case) = fixture(kind).await;
        let (base, server) = serve(state.clone()).await;
        let before = snapshot(&state, false).await;
        assert_eq!(
            send(&client, &base, &case, "admin", None).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(snapshot(&state, false).await, before);
        assert_eq!(
            send(&client, &base, &case, "admin", Some("same"))
                .await
                .status(),
            case.status
        );
        let before = snapshot(&state, false).await;
        case.body["unexpected"] = json!(true);
        assert_eq!(
            send(&client, &base, &case, "admin", Some("same"))
                .await
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(snapshot(&state, false).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn failed_imports_redact_material_and_same_header_keys_are_scoped_to_actor_and_operation() {
    let client = client();
    let state = state().await;
    let (base, server) = serve(state.clone()).await;
    let material = Authority::generate(Utc::now()).unwrap().payload();
    let canary = "private-material-canary-keep-out-of-audits";
    let invalid = Case {
        method: Method::POST,
        path: "/credentials/ca-identities".into(),
        body: json!({"name":"Invalid","source":"imported","certificate":material["certificate"],"private_key":canary}),
        status: StatusCode::CREATED,
        ca: None,
    };
    let response = send(&client, &base, &invalid, "admin", Some("shared")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!response.text().await.unwrap().contains(canary));
    assert_eq!(count(&state, "SELECT count(*) FROM credentials").await, 0);
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        0
    );
    let audit: Value =
        sqlx::query_scalar("SELECT detail_json FROM audit_records WHERE actor='operator'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(!audit.to_string().contains(canary));
    assert!(!audit.to_string().contains("BEGIN CERTIFICATE"));
    let create = Case {
        method: Method::POST,
        path: invalid.path,
        body: creation(),
        status: StatusCode::CREATED,
        ca: None,
    };
    let response = send(&client, &base, &create, "admin", Some("shared")).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let value: Value = response.json().await.unwrap();
    let policy = Case {
        method: Method::PATCH,
        path: format!("/credentials/{}/ca-policy", value["id"].as_str().unwrap()),
        body: json!({"expected_revision":1,"policy":{"client_validity_days":30,"renew_before_hours":24},"idempotency_key":"legacy-shared"}),
        status: StatusCode::OK,
        ca: None,
    };
    assert_eq!(
        send(&client, &base, &policy, "admin", Some("shared"))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        send(&client, &base, &create, "other", Some("shared"))
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        3
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_operations WHERE idempotency_key LIKE 'v2:%'"
        )
        .await,
        3
    );
    server.abort();
}

#[tokio::test]
async fn an_existing_legacy_publication_cannot_manufacture_an_encrypted_management_receipt() {
    let (state, case) = fixture(2).await;
    let ca = case.ca.clone().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", "orphan".parse().unwrap());
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        "operator",
        "mtls.ca.publish",
        &headers,
        json!({"method":"POST","uri":format!("/api/v2{}",case.path),"id":ca,"body":case.body}),
    )
    .unwrap();
    let (_, Json(_)) = super::super::publish(
        State(state.clone()),
        Extension(AdminActor {
            id: "operator".into(),
        }),
        Path(ca),
        Json(PublishAuthority {
            expected_revision: 1,
            certificate: None,
            private_key: None,
            replace_rotation_id: None,
            idempotency_key: identity.operation_key(&state.secrets).unwrap(),
        }),
    )
    .await
    .unwrap();
    let before = snapshot(&state, true).await;
    let (base, server) = serve(state.clone()).await;
    assert_eq!(
        send(&client(), &base, &case, "admin", Some("orphan"))
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state, true).await, before);
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        0
    );
    server.abort();
}
