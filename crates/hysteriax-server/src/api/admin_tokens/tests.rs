use super::*;
use crate::{db, security::SecretBox};

async fn fixture() -> AppState {
    let pool = db::test_pool_with_max_connections(8).await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('one',$1,'one',clock_timestamp(),clock_timestamp()),('two',$2,'two',clock_timestamp(),clock_timestamp())")
        .bind(token_digest("one")).bind(token_digest("two")).execute(&pool).await.unwrap();
    AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    )
}

async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/api/v2/admin/tokens",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, super::super::router(state))
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

async fn counts(state: &AppState) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT count(*) FROM admin_tokens),(SELECT count(*) FROM admin_tokens WHERE revoked_at IS NOT NULL),(SELECT count(*) FROM audit_records),(SELECT count(*) FROM management_receipts)")
        .fetch_one(&state.pool).await.unwrap()
}

#[tokio::test]
async fn actual_token_creation_replays_identical_secret_across_servers_and_never_replaces_it() {
    let state = fixture().await;
    let (one, a) = serve(state.clone()).await;
    let (two, b) = serve(AppState::new(
        state.pool.clone(),
        state.secrets.as_ref().clone(),
    ))
    .await;
    let client = client();
    let body = json!({"label":"Shared receipt"});
    let first = client
        .post(&one)
        .bearer_auth("one")
        .header("idempotency-key", "create-token")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    assert_eq!(first.headers()["cache-control"], "no-store");
    let first = first.bytes().await.unwrap();
    let value: Value = serde_json::from_slice(&first).unwrap();
    let secret = value["token"].as_str().unwrap();
    let id = value["id"].as_str().unwrap();
    let replay = client
        .post(&two)
        .bearer_auth("one")
        .header("idempotency-key", "create-token")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(replay.bytes().await.unwrap(), first);
    assert_eq!(counts(&state).await, (3, 0, 1, 1));
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT token_hash FROM admin_tokens WHERE id=$1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        token_digest(secret)
    );
    let cipher: String = sqlx::query_scalar("SELECT response_enc FROM management_receipts")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(!cipher.contains(secret));
    let decrypted: Value = serde_json::from_str(&state.secrets.decrypt(&cipher).unwrap()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(decrypted["response"]["body"].as_str().unwrap()).unwrap(),
        value
    );
    let directory = client.get(&two).bearer_auth(secret).send().await.unwrap();
    assert_eq!(directory.status(), StatusCode::OK);
    assert!(!directory.text().await.unwrap().contains(secret));
    // Later revocation does not let replay silently manufacture a replacement credential.
    sqlx::query("UPDATE admin_tokens SET revoked_at=clock_timestamp() WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        client
            .post(&two)
            .bearer_auth("one")
            .header("idempotency-key", "create-token")
            .json(&body)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
        first
    );
    assert_eq!(counts(&state).await, (3, 1, 1, 1));
    a.abort();
    b.abort();
}

