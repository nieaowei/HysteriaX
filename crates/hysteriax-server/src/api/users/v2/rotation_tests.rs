use super::tests::{client, count, fixture, serve};
use super::*;

fn endpoint(base: &str, subscription: bool) -> String {
    format!(
        "{base}/{}",
        if subscription {
            "subscription"
        } else {
            "credentials/rotate"
        }
    )
}

async fn state(subscription: bool) -> AppState {
    let state = fixture().await;
    if subscription {
        sqlx::query("INSERT INTO subscription_credentials(id,user_id,token_hash,token_enc,created_at) VALUES('old','user',$1,$2,clock_timestamp())")
            .bind(crate::security::token_digest("old-subscription")).bind(state.secrets.encrypt("old-subscription").unwrap())
            .execute(&state.pool).await.unwrap();
    }
    state
}

async fn post(
    client: &reqwest::Client,
    uri: &str,
    key: Option<&str>,
    revision: i64,
) -> reqwest::Response {
    let mut request = client
        .post(uri)
        .bearer_auth("admin")
        .json(&json!({"expected_revision":revision}));
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    request.send().await.unwrap()
}

async fn snapshot(state: &AppState) -> Value {
    let subscriptions: Value = sqlx::query_scalar("SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY id),'[]'::jsonb) FROM subscription_credentials t")
        .fetch_one(&state.pool).await.unwrap();
    json!({"state":super::tests::snapshot(state).await,"subscriptions":subscriptions})
}

