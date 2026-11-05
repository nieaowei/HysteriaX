use super::*;
use axum::{Json, Router, http::StatusCode, routing::get};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[tokio::test]
async fn pagination_and_redacted_errors() {
    let pages = Arc::new(AtomicUsize::new(0));
    let count = pages.clone();
    let app = Router::new().route("/zones", get(move || {
            let page = count.fetch_add(1, Ordering::SeqCst) + 1;
            async move { Json(json!({"success":true,"result":[{"id":page}],"result_info":{"total_pages":2}})) }
        })) .route("/zones/denied", get(|| async { (StatusCode::FORBIDDEN, "secret-provider-token") }))
            .route("/zones/limited", get(|| async {
                let deadline=(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc2822();
                (StatusCode::TOO_MANY_REQUESTS,[("retry-after",deadline)],"not logged")
            }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let provider = Cloudflare::with_base("secret-provider-token".into(), base).unwrap();
    assert_eq!(provider.zones().await.unwrap().len(), 2);
    let limited = provider.zone("limited").await.unwrap_err();
    assert!((3590..=3600).contains(&retry_delay(&limited).unwrap()));
    let error = provider.zone("denied").await.unwrap_err();
    assert!(!error.to_string().contains("secret-provider-token"));
    assert_eq!(retry_delay(&error), None);
    assert_eq!(
        retry_delay(
            &ProviderError::Http {
                status: 429,
                retry_after: 125
            }
            .into()
        ),
        Some(125)
    );
    assert_eq!(
        retry_delay(
            &ProviderError::Http {
                status: 429,
                retry_after: 3600
            }
            .into()
        ),
        Some(3600)
    );
    assert_eq!(
        retry_delay(
            &ProviderError::Http {
                status: 429,
                retry_after: 172800
            }
            .into()
        ),
        None
    );
    server.abort();
}