#[tokio::test]
async fn concurrent_token_creation_keeps_one_secret_and_different_body_conflicts() {
    let state = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let request = || {
        client
            .post(&base)
            .bearer_auth("one")
            .header("idempotency-key", "concurrent")
            .json(&json!({"label":"One secret"}))
            .send()
    };
    let (a, b) = tokio::join!(request(), request());
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.status(), StatusCode::CREATED);
    assert_eq!(b.status(), StatusCode::CREATED);
    let first = a.bytes().await.unwrap();
    assert_eq!(first, b.bytes().await.unwrap());
    let value: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT token_hash FROM admin_tokens WHERE id=$1")
            .bind(value["id"].as_str().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        token_digest(value["token"].as_str().unwrap())
    );
    assert_eq!(
        client
            .post(&base)
            .bearer_auth("one")
            .header("idempotency-key", "concurrent")
            .json(&json!({"label":"Different"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(counts(&state).await, (3, 0, 1, 1));
    server.abort();
}

#[tokio::test]
async fn revoke_replays_empty_success_after_target_disappears_and_conflicting_target_cannot_mutate()
{
    let state = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let response = client
        .delete(format!("{base}/two"))
        .bearer_auth("one")
        .header("idempotency-key", "revoke")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response.bytes().await.unwrap().is_empty());
    assert_eq!(counts(&state).await, (2, 1, 1, 1));
    sqlx::query("DELETE FROM admin_tokens WHERE id='two'")
        .execute(&state.pool)
        .await
        .unwrap();
    let replay = client
        .delete(format!("{base}/two"))
        .bearer_auth("one")
        .header("idempotency-key", "revoke")
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::NO_CONTENT);
    assert!(replay.bytes().await.unwrap().is_empty());
    let conflict = client
        .delete(format!("{base}/one"))
        .bearer_auth("one")
        .header("idempotency-key", "revoke")
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        conflict.json::<Value>().await.unwrap()["error"]["code"],
        "idempotency_conflict"
    );
    assert_eq!(counts(&state).await, (1, 0, 1, 1));
    assert_eq!(
        client
            .delete(format!("{base}/one"))
            .bearer_auth("one")
            .header("idempotency-key", "self")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(counts(&state).await, (1, 0, 1, 1));
    server.abort();
}

#[tokio::test]
async fn receipt_failure_rolls_back_token_creation_and_revocation_then_native_retry_commits_once() {
    let state = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    sqlx::raw_sql("CREATE FUNCTION fail_token_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER fail_token_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION fail_token_receipt()")
        .execute(&state.pool).await.unwrap();
    assert_eq!(
        client
            .post(&base)
            .bearer_auth("one")
            .header("idempotency-key", "create")
            .json(&json!({"label":"Atomic"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        client
            .delete(format!("{base}/two"))
            .bearer_auth("one")
            .header("idempotency-key", "revoke")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(counts(&state).await, (2, 0, 0, 0));
    sqlx::query("DROP TRIGGER fail_token_receipt ON management_receipts")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE SEQUENCE token_receipt_attempt; CREATE FUNCTION retry_token_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF mod(nextval('token_receipt_attempt'),2)=1 THEN RAISE EXCEPTION 'native retry' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_token_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION retry_token_receipt()")
        .execute(&state.pool).await.unwrap();
    let response = client
        .post(&base)
        .bearer_auth("one")
        .header("idempotency-key", "create")
        .json(&json!({"label":"Atomic"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let value: Value = response.json().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT token_hash FROM admin_tokens WHERE id=$1")
            .bind(value["id"].as_str().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        token_digest(value["token"].as_str().unwrap())
    );
    assert_eq!(
        client
            .delete(format!("{base}/two"))
            .bearer_auth("one")
            .header("idempotency-key", "revoke")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(counts(&state).await, (3, 1, 2, 2));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM token_receipt_attempt")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        4
    );
    server.abort();
}

#[tokio::test]
async fn concurrent_v2_cross_revocation_keeps_one_active_administrator_and_one_success_receipt() {
    let state = fixture().await;
    let (base, server) = serve(state.clone()).await;
    sqlx::raw_sql("CREATE SEQUENCE v2_revoke_arrival; CREATE FUNCTION v2_pause_revoke() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE deadline timestamptz:=clock_timestamp()+interval '3 seconds'; arrivals bigint; BEGIN PERFORM nextval('v2_revoke_arrival'); LOOP SELECT last_value INTO arrivals FROM v2_revoke_arrival; EXIT WHEN arrivals>=2; IF clock_timestamp()>deadline THEN RAISE EXCEPTION 'revoke barrier timed out'; END IF; PERFORM pg_sleep(0.01); END LOOP; RETURN NEW; END $$; CREATE TRIGGER v2_pause_revoke BEFORE UPDATE OF revoked_at ON admin_tokens FOR EACH ROW EXECUTE FUNCTION v2_pause_revoke()")
        .execute(&state.pool).await.unwrap();
    let client = client();
    let (a, b) = tokio::join!(
        client
            .delete(format!("{base}/two"))
            .bearer_auth("one")
            .header("idempotency-key", "one")
            .send(),
        client
            .delete(format!("{base}/one"))
            .bearer_auth("two")
            .header("idempotency-key", "two")
            .send()
    );
    let statuses = [a.unwrap().status(), b.unwrap().status()];
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::NO_CONTENT)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::UNAUTHORIZED)
            .count(),
        1
    );
    assert_eq!(counts(&state).await, (2, 1, 1, 1));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM v2_revoke_arrival")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
    server.abort();
}

#[tokio::test]
async fn missing_keys_and_invalid_labels_do_not_reserve_or_mutate() {
    let state = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    assert_eq!(
        client
            .post(&base)
            .bearer_auth("one")
            .json(&json!({"label":"Valid"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .delete(format!("{base}/two"))
            .bearer_auth("one")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .post(&base)
            .bearer_auth("one")
            .header("idempotency-key", "invalid")
            .json(&json!({"label":"  "}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(counts(&state).await, (2, 0, 0, 0));
    server.abort();
}

#[test]
fn stored_empty_success_has_no_body_and_json_cannot_masquerade_as_no_content() {
    let empty = StoredResponse::empty(StatusCode::NO_CONTENT)
        .into_http()
        .unwrap();
    assert_eq!(empty.status(), StatusCode::NO_CONTENT);
    assert!(empty.headers().get("content-type").is_none());
    assert!(
        StoredResponse::json(StatusCode::NO_CONTENT, json!({"hidden":true}))
            .into_http()
            .is_err()
    );
}
