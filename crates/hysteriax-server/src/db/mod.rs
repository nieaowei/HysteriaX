use std::{str::FromStr, time::Duration};

use anyhow::{Context, Result};
use sqlx::{
    PgPool, Postgres, Transaction,
    migrate::Migrator,
    postgres::{PgConnectOptions, PgPoolOptions},
};

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[cfg(test)]
use sqlx::{Connection, PgConnection};
#[cfg(test)]
mod legacy_lock_fixture;
#[cfg(test)]
pub use legacy_lock_fixture::hold_retired_global_write_lock;

// Runtime configuration validates that a stopped client's statement and idle
// transaction limits together fit inside a fresh execution lease.

pub fn connect_options(database_url: &str) -> Result<PgConnectOptions> {
    let options = PgConnectOptions::from_str(database_url)
        .context("DATABASE_URL is not a valid PostgreSQL connection URL")?;
    Ok(options)
}

#[cfg(test)]
pub fn pool_options() -> PgPoolOptions {
    pool_options_for_maintenance(false)
}
pub fn pool_options_for_maintenance(maintenance: bool) -> PgPoolOptions {
    let lock_timeout_seconds = crate::runtime_config::db_lock_timeout_seconds();
    let statement_timeout_seconds = crate::runtime_config::db_statement_timeout_seconds();
    let idle_transaction_timeout_seconds =
        crate::runtime_config::db_idle_transaction_timeout_seconds();
    let execution_lease_seconds = crate::runtime_config::execution_lease_seconds();
    tracing::info!(
        lock_timeout_seconds,
        statement_timeout_seconds,
        idle_transaction_timeout_seconds,
        execution_lease_seconds,
        "database timeout budgets configured"
    );
    PgPoolOptions::new()
        .max_connections(crate::runtime_config::db_pool_max_connections() as u32)
        .acquire_timeout(Duration::from_secs(
            crate::runtime_config::db_pool_acquire_timeout_seconds(),
        ))
        .after_release(|connection, _| {
            Box::pin(async move {
                // A cancelled SQLx BEGIN can leave an open server transaction
                // even when its client-side transaction depth is zero. Reset
                // unconditionally; a protocol ping alone cannot detect it.
                // Suppress only the expected idle-ROLLBACK notice during reset.
                // SET fails in an aborted transaction: reject that connection
                // so SQLx closes it rather than publishing it back to the pool.
                let reset = sqlx::raw_sql(
                    "SET client_min_messages='error'; ROLLBACK; RESET client_min_messages",
                ).execute(connection);
                match tokio::time::timeout(Duration::from_secs(5), reset).await {
                    Ok(result) => result.map(|_| true),
                    Err(_) => Err(sqlx::Error::Protocol("pool transaction reset timed out".into())),
                }
            })
        })
        .after_connect(move |connection, _| {
            Box::pin(async move {
                // Bound resource waits and make runtime source checks explicit.
                sqlx::query("SELECT set_config('lock_timeout',$1,false)")
                    .bind(format!("{lock_timeout_seconds}s"))
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("SELECT set_config('statement_timeout',$1,false),set_config('idle_in_transaction_session_timeout',$2,false)")
                    .bind(if maintenance {"0".to_owned()} else {format!("{statement_timeout_seconds}s")})
                    .bind(if maintenance {"0".to_owned()} else {format!("{idle_transaction_timeout_seconds}s")})
                    .execute(&mut *connection).await?;
                sqlx::query("SELECT set_config('hysteriax.reference_maintenance',$1,false)")
                    .bind(if maintenance { "on" } else { "off" })
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
}

/// Error metadata used by the pure-database retry boundary. Business errors
/// (including revision conflicts) must return false.
pub trait RetryableTransactionError: From<sqlx::Error> {
    fn retryable_transaction(&self) -> bool;
}
impl RetryableTransactionError for sqlx::Error {
    fn retryable_transaction(&self) -> bool {
        self.as_database_error()
            .and_then(|error| error.code())
            .is_some_and(|code| matches!(code.as_ref(), "40001" | "40P01"))
    }
}
impl RetryableTransactionError for anyhow::Error {
    fn retryable_transaction(&self) -> bool {
        self.chain().any(|error| {
            error
                .downcast_ref::<sqlx::Error>()
                .is_some_and(RetryableTransactionError::retryable_transaction)
                || error
                    .downcast_ref::<crate::error::ApiError>()
                    .is_some_and(RetryableTransactionError::retryable_transaction)
        })
    }
}

/// Retry ONLY database work. Never put SSH, DNS, HTTP, file writes or other
/// external side effects in this closure. Each attempt uses a fresh snapshot.
/// A transaction may be attempted once plus at most five conflict retries.
pub async fn serializable<T, E, F>(pool: &PgPool, mut operation: F) -> Result<T, E>
where
    E: RetryableTransactionError,
    F: for<'connection> FnMut(
        &'connection mut Transaction<'static, Postgres>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'connection>,
    >,
{
    for attempt in 0..=5 {
        let result = async {
            let mut tx = pool
                .begin_with("BEGIN ISOLATION LEVEL SERIALIZABLE")
                .await
                .map_err(E::from)?;
            crate::execution::guard_current(&mut tx)
                .await
                .map_err(E::from)?;
            let attempt = crate::execution::TransactionAttempt::new(&mut tx);
            match attempt.operation(operation(&mut tx)).await {
                Ok(value) => attempt.commit(tx).await.map(|()| value).map_err(E::from),
                Err(error) => {
                    // Finish rollback before retrying or returning a connection.
                    attempt.rollback(tx).await.map_err(E::from)?;
                    Err(error)
                }
            }
        }
        .await;
        match result {
            Err(error) if error.retryable_transaction() && attempt < 5 => {
                let delay = rand::random_range(10..40) * (1_u64 << attempt);
                tracing::debug!(
                    attempt = attempt + 1,
                    delay_ms = delay,
                    "retrying database transaction"
                );
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            result => return result,
        }
    }
    unreachable!("bounded transaction attempts always return")
}

/// READ COMMITTED database work protected by explicit resource/row locks.
/// The closure has the same side-effect prohibition and retry budget as
/// `serializable`; callers must acquire resource rows in a fixed order.
pub async fn write<T, E, F>(pool: &PgPool, mut operation: F) -> Result<T, E>
where
    E: RetryableTransactionError,
    F: for<'connection> FnMut(
        &'connection mut Transaction<'static, Postgres>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'connection>,
    >,
{
    for attempt in 0..=5 {
        let result = async {
            let mut tx = pool
                .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED")
                .await
                .map_err(E::from)?;
            crate::execution::guard_current(&mut tx)
                .await
                .map_err(E::from)?;
            let attempt = crate::execution::TransactionAttempt::new(&mut tx);
            match attempt.operation(operation(&mut tx)).await {
                Ok(value) => attempt.commit(tx).await.map(|()| value).map_err(E::from),
                Err(error) => {
                    // Finish rollback before retrying or returning a connection.
                    attempt.rollback(tx).await.map_err(E::from)?;
                    Err(error)
                }
            }
        }
        .await;
        match result {
            Err(error) if error.retryable_transaction() && attempt < 5 => {
                let delay = rand::random_range(10..40) * (1_u64 << attempt);
                tracing::debug!(
                    attempt = attempt + 1,
                    delay_ms = delay,
                    "retrying database transaction"
                );
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            result => return result,
        }
    }
    unreachable!("bounded transaction attempts always return")
}

pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    migrate_with(pool, &MIGRATOR).await
}

async fn migrate_with(pool: &PgPool, migrator: &Migrator) -> Result<()> {
    // Maintenance migrations can legitimately exceed runtime statement/idle
    // deadlines. Restore all settings even on failure; never leak an unlimited
    // connection back into the runtime pool.
    let mut connection = pool.acquire().await?;
    let previous: (String,String,String) = sqlx::query_as("SELECT current_setting('lock_timeout'),current_setting('statement_timeout'),current_setting('idle_in_transaction_session_timeout')")
        .fetch_one(&mut *connection).await?;
    sqlx::query("SELECT set_config('lock_timeout','0',false),set_config('statement_timeout','0',false),set_config('idle_in_transaction_session_timeout','0',false)")
        .execute(&mut *connection).await?;
    let result = migrator.run(&mut *connection).await;
    let restored = sqlx::query("SELECT set_config('lock_timeout',$1,false),set_config('statement_timeout',$2,false),set_config('idle_in_transaction_session_timeout',$3,false)")
        .bind(previous.0).bind(previous.1).bind(previous.2).execute(&mut *connection).await;
    // SQLx returns early on a failed migration without releasing its session
    // advisory lock. Never return that locked connection to the pool: a later
    // attempt may acquire another connection and wait on its own idle session.
    if result.is_err() || restored.is_err() {
        connection.close().await?;
    }
    result.context("run PostgreSQL migrations")?;
    restored?;
    Ok(())
}

#[cfg(test)]
pub async fn test_pool() -> PgPool {
    test_pool_with_max_connections(1).await
}

#[cfg(test)]
pub async fn test_pool_with_max_connections(max_connections: u32) -> PgPool {
    use uuid::Uuid;

    let database_url = std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .expect("TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let schema = format!("hysteriax_test_{}", Uuid::new_v4().simple());
    let mut admin = PgConnection::connect(&database_url)
        .await
        .expect("connect to test PostgreSQL database");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&mut admin)
        .await
        .expect("create isolated PostgreSQL test schema");
    admin.close().await.expect("close schema setup connection");

    let connection_schema = schema.clone();
    let pool = pool_options()
        .max_connections(max_connections)
        .after_connect(move |connection, _| {
            let schema = connection_schema.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}")))
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("SET lock_timeout = '1s'")
                    .execute(&mut *connection)
                    .await?;
                // Unit fixtures intentionally import legacy encrypted snapshots.
                // Production-mode reference tests reconnect without this flag.
                sqlx::query("SET hysteriax.reference_maintenance='on'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&database_url)
        .await
        .expect("connect to isolated PostgreSQL test schema");
    run_migrations(&pool)
        .await
        .expect("run PostgreSQL migrations in isolated test schema");
    pool
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod runtime_timeout_tests;

#[cfg(test)]
mod cutover_tests;

#[cfg(test)]
mod transaction_cancellation_tests;

#[cfg(test)]
pub mod test_relay;
