mod api;
mod config;
mod db;
mod deployment;
mod error;
mod jobs;
mod security;
mod ssh;
mod state;
mod traffic;

use std::{env, future::IntoFuture, time::Duration};

use anyhow::{Context, Result, bail};
use sqlx::{PgConnection, PgPool};
use tracing_subscriber::EnvFilter;

use crate::{security::SecretBox, state::AppState};

#[tokio::main]
async fn main() -> Result<()> {
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
    let pool = db::pool_options()
        .connect_with(options)
        .await
        .context("connect to PostgreSQL database")?;
    let mut instance_lock = db::acquire_instance_lock(&database_url).await?;

    db::run_migrations(&pool).await?;
    initialize_admin_token(&pool).await?;
    let master_key = env::var("HYSTERIAX_MASTER_KEY")
        .context("HYSTERIAX_MASTER_KEY must be a base64 encoded 32-byte key")?;
    let secret_box = SecretBox::from_base64(&master_key)?;
    let state = AppState::new(pool, secret_box);
    let jobs = tokio::spawn(jobs::run(state.clone()));
    let traffic = tokio::spawn(traffic::run(state.clone()));
    let app = api::router(state);

    let address = env::var("HYSTERIAX_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("bind to {address}"))?;
    tracing::info!(%address, "HysteriaX management API listening");
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .into_future();
    tokio::pin!(server);
    let monitor = monitor_instance_lock(&mut instance_lock);
    tokio::pin!(monitor);
    tokio::select! {
        result = &mut server => {
            result.context("serve HTTP API")?;
        }
        result = &mut monitor => {
            jobs.abort();
            traffic.abort();
            result?;
            bail!("single-instance lock monitor stopped unexpectedly");
        }
    }
    jobs.abort();
    traffic.abort();
    Ok(())
}

async fn monitor_instance_lock(connection: &mut PgConnection) -> Result<()> {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        db::check_instance_lock(connection).await?;
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
    let mut tx = db::begin_write(pool).await?;
    sqlx::query(
        "INSERT INTO admin_tokens (id, token_hash, label, created_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(digest)
    .bind("initial administrator token")
    .bind(timestamp)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::warn!("created the initial administrator token hash");
    Ok(())
}
