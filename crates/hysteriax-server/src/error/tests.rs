use super::*;
use crate::db::RetryableTransactionError;

#[test]
fn pool_pressure_is_not_a_serialization_retry_for_a_database_closure() {
    for cause in [
        anyhow::Error::new(sqlx::Error::PoolTimedOut),
        anyhow::Error::new(ApiError::from(sqlx::Error::PoolTimedOut)),
    ] {
        let error = ApiError::from(cause.context("load current state"));
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code, "database_pool_busy");
        assert!(
            !error.retryable_transaction(),
            "only native transaction conflicts may rerun a closure"
        );
    }
}

#[tokio::test]
async fn actual_http_pool_pressure_returns_retryable_status_without_consuming_write_identity() {
    let pool = crate::db::test_pool_with_max_connections(1).await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('pool-admin',$1,'fixture',clock_timestamp())").bind(crate::security::token_digest("pool-admin")).execute(&pool).await.unwrap();
    let state = crate::state::AppState::new(
        pool.clone(),
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/v2/users", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, crate::api::router(state))
            .await
            .unwrap()
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let held = pool.acquire().await.unwrap();
    let busy = client
        .post(&url)
        .bearer_auth("pool-admin")
        .header("idempotency-key", "same-write")
        .json(&serde_json::json!({"name":"pool recovery"}))
        .send()
        .await
        .unwrap();
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        busy.json::<serde_json::Value>().await.unwrap()["error"]["code"],
        "database_pool_busy"
    );
    drop(held);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM management_receipts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let committed = client
        .post(&url)
        .bearer_auth("pool-admin")
        .header("idempotency-key", "same-write")
        .json(&serde_json::json!({"name":"pool recovery"}))
        .send()
        .await
        .unwrap();
    assert_eq!(committed.status(), StatusCode::CREATED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM management_receipts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    server.abort();
}
