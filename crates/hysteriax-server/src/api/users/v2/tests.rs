use super::*;
use chrono::{DateTime, Utc};

pub(super) async fn fixture() -> AppState {
    let state = crate::kick_requests::tests::fixture().await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('admin',$1,'Admin',clock_timestamp(),clock_timestamp())")
        .bind(crate::security::token_digest("admin")).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('user','node','hash',$1,clock_timestamp())")
        .bind(state.secrets.encrypt("old-credential").unwrap()).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE users SET quota_bytes=100,usage_bytes=90 WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    state
}

pub(super) async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/api/v2/users/user",
        listener.local_addr().unwrap()
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, crate::api::router(state))
            .await
            .unwrap()
    });
    (base, task)
}

pub(super) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap()
}

pub(super) async fn snapshot(state: &AppState) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('users',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM users t),'assignments',(SELECT jsonb_agg(to_jsonb(t) ORDER BY user_id,node_id) FROM node_assignments t),'jobs',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM jobs t),'events',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM job_events t),'audits',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM audit_records t),'kicks',(SELECT jsonb_agg(to_jsonb(t) ORDER BY node_id,user_id) FROM kick_requests t),'receipts',(SELECT jsonb_agg(to_jsonb(t) ORDER BY actor_id,operation,idempotency_key) FROM management_receipts t),'buckets',(SELECT jsonb_agg(to_jsonb(t) ORDER BY domain,bucket) FROM cache_bucket_versions t))")
        .fetch_one(&state.pool).await.unwrap()
}

pub(super) async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn disable_replay_keeps_one_revision_audit_kick_and_event() {
    let state = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let body = json!({"expected_revision":1,"enabled":false});
    let first = client
        .patch(&base)
        .bearer_auth("admin")
        .header("idempotency-key", "disable")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first = first.bytes().await.unwrap();
    let before = snapshot(&state).await;
    let replay = client
        .patch(&base)
        .bearer_auth("admin")
        .header("idempotency-key", "disable")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.bytes().await.unwrap(), first);
    assert_eq!(snapshot(&state).await, before);
    assert_eq!(
        count(&state, "SELECT revision FROM users WHERE id='user'").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM job_events").await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='user.updated' AND actor='admin'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    server.abort();
}

#[tokio::test]
async fn user_deletion_replays_after_cascade_and_changed_uri_or_body_conflicts() {
    let state = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let uri = format!("{base}?expected_revision=1");
    let first = client
        .delete(&uri)
        .bearer_auth("admin")
        .header("idempotency-key", "delete")
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    assert!(first.bytes().await.unwrap().is_empty());
    let before = snapshot(&state).await;
    let replay = client
        .delete(&uri)
        .bearer_auth("admin")
        .header("idempotency-key", "delete")
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::NO_CONTENT);
    assert!(replay.bytes().await.unwrap().is_empty());
    assert_eq!(snapshot(&state).await, before);
    assert_eq!(count(&state, "SELECT count(*) FROM users").await, 0);
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='kick'").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='user.deleted'"
        )
        .await,
        1
    );
    assert_eq!(
        client
            .delete(format!("{base}?expected_revision=2"))
            .bearer_auth("admin")
            .header("idempotency-key", "delete")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        client
            .delete(uri)
            .bearer_auth("admin")
            .header("idempotency-key", "delete")
            .body("different-body")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(snapshot(&state).await, before);
    server.abort();
}

#[tokio::test]
async fn quota_reset_uses_database_time_and_replay_does_not_erase_later_billing() {
    let state = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let before: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let body = json!({"expected_revision":1});
    let first = client
        .post(format!("{base}/quota/reset"))
        .bearer_auth("admin")
        .header("idempotency-key", "reset")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first = first.bytes().await.unwrap();
    let timestamp: DateTime<Utc> =
        sqlx::query_scalar("SELECT quota_reset_at FROM users WHERE id='user'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let after: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(before <= timestamp && timestamp <= after);
    assert_eq!(
        serde_json::from_slice::<Value>(&first).unwrap()["quota_reset_at"],
        json!(timestamp)
    );
    sqlx::query("UPDATE users SET usage_bytes=7 WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    let committed = snapshot(&state).await;
    let replay = client
        .post(format!("{base}/quota/reset"))
        .bearer_auth("admin")
        .header("idempotency-key", "reset")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.bytes().await.unwrap(), first);
    assert_eq!(snapshot(&state).await, committed);
    assert_eq!(
        count(&state, "SELECT usage_bytes FROM users WHERE id='user'").await,
        7
    );
    assert_eq!(
        count(&state, "SELECT revision FROM users WHERE id='user'").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='user.quota_reset' AND actor='admin'"
        )
        .await,
        1
    );
    server.abort();
}

