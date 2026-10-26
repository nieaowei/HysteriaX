mod analytics;
mod api;
mod artifact_cache;
mod background;
mod bootstrap;
mod cache;
mod cache_domains;
mod config;
mod credentials;
mod db;
mod deployment;
mod dns;
mod error;
mod events;
mod execution;
mod jobs;
mod kick_requests;
mod monitoring;
mod node_acme;
mod node_cleanup;
mod node_limits;
mod overview;
mod proxy_monitor;
mod quota_rank;
mod read_snapshot;
mod redis_cache;
pub(crate) mod runtime_config;
mod security;
mod ssh;
mod state;
mod traffic;

use std::{env, future::IntoFuture, time::Duration};

use anyhow::{Context, Result, bail};
use sqlx::PgPool;
use tracing_subscriber::EnvFilter;

use crate::{security::SecretBox, state::AppState};

#[tokio::main]
async fn main() -> Result<()> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    if arguments.as_slice() == ["--check-template-runtime"] {
        let result = api::subscriptions::templates::check_runtime()
            .map_err(|error| anyhow::anyhow!("template runtime check: {}", error.message))?;
        println!("{}", serde_json::to_string(&result)?);
        return Ok(());
    }
    let migrate_only = match arguments.as_slice() {
        [] => false,
        [arg] if arg == "--migrate-only" => true,
        _ => bail!("usage: hysteriax-server [--migrate-only | --check-template-runtime]"),
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let database_url = env::var("DATABASE_URL").context("DATABASE_URL must be configured")?;
    if !database_url.starts_with("postgres://") && !database_url.starts_with("postgresql://") {
        bail!("DATABASE_URL must use the PostgreSQL scheme");
    }
    let options = db::connect_options(&database_url)?;
    let pool = db::pool_options_for_maintenance(migrate_only)
        .connect_with(options)
        .await
        .context("connect to PostgreSQL database")?;

    let master_key = env::var("HYSTERIAX_MASTER_KEY")
        .context("HYSTERIAX_MASTER_KEY must be a base64 encoded 32-byte key")?;
    let secret_box = SecretBox::from_base64(&master_key)?;
    if migrate_only {
        bootstrap::check_existing_key(&pool, &secret_box).await?;
        db::run_migrations(&pool).await?;
        bootstrap::check_migration_key(&pool, &secret_box).await?;
        initialize_admin_token(&pool).await?;
        credentials::migration::migrate(&pool, &secret_box).await?;
        api::subscriptions::templates::initialize(&pool, &secret_box)
            .await
            .map_err(|e| anyhow::anyhow!("initialize subscription templates: {}", e.message))?;
        credentials::reference_index::initialize(&pool, &secret_box)
            .await
            .map_err(|error| anyhow::anyhow!("initialize reference index: {}", error.message))?;
        traffic::work::initialize(&pool).await?;
        traffic::access::initialize(&pool).await?;
        analytics::refresh(&pool)
            .await
            .context("initialize analytics")?;
        bootstrap::finalize(&pool, &secret_box).await?;
        tracing::info!("database and credential migration completed; workers were not started");
        return Ok(());
    }
    bootstrap::check_runtime(&pool, &secret_box).await?;
    let state = AppState::new(pool, secret_box);
    proxy_monitor::cleanup_probe_directories()?;
    let publisher = tokio::spawn(events::run_publisher(state.clone()));
    let heartbeat = tokio::spawn(execution::run_heartbeats(state.clone()));
    let mtls = tokio::spawn(credentials::maintenance::run(state.clone()));
    let analytics = tokio::spawn(analytics::run(state.clone()));
    let jobs = tokio::spawn(jobs::run(state.clone()));
    let traffic = tokio::spawn(traffic::run(state.clone()));
    let network = tokio::spawn(node_limits::collect(state.clone()));
    let probes = tokio::spawn(proxy_monitor::run(state.clone()));
    let limits = tokio::spawn(node_limits::run(state.clone()));
    let quota_rank = tokio::spawn(quota_rank::run(state.clone()));
    let shutdown = state.shutdown.clone();
    let app = api::router(state);

    let address = env::var("HYSTERIAX_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("bind to {address}"))?;
    tracing::info!(%address, "HysteriaX management API listening");
    let server = serve_with_shutdown(
        listener,
        app,
        shutdown.clone(),
        shutdown_signal(),
        Duration::from_secs(8),
    );
    let result = server.await;
    // Stop all workers before returning. Recovery is driven by expired resource
    // leases; another healthy instance continues its own claimed work.
    let workers = [
        analytics, jobs, traffic, network, limits, probes, quota_rank, mtls, heartbeat, publisher,
    ];
    for worker in &workers {
        worker.abort();
    }
    for worker in workers {
        let _ = worker.await;
    }
    result?;
    Ok(())
}

async fn serve_with_shutdown(
    listener: tokio::net::TcpListener,
    app: axum::Router,
    shutdown: tokio::sync::watch::Sender<bool>,
    signal: impl std::future::Future<Output = ()>,
    drain_timeout: Duration,
) -> Result<()> {
    let mut stopping = shutdown.subscribe();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = stopping.wait_for(|value| *value).await;
        })
        .into_future();
    tokio::pin!(server, signal);
    tokio::select! {
        result = &mut server => return result.context("serve HTTP API"),
        _ = &mut signal => { shutdown.send_replace(true); }
    }
    match tokio::time::timeout(drain_timeout, &mut server).await {
        Ok(result) => result.context("drain HTTP requests"),
        Err(_) => {
            tracing::warn!(
                "HTTP drain deadline reached; closing remaining connections on process exit"
            );
            Ok(())
        }
    }
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate()).expect("register SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
    tracing::info!("shutdown signal received; draining HTTP requests");
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received; draining HTTP requests");
}

async fn initialize_admin_token(pool: &PgPool) -> Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM admin_tokens")
        .fetch_one(pool)
        .await?;
    if count > 0 {
        return Ok(());
    }

    let token = env::var("HYSTERIAX_ADMIN_TOKEN").context(
        "no active administrator token exists; set HYSTERIAX_ADMIN_TOKEN for first startup",
    )?;
    if token.len() < 43 {
        anyhow::bail!(
            "HYSTERIAX_ADMIN_TOKEN must contain at least 43 characters (256 bits of random entropy encoded as text)"
        );
    }
    let digest = security::token_digest(&token);
    let timestamp = chrono::Utc::now();
    let id = uuid::Uuid::new_v4().to_string();
    db::serializable::<_,anyhow::Error,_>(pool,move |tx| {let (id,digest)=(id.clone(),digest.clone());Box::pin(async move {
        if sqlx::query_scalar::<_,i64>("SELECT count(*) FROM admin_tokens").fetch_one(&mut **tx).await?==0 {
            sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES($1,$2,'initial administrator token',$3)")
                .bind(id).bind(digest).bind(timestamp).execute(&mut **tx).await?;
        }
        Ok(())
    })}).await?;
    tracing::warn!("created the initial administrator token hash");
    Ok(())
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_closes_sse_and_bounds_unresponsive_requests() {
        let state = AppState::new(
            crate::db::test_pool().await,
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
        );
        sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('admin',$1,'test',now())").bind(security::token_digest("test")).execute(&state.pool).await.unwrap();
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
}
