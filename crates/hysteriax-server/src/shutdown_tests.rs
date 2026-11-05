use super::*;

#[tokio::test]
async fn shutdown_closes_sse_and_bounds_unresponsive_requests() {
    let state = AppState::new(
        crate::db::test_pool().await,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    sqlx::query(
        "INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('admin',$1,'test',now())",
    )
    .bind(security::token_digest("test"))
    .execute(&state.pool)
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (signal, received) = tokio::sync::oneshot::channel();
    let app = api::router(state.clone());
    let server = tokio::spawn(serve_with_shutdown(
        listener,
        app,
        state.shutdown.clone(),
        async {
            let _ = received.await;
        },
        Duration::from_secs(2),
    ));
    let mut stream = reqwest::Client::new()
        .get(format!("{base}/api/v2/events"))
        .header("Last-Event-ID", {
            let (epoch, head): (String, i64) =
                sqlx::query_as("SELECT epoch,head FROM event_publication_stream WHERE id")
                    .fetch_one(&state.pool)
                    .await
                    .unwrap();
            format!("v2:{epoch}:{head}")
        })
        .bearer_auth("test")
        .send()
        .await
        .unwrap();
    signal.send(()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), stream.chunk())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hang", listener.local_addr().unwrap());
    let (entered, started) = tokio::sync::oneshot::channel();
    let entered = std::sync::Arc::new(tokio::sync::Mutex::new(Some(entered)));
    let app = axum::Router::new().route(
        "/hang",
        axum::routing::get(move || {
            let entered = entered.clone();
            async move {
                entered.lock().await.take().unwrap().send(()).unwrap();
                std::future::pending::<String>().await
            }
        }),
    );
    let (signal, received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(serve_with_shutdown(
        listener,
        app,
        tokio::sync::watch::channel(false).0,
        async {
            let _ = received.await;
        },
        Duration::from_millis(50),
    ));
    let request = tokio::spawn(async move { reqwest::get(url).await });
    started.await.unwrap();
    signal.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    request.abort();
}