#[tokio::test]
async fn receipt_failure_rolls_back_edit_delete_and_reset_with_kicks_events_and_cache_versions() {
    let state = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    sqlx::raw_sql("CREATE FUNCTION reject_user_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER reject_user_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_user_receipt()")
        .execute(&state.pool).await.unwrap();
    let before = snapshot(&state).await;
    assert_eq!(
        client
            .patch(&base)
            .bearer_auth("admin")
            .header("idempotency-key", "edit")
            .json(&json!({"expected_revision":1,"enabled":false}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(snapshot(&state).await, before);
    assert_eq!(
        client
            .delete(format!("{base}?expected_revision=1"))
            .bearer_auth("admin")
            .header("idempotency-key", "delete")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(snapshot(&state).await, before);
    assert_eq!(
        client
            .post(format!("{base}/quota/reset"))
            .bearer_auth("admin")
            .header("idempotency-key", "reset")
            .json(&json!({"expected_revision":1}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(snapshot(&state).await, before);
    sqlx::query("DROP TRIGGER reject_user_receipt ON management_receipts")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        client
            .post(format!("{base}/quota/reset"))
            .bearer_auth("admin")
            .header("idempotency-key", "reset")
            .json(&json!({"expected_revision":1}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    server.abort();
}

#[tokio::test]
async fn native_conflicts_retry_each_user_mutation_with_one_business_commit() {
    for code in ["40001", "40P01"] {
        let state = fixture().await;
        let (base, server) = serve(state.clone()).await;
        let client = client();
        sqlx::raw_sql(&format!("CREATE SEQUENCE user_receipt_attempt; CREATE FUNCTION retry_user_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF mod(nextval('user_receipt_attempt'),2)=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_user_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION retry_user_receipt()"))
            .execute(&state.pool).await.unwrap();
        assert_eq!(
            client
                .patch(&base)
                .bearer_auth("admin")
                .header("idempotency-key", "edit")
                .json(&json!({"expected_revision":1,"enabled":false}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            client
                .post(format!("{base}/quota/reset"))
                .bearer_auth("admin")
                .header("idempotency-key", "reset")
                .json(&json!({"expected_revision":2}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            client
                .delete(format!("{base}?expected_revision=3"))
                .bearer_auth("admin")
                .header("idempotency-key", "delete")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(count(&state, "SELECT count(*) FROM users").await, 0);
        assert_eq!(
            count(&state, "SELECT count(*) FROM management_receipts").await,
            3
        );
        assert_eq!(count(&state, "SELECT count(*) FROM audit_records").await, 3);
        assert_eq!(
            count(&state, "SELECT last_value FROM user_receipt_attempt").await,
            6
        );
        server.abort();
    }
}

#[tokio::test]
async fn concurrent_identical_edit_runs_once_and_missing_keys_cannot_mutate() {
    let state = fixture().await;
    let (base, server) = serve(state.clone()).await;
    let client = client();
    let body = json!({"expected_revision":1,"enabled":false});
    let before = snapshot(&state).await;
    assert_eq!(
        client
            .patch(&base)
            .bearer_auth("admin")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .delete(format!("{base}?expected_revision=1"))
            .bearer_auth("admin")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .post(format!("{base}/quota/reset"))
            .bearer_auth("admin")
            .json(&json!({"expected_revision":1}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(snapshot(&state).await, before);
    let request = || {
        client
            .patch(&base)
            .bearer_auth("admin")
            .header("idempotency-key", "concurrent")
            .json(&body)
            .send()
    };
    let (a, b) = tokio::join!(request(), request());
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.status(), StatusCode::OK);
    assert_eq!(b.status(), StatusCode::OK);
    assert_eq!(a.bytes().await.unwrap(), b.bytes().await.unwrap());
    assert_eq!(
        count(&state, "SELECT revision FROM users WHERE id='user'").await,
        2
    );
    assert_eq!(count(&state, "SELECT count(*) FROM audit_records").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    server.abort();
}
