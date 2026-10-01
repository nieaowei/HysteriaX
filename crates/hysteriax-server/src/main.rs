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

use std::{env, str::FromStr, time::Duration};

use anyhow::{Context, Result};
use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use tracing_subscriber::EnvFilter;

use crate::{security::SecretBox, state::AppState};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://data/hysteriax.db?mode=rwc".to_owned());
    if database_url.starts_with("sqlite://data/") {
        std::fs::create_dir_all("data").context("create local data directory")?;
    }
    let options = SqliteConnectOptions::from_str(&database_url)
        .with_context(|| format!("invalid DATABASE_URL: {database_url}"))?
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10))
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    let pool = SqlitePool::connect_with(options)
        .await
        .context("connect to SQLite database")?;

    let migration_backup_directory = env::var_os("HYSTERIAX_MIGRATION_BACKUP_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "backups/migrations".into());
    db::run_migrations(&pool, &migration_backup_directory).await?;

    initialize_admin_token(&pool).await?;
    let master_key = env::var("HYSTERIAX_MASTER_KEY")
        .context("HYSTERIAX_MASTER_KEY must be a base64 encoded 32-byte key")?;
    let secret_box = SecretBox::from_base64(&master_key)?;
    let state = AppState::new(pool, secret_box);
    tokio::spawn(jobs::run(state.clone()));
    tokio::spawn(traffic::run(state.clone()));
    let app = api::router(state);

    let address = env::var("HYSTERIAX_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("bind to {address}"))?;
    tracing::info!(%address, "HysteriaX management API listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve HTTP API")?;
    Ok(())
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

async fn initialize_admin_token(pool: &SqlitePool) -> Result<()> {
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
    sqlx::query("INSERT INTO admin_tokens (id, token_hash, label, created_at) VALUES (?, ?, ?, datetime('now'))")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(digest)
        .bind("initial administrator token")
        .execute(pool)
        .await?;
    tracing::warn!("created the initial administrator token hash");
    Ok(())
}
