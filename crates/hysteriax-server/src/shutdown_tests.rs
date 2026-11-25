use super::*;

#[tokio::test]
async fn sampling_drain_has_one_deadline_and_reports_timeout_or_worker_panic() {
    let done = tokio::spawn(async {});
    let hanging = tokio::spawn(std::future::pending());
    let mut workers = [("done", done), ("hanging", hanging)];
    let result = drain_sampling_workers(&mut workers, Duration::from_millis(30)).await;
    assert!(result.unwrap_err().to_string().contains("deadline reached"));
    assert!(workers.iter().all(|(_, task)| task.is_finished()));

    let panicked = tokio::spawn(async { panic!("fixture worker panic") });
    let completed = tokio::spawn(async {});
    let mut workers = [("panicked", panicked), ("completed", completed)];
    assert!(
        drain_sampling_workers(&mut workers, Duration::from_secs(1))
            .await
            .unwrap_err()
            .to_string()
            .contains("fixture worker panic")
    );
    assert!(workers.iter().all(|(_, task)| task.is_finished()));
}

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

#[tokio::test]
async fn graceful_worker_drain_waits_for_clean_exit_and_aborts_on_deadline() {
    let completed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = completed.clone();
    let finishing = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let hanging = tokio::spawn(std::future::pending());
    let mut workers = [("finishing", finishing), ("hanging", hanging)];
    drain_graceful_workers(&mut workers, Duration::from_millis(80)).await;
    assert!(completed.load(std::sync::atomic::Ordering::SeqCst));
    assert!(workers.iter().all(|(_, task)| task.is_finished()));
}
