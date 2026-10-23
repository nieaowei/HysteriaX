use super::{tests::*, *};
use crate::execution;

#[tokio::test]
async fn provider_loading_uses_the_prepared_credential_and_rebinding_cannot_publish() {
    use axum::{Json, Router, routing::get};
    let f = fixture().await;
    let credential: String = sqlx::query_scalar("SELECT credential_id FROM dns_connections")
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    let payload =
        json!({"provider":"cloudflare","config":{"cloudflare_api_token":"other-fixture-token"}});
    let mut tx = f.state.pool.begin().await.unwrap();
    let other = crate::credentials::insert(
        &mut tx,
        &f.state.secrets,
        "other DNS",
        "dns",
        None,
        &payload,
        &crate::credentials::validate_payload("dns", &payload).unwrap(),
        &json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let app = Router::new().fallback(get(|| async {
        Json(json!({"success":true,"result":[],"result_info":{"total_pages":1}}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let pool = f.state.pool.clone();
    let result=execution::supervise(f.state.pool.clone(),f.leases.clone(),execute_with(&f.state.pool,&f.job,&f.operation,&f.expected,move |actual,version|async move {
        assert_eq!(actual,credential);assert_eq!(version,2);
        sqlx::query("UPDATE dns_connections SET credential_id=$1,credential_version=1,revision=revision+1").bind(&other).execute(&pool).await.unwrap();
        Cloudflare::with_base("new-fixture-token".into(),base)
    })).await.unwrap();
    assert!(result.is_err());
    server.abort();
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM dns_credential_batch_items WHERE applied_at IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(count(&f,"SELECT count(*) FROM dns_operations WHERE action='credential-apply' AND applied_at IS NOT NULL").await,0);
}
