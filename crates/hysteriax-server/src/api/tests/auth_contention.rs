use super::*;
use crate::security::SecretBox;

#[tokio::test]
async fn authenticated_reads_remain_available_while_writer_lock_is_held() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    for (id, token, revoked) in [
        ("active", "active-token", false),
        ("revoked", "revoked-token", true),
    ] {
        sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,revoked_at) VALUES($1,$2,$1,now(),CASE WHEN $3 THEN now() ELSE NULL END)")
            .bind(id).bind(token_digest(token)).bind(revoked).execute(&pool).await.unwrap();
    }
    let state = AppState::new(
        pool.clone(),
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    sqlx::query("INSERT INTO runtime_contract(id,schema_version,management_protocol,master_key_fingerprint) VALUES(true,$1,$2,$3)")
        .bind(db::MIGRATOR.iter().last().unwrap().version).bind(crate::bootstrap::MANAGEMENT_PROTOCOL).bind(state.secrets.fingerprint()).execute(&pool).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap();
    let holder = db::hold_retired_global_write_lock(&pool).await.unwrap();
    for path in ["/readyz", "/api/v2/version", "/api/v2/nodes?page_size=1"] {
        let response = client
            .get(format!("{base}{path}"))
            .bearer_auth("active-token")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
    }
    let denied = client
        .get(format!("{base}/api/v2/version"))
        .bearer_auth("revoked-token")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    holder.rollback().await.unwrap();
    let independent_touch: bool =
        sqlx::query_scalar("SELECT last_used_at IS NOT NULL FROM admin_tokens WHERE id='active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(independent_touch);
    assert_eq!(
        client
            .get(format!("{base}/api/v2/version"))
            .bearer_auth("active-token")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let touched: bool =
        sqlx::query_scalar("SELECT last_used_at IS NOT NULL FROM admin_tokens WHERE id='active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(touched);
    server.abort();
}