async fn assert_material(state: &AppState, subscription: bool, response: &Value) {
    let secret = if subscription {
        assert_eq!(
            count(
                state,
                "SELECT count(*) FROM subscription_credentials WHERE revoked_at IS NULL"
            )
            .await,
            1
        );
        assert_eq!(count(state,"SELECT count(*) FROM subscription_credentials WHERE id='old' AND revoked_at IS NOT NULL").await,1);
        response["token"].as_str().unwrap()
    } else {
        assert_eq!(
            count(state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
            1
        );
        assert_eq!(count(state, "SELECT count(*) FROM job_events").await, 1);
        assert_eq!(response["credentials"][0]["node_id"], "node");
        response["credentials"][0]["credential"].as_str().unwrap()
    };
    let query = if subscription {
        "SELECT token_hash,token_enc FROM subscription_credentials WHERE revoked_at IS NULL"
    } else {
        "SELECT credential_hash,credential_enc FROM node_assignments WHERE user_id='user' AND node_id='node'"
    };
    let (hash, cipher): (String, String) =
        sqlx::query_as(query).fetch_one(&state.pool).await.unwrap();
    assert_eq!(hash, crate::security::token_digest(secret));
    assert_eq!(state.secrets.decrypt(&cipher).unwrap(), secret);
    let cipher: String = sqlx::query_scalar(
        "SELECT response_enc FROM management_receipts ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(!cipher.contains(secret));
    let decrypted: Value = serde_json::from_str(&state.secrets.decrypt(&cipher).unwrap()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(decrypted["response"]["body"].as_str().unwrap()).unwrap(),
        *response
    );
}

#[tokio::test]
async fn secret_rotation_replays_after_user_deletion_without_preparing_or_rotating_again() {
    for subscription in [false, true] {
        let state = state(subscription).await;
        let (base, server) = serve(state.clone()).await;
        let uri = endpoint(&base, subscription);
        let client = client();
        let first = post(&client, &uri, Some("rotation"), 1).await;
        assert_eq!(first.status(), StatusCode::OK);
        let first = first.bytes().await.unwrap();
        assert_material(
            &state,
            subscription,
            &serde_json::from_slice(&first).unwrap(),
        )
        .await;
        assert_eq!(
            count(&state, "SELECT revision FROM users WHERE id='user'").await,
            2
        );
        assert_eq!(count(&state, "SELECT count(*) FROM audit_records").await, 1);
        sqlx::query("DELETE FROM users WHERE id='user'")
            .execute(&state.pool)
            .await
            .unwrap();
        let before = snapshot(&state).await;
        let replay = post(&client, &uri, Some("rotation"), 1).await;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(replay.bytes().await.unwrap(), first);
        assert_eq!(snapshot(&state).await, before);
        assert_eq!(
            post(&client, &uri, Some("rotation"), 99).await.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn concurrent_secret_rotations_reuse_one_result_and_revision_conflicts_leave_no_receipt() {
    for subscription in [false, true] {
        let state = state(subscription).await;
        let (base, server) = serve(state.clone()).await;
        let uri = endpoint(&base, subscription);
        let client = client();
        let (a, b) = tokio::join!(
            post(&client, &uri, Some("same"), 1),
            post(&client, &uri, Some("same"), 1)
        );
        assert_eq!(a.status(), StatusCode::OK);
        assert_eq!(b.status(), StatusCode::OK);
        let first = a.bytes().await.unwrap();
        assert_eq!(b.bytes().await.unwrap(), first);
        assert_material(
            &state,
            subscription,
            &serde_json::from_slice(&first).unwrap(),
        )
        .await;
        let (a, b) = tokio::join!(
            post(&client, &uri, Some("one"), 2),
            post(&client, &uri, Some("two"), 2)
        );
        let statuses = [a.status(), b.status()];
        assert_eq!(statuses.iter().filter(|s| **s == StatusCode::OK).count(), 1);
        assert_eq!(
            statuses
                .iter()
                .filter(|s| **s == StatusCode::CONFLICT)
                .count(),
            1
        );
        assert_eq!(
            count(&state, "SELECT revision FROM users WHERE id='user'").await,
            3
        );
        assert_eq!(count(&state, "SELECT count(*) FROM audit_records").await, 2);
        assert_eq!(
            count(&state, "SELECT count(*) FROM management_receipts").await,
            2
        );
        server.abort();
    }
}

#[tokio::test]
async fn rotation_receipt_failure_rolls_back_all_secrets_and_native_retry_commits_one_fixed_response()
 {
    for subscription in [false, true] {
        for code in ["40001", "40P01"] {
            let state = state(subscription).await;
            let (base, server) = serve(state.clone()).await;
            let uri = endpoint(&base, subscription);
            let client = client();
            sqlx::raw_sql("CREATE FUNCTION reject_rotation_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER reject_rotation_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_rotation_receipt()")
                .execute(&state.pool).await.unwrap();
            let before = snapshot(&state).await;
            assert_eq!(
                post(&client, &uri, Some("retry"), 1).await.status(),
                StatusCode::INTERNAL_SERVER_ERROR
            );
            assert_eq!(snapshot(&state).await, before);
            sqlx::raw_sql(&format!("DROP TRIGGER reject_rotation_receipt ON management_receipts; CREATE SEQUENCE rotation_receipt_attempt; CREATE FUNCTION retry_rotation_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('rotation_receipt_attempt')=1 THEN RAISE EXCEPTION 'native conflict' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_rotation_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION retry_rotation_receipt()"))
                .execute(&state.pool).await.unwrap();
            let response = post(&client, &uri, Some("retry"), 1).await;
            assert_eq!(response.status(), StatusCode::OK);
            let response = response.bytes().await.unwrap();
            assert_material(
                &state,
                subscription,
                &serde_json::from_slice(&response).unwrap(),
            )
            .await;
            assert_eq!(
                post(&client, &uri, Some("retry"), 1)
                    .await
                    .bytes()
                    .await
                    .unwrap(),
                response
            );
            assert_eq!(
                count(&state, "SELECT last_value FROM rotation_receipt_attempt").await,
                2
            );
            assert_eq!(
                count(&state, "SELECT count(*) FROM management_receipts").await,
                1
            );
            assert_eq!(count(&state, "SELECT count(*) FROM audit_records").await, 1);
            server.abort();
        }
    }
}

#[tokio::test]
async fn missing_key_or_stale_rotation_revision_cannot_change_credentials_or_consume_a_key() {
    for subscription in [false, true] {
        let state = state(subscription).await;
        let (base, server) = serve(state.clone()).await;
        let uri = endpoint(&base, subscription);
        let client = client();
        let before = snapshot(&state).await;
        assert_eq!(
            post(&client, &uri, None, 1).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post(&client, &uri, Some("stale"), 99).await.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}
