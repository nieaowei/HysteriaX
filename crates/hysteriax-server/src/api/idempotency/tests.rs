use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const SECRET: &str = "successful-response-secret";

#[tokio::test]
async fn preparation_lookup_is_readonly_and_reuses_authenticated_bound_proof_without_row_locks() {
    let state = fixture().await;
    let identity = identity(&state, "one", "create", "lookup", json!(1));
    assert!(replay(&state, &identity).await.unwrap().is_none());
    assert_eq!(counts(&state).await, (0, 0, 0));
    let first = mutate(
        &state,
        identity.clone(),
        "committed",
        Arc::new(AtomicUsize::new(0)),
    )
    .await
    .unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT actor_id FROM management_receipts FOR UPDATE")
        .fetch_all(&mut *held)
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            replay(&state, &identity)
        )
        .await
        .unwrap()
        .unwrap(),
        Some(first)
    );
    held.rollback().await.unwrap();
    let mut changed = identity.clone();
    changed.digest = "0".repeat(64);
    assert_eq!(
        replay(&state, &changed).await.unwrap_err().status,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE admin_tokens SET revoked_at=clock_timestamp() WHERE id='one'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        replay(&state, &identity).await.unwrap_err().status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(counts(&state).await, (1, 1, 1));
}

async fn fixture() -> AppState {
    let pool = db::test_pool_with_max_connections(8).await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('one',$1,'one',clock_timestamp()),('two',$2,'two',clock_timestamp())")
        .bind(crate::security::token_digest("one")).bind(crate::security::token_digest("two"))
        .execute(&pool).await.unwrap();
    AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    )
}

fn identity(
    state: &AppState,
    actor: &str,
    operation: &str,
    key: &str,
    body: Value,
) -> RequestIdentity {
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", key.parse().unwrap());
    RequestIdentity::from_headers(&state.secrets, actor, operation, &headers, body).unwrap()
}

async fn mutate(
    state: &AppState,
    request: RequestIdentity,
    id: &str,
    calls: Arc<AtomicUsize>,
) -> Result<StoredResponse, ApiError> {
    let id = id.to_owned();
    let actor = request.actor.clone();
    execute(state, request, move |tx| {
        let (id, actor, calls) = (id.clone(), actor.clone(), calls.clone());
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES($1,'Receipt user',TRUE,clock_timestamp(),clock_timestamp())")
                .bind(&id).execute(&mut **tx).await?;
            super::super::audit_on(tx, &actor, "user.created", "user", &id, json!({})).await?;
            Ok(StoredResponse::json(StatusCode::CREATED, json!({"id":id,"token":SECRET})))
        })
    }).await
}

async fn counts(state: &AppState) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM users),(SELECT count(*) FROM audit_records),(SELECT count(*) FROM management_receipts)")
        .fetch_one(&state.pool).await.unwrap()
}

#[test]
fn header_and_keyed_digest_contract() {
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    let other = SecretBox::from_base64("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE").unwrap();
    for values in [vec![], vec![""], vec!["has space"], vec!["one", "two"]] {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append("idempotency-key", value.parse().unwrap());
        }
        assert_eq!(
            RequestIdentity::from_headers(&secrets, "one", "create", &headers, json!({}))
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", "x".repeat(201).parse().unwrap());
    assert!(RequestIdentity::from_headers(&secrets, "one", "create", &headers, json!({})).is_err());
    headers.insert("idempotency-key", "valid:operation-1".parse().unwrap());
    let first = RequestIdentity::from_headers(
        &secrets,
        "one",
        "create",
        &headers,
        json!({"password":"guessable"}),
    )
    .unwrap();
    assert_eq!(
        first.digest,
        RequestIdentity::from_headers(
            &secrets,
            "one",
            "create",
            &headers,
            json!({"password":"guessable"})
        )
        .unwrap()
        .digest
    );
    assert_ne!(
        first.digest,
        RequestIdentity::from_headers(
            &other,
            "one",
            "create",
            &headers,
            json!({"password":"guessable"})
        )
        .unwrap()
        .digest
    );
    assert_ne!(
        secrets.request_digest("one", b"same"),
        secrets.request_digest("two", b"same")
    );
}

#[tokio::test]
async fn committed_secret_response_replays_exactly_without_business_or_audit_reexecution() {
    let state = fixture().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let request = identity(&state, "one", "users.create", "key", json!({"name":"A"}));
    let first = mutate(&state, request.clone(), "first", calls.clone())
        .await
        .unwrap();
    // Model a caller losing the first HTTP response after its transaction committed.
    let second = mutate(&state, request, "replacement-must-not-exist", calls.clone())
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(counts(&state).await, (1, 1, 1));
    let (cipher, digest): (String, String) =
        sqlx::query_as("SELECT response_enc,request_digest FROM management_receipts")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(!cipher.contains(SECRET));
    assert!(!cipher.contains("first"));
    assert_eq!(digest.len(), 64);
    let envelope: Envelope =
        serde_json::from_str(&state.secrets.decrypt(&cipher).unwrap()).unwrap();
    assert_eq!(envelope.response, first);
    let response = second.into_http().unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-type"], "application/json");
}

#[tokio::test]
async fn key_conflicts_are_scoped_to_actor_and_operation() {
    let state = fixture().await;
    let calls = Arc::new(AtomicUsize::new(0));
    mutate(
        &state,
        identity(&state, "one", "create", "key", json!(1)),
        "first",
        calls.clone(),
    )
    .await
    .unwrap();
    let error = mutate(
        &state,
        identity(&state, "one", "create", "key", json!(2)),
        "conflict",
        calls.clone(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.code, "idempotency_conflict");
    mutate(
        &state,
        identity(&state, "two", "create", "key", json!(1)),
        "other-actor",
        calls.clone(),
    )
    .await
    .unwrap();
    mutate(
        &state,
        identity(&state, "one", "update", "key", json!(1)),
        "other-operation",
        calls.clone(),
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(counts(&state).await, (3, 3, 3));
}

#[tokio::test]
async fn concurrent_same_key_executes_business_once_and_conflicting_body_does_not_execute() {
    let state = fixture().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let request = identity(&state, "one", "create", "concurrent", json!(1));
    let (a, b) = tokio::join!(
        mutate(&state, request.clone(), "candidate-a", calls.clone()),
        mutate(&state, request, "candidate-b", calls.clone())
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let (a, b) = tokio::join!(
        mutate(
            &state,
            identity(&state, "one", "create", "conflicting", json!(1)),
            "candidate-c",
            calls.clone()
        ),
        mutate(
            &state,
            identity(&state, "one", "create", "conflicting", json!(2)),
            "candidate-d",
            calls.clone()
        )
    );
    assert!(a.is_ok() ^ b.is_ok());
    assert_eq!(a.err().or(b.err()).unwrap().status, StatusCode::CONFLICT);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(counts(&state).await, (2, 2, 2));
}

#[tokio::test]
async fn receipt_failure_rolls_back_business_audit_and_reservation_and_native_errors_retry_fixed_result()
 {
    let state = fixture().await;
    let calls = Arc::new(AtomicUsize::new(0));
    sqlx::raw_sql("CREATE FUNCTION reject_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'rejected'; END $$; CREATE TRIGGER reject_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_receipt()")
        .execute(&state.pool).await.unwrap();
    let request = identity(&state, "one", "create", "key", json!(1));
    assert_eq!(
        mutate(&state, request.clone(), "fixed", calls.clone())
            .await
            .unwrap_err()
            .status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(counts(&state).await, (0, 0, 0));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    sqlx::query("DROP TRIGGER reject_receipt ON management_receipts")
        .execute(&state.pool)
        .await
        .unwrap();
    for code in ["40001", "40P01"] {
        sqlx::raw_sql(&format!("CREATE SEQUENCE receipt_attempt; CREATE FUNCTION retry_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('receipt_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION retry_receipt()"))
            .execute(&state.pool).await.unwrap();
        let first = mutate(
            &state,
            identity(&state, "one", "create", code, json!(1)),
            code,
            calls.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&first.body).unwrap()["id"],
            code
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT last_value FROM receipt_attempt")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            2
        );
        sqlx::raw_sql("DROP TRIGGER retry_receipt ON management_receipts; DROP FUNCTION retry_receipt(); DROP SEQUENCE receipt_attempt")
            .execute(&state.pool).await.unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    assert_eq!(counts(&state).await, (2, 2, 2));
    // A failed request did not consume its key.
    mutate(&state, request, "fixed", calls.clone())
        .await
        .unwrap();
    assert_eq!(counts(&state).await, (3, 3, 3));
}

#[tokio::test]
async fn empty_reservation_cannot_commit_and_invalid_response_rolls_back() {
    let state = fixture().await;
    let error = sqlx::query("INSERT INTO management_receipts(actor_id,operation,idempotency_key,request_digest,response_enc) VALUES('one','create','empty',repeat('a',64),'')")
        .execute(&state.pool).await.unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("23514")
    );
    let error = execute(&state,identity(&state,"one","create","bad-response",json!(1)),|tx|Box::pin(async move {
        sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('bad-response','bad',clock_timestamp(),clock_timestamp())").execute(&mut **tx).await?;
        Ok(StoredResponse::json(StatusCode::BAD_REQUEST,json!({})))
    })).await.unwrap_err();
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(counts(&state).await, (0, 0, 0));
    // Even if an operator bypassed the constraint, an old empty reservation is
    // missing proof, rather than permission to execute the business work again.
    sqlx::query("ALTER TABLE management_receipts DISABLE TRIGGER management_receipt_complete")
        .execute(&state.pool)
        .await
        .unwrap();
    let damaged = identity(&state, "one", "create", "damaged", json!(1));
    sqlx::query("INSERT INTO management_receipts(actor_id,operation,idempotency_key,request_digest,response_enc) VALUES('one','create','damaged',$1,'')")
        .bind(&damaged.digest).execute(&state.pool).await.unwrap();
    sqlx::query("ALTER TABLE management_receipts ENABLE TRIGGER management_receipt_complete")
        .execute(&state.pool)
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        mutate(&state, damaged, "must-not-write", calls.clone())
            .await
            .unwrap_err()
            .status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(counts(&state).await, (0, 0, 1));
}

#[tokio::test]
async fn replay_requires_active_actor_and_ciphertext_is_bound_to_its_request() {
    let state = fixture().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let request = identity(&state, "one", "create", "one", json!(1));
    mutate(&state, request.clone(), "first", calls.clone())
        .await
        .unwrap();
    let other = identity(&state, "two", "create", "two", json!(1));
    mutate(&state, other.clone(), "second", calls.clone())
        .await
        .unwrap();
    sqlx::query("UPDATE management_receipts SET response_enc=(SELECT response_enc FROM management_receipts WHERE actor_id='one') WHERE actor_id='two'")
        .execute(&state.pool).await.unwrap();
    assert_eq!(
        mutate(&state, other, "must-not-write", calls.clone())
            .await
            .unwrap_err()
            .status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    sqlx::query("UPDATE admin_tokens SET revoked_at=clock_timestamp() WHERE id='one'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        mutate(&state, request.clone(), "must-not-write", calls.clone())
            .await
            .unwrap_err()
            .status,
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("DELETE FROM admin_tokens WHERE id='one'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        mutate(&state, request, "must-not-write", calls.clone())
            .await
            .unwrap_err()
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(counts(&state).await, (2, 2, 2));
}

async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v2/users", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, super::super::router(state))
            .await
            .unwrap()
    });
    (base, server)
}

#[tokio::test]
async fn actual_v2_http_requires_header_and_replays_across_servers_with_one_user_and_audit() {
    let state = fixture().await;
    let (one, server_one) = serve(state.clone()).await;
    let (two, server_two) = serve(AppState::new(
        state.pool.clone(),
        state.secrets.as_ref().clone(),
    ))
    .await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let body = json!({"name":"HTTP user","quota_bytes":123});
    assert_eq!(
        client
            .post(&one)
            .bearer_auth("one")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .post(&one)
            .bearer_auth("one")
            .header("idempotency-key", "one")
            .header("idempotency-key", "two")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(counts(&state).await, (0, 0, 0));
    let first = client
        .post(&one)
        .bearer_auth("one")
        .header("idempotency-key", "http-key")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_body = first.bytes().await.unwrap();
    // Replay through another HTTP listener sharing only the authoritative state.
    let second = client
        .post(&two)
        .bearer_auth("one")
        .header("idempotency-key", "http-key")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CREATED);
    assert_eq!(second.headers()["cache-control"], "no-store");
    assert_eq!(second.bytes().await.unwrap(), first_body);
    assert_eq!(counts(&state).await, (1, 1, 1));
    assert_eq!(
        client
            .post(&two)
            .bearer_auth("one")
            .header("idempotency-key", "http-key")
            .json(&json!({"name":"HTTP user","quota_bytes":123,"ignored_extra":true}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT actor FROM audit_records")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "one"
    );
    let conflict = client
        .post(&two)
        .bearer_auth("one")
        .header("idempotency-key", "http-key")
        .json(&json!({"name":"different"}))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        conflict.json::<Value>().await.unwrap()["error"]["code"],
        "idempotency_conflict"
    );
    assert_eq!(
        client
            .get(&two)
            .bearer_auth("one")
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["total"],
        1
    );
    sqlx::query("UPDATE admin_tokens SET revoked_at=clock_timestamp() WHERE id='one'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        client
            .post(&two)
            .bearer_auth("one")
            .header("idempotency-key", "http-key")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(counts(&state).await, (1, 1, 1));
    server_one.abort();
    server_two.abort();
}

#[tokio::test]
async fn actual_http_timeout_after_commit_retries_on_another_server_without_duplicate_mutation() {
    let state = fixture().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let slow = format!("http://{}/api/v2/users", listener.local_addr().unwrap());
    let app = super::super::router(state.clone()).layer(axum::middleware::from_fn(
        |request: axum::extract::Request, next: axum::middleware::Next| async move {
            let response = next.run(request).await;
            // The handler has committed; withhold the response past the client deadline.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            response
        },
    ));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (other, other_server) = serve(AppState::new(
        state.pool.clone(),
        state.secrets.as_ref().clone(),
    ))
    .await;
    let body = json!({"name":"Lost response"});
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let error = client
        .post(slow)
        .bearer_auth("one")
        .header("idempotency-key", "lost-response")
        .json(&body)
        .timeout(std::time::Duration::from_millis(500))
        .send()
        .await
        .unwrap_err();
    assert!(error.is_timeout());
    assert_eq!(counts(&state).await, (1, 1, 1));
    let committed: String = sqlx::query_scalar("SELECT id FROM users")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let replay = client
        .post(other)
        .bearer_auth("one")
        .header("idempotency-key", "lost-response")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(replay.json::<Value>().await.unwrap()["id"], committed);
    assert_eq!(counts(&state).await, (1, 1, 1));
    server.abort();
    other_server.abort();
}
