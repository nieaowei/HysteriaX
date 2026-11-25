use std::{str::FromStr, time::Duration};

use anyhow::{Context, Result};
use sqlx::{
    PgPool, Postgres, Transaction,
    migrate::Migrator,
    postgres::{PgConnectOptions, PgPoolOptions},
};

pub(crate) mod diagnostics;

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
                sqlx::query("SELECT set_config('lock_timeout',$1,false),set_config('statement_timeout',$2,false),set_config('idle_in_transaction_session_timeout',$3,false),set_config('hysteriax.reference_maintenance',$4,false)")
                    .bind(format!("{lock_timeout_seconds}s"))
                    .bind(if maintenance {"0".to_owned()} else {format!("{statement_timeout_seconds}s")})
                    .bind(if maintenance {"0".to_owned()} else {format!("{idle_transaction_timeout_seconds}s")})
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
    fn database_error(&self) -> Option<&sqlx::Error> {
        None
    }
}
impl RetryableTransactionError for sqlx::Error {
    fn database_error(&self) -> Option<&sqlx::Error> {
        Some(self)
    }
    fn retryable_transaction(&self) -> bool {
        self.as_database_error()
            .and_then(|error| error.code())
            .is_some_and(|code| matches!(code.as_ref(), "40001" | "40P01"))
    }
}
impl RetryableTransactionError for anyhow::Error {
    fn database_error(&self) -> Option<&sqlx::Error> {
        self.chain()
            .find_map(|error| error.downcast_ref::<sqlx::Error>())
    }
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
pub async fn serializable<T, E, F>(pool: &PgPool, operation: F) -> Result<T, E>
where
    E: RetryableTransactionError,
    F: for<'connection> FnMut(
        &'connection mut Transaction<'static, Postgres>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'connection>,
    >,
{
    transaction(
        pool,
        "serializable",
        "BEGIN ISOLATION LEVEL SERIALIZABLE",
        None,
        operation,
    )
    .await
}

#[allow(dead_code)]
pub(crate) async fn serializable_after_probe<T, E, F>(
    pool: &PgPool,
    connection: sqlx::pool::PoolConnection<Postgres>,
    operation: F,
) -> Result<T, E>
where
    E: RetryableTransactionError,
    F: for<'connection> FnMut(
        &'connection mut Transaction<'static, Postgres>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'connection>,
    >,
{
    transaction(
        pool,
        "serializable",
        "BEGIN ISOLATION LEVEL SERIALIZABLE",
        Some(connection),
        operation,
    )
    .await
}

/// READ COMMITTED work protected by explicit, consistently ordered row locks.
/// Same side-effect prohibition and retry budget as `serializable`.
pub async fn write<T, E, F>(pool: &PgPool, operation: F) -> Result<T, E>
where
    E: RetryableTransactionError,
    F: for<'connection> FnMut(
        &'connection mut Transaction<'static, Postgres>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'connection>,
    >,
{
    transaction(
        pool,
        "read_committed",
        "BEGIN ISOLATION LEVEL READ COMMITTED",
        None,
        operation,
    )
    .await
}

/// Reuse the connection that performed a non-authoritative scheduling probe.
/// Only the first attempt uses it; a conflict still retries on a fresh transaction.
/// The connection must have been acquired from `pool`.
pub(crate) async fn write_after_probe<T, E, F>(
    pool: &PgPool,
    connection: sqlx::pool::PoolConnection<Postgres>,
    operation: F,
) -> Result<T, E>
where
    E: RetryableTransactionError,
    F: for<'connection> FnMut(
        &'connection mut Transaction<'static, Postgres>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'connection>,
    >,
{
    transaction(
        pool,
        "read_committed",
        "BEGIN ISOLATION LEVEL READ COMMITTED",
        Some(connection),
        operation,
    )
    .await
}

async fn transaction<T, E, F>(
    pool: &PgPool,
    isolation: &'static str,
    begin: &'static str,
    mut first_connection: Option<sqlx::pool::PoolConnection<Postgres>>,
    mut operation: F,
) -> Result<T, E>
where
    E: RetryableTransactionError,
    F: for<'connection> FnMut(
        &'connection mut Transaction<'static, Postgres>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, E>> + Send + 'connection>,
    >,
{
    let mut observation = diagnostics::Observation::new(std::any::type_name::<F>(), isolation);
    let mut last_conflict = None;
    for attempt_index in 0..=5 {
        observation.attempted();
        let (result, report) = diagnostics::attempt(async {
            let begun = match first_connection.take() {
                Some(connection) => {
                    Transaction::begin(connection, Some(sqlx::SqlSafeStr::into_sql_str(begin)))
                        .await
                }
                None => pool.begin_with(begin).await,
            };
            let mut tx = begun.map_err(|error| {
                diagnostics::record(&error);
                E::from(error)
            })?;
            diagnostics::phase("guard");
            crate::execution::guard_current(&mut tx)
                .await
                .map_err(|error| {
                    diagnostics::record(&error);
                    E::from(error)
                })?;
            let attempt = crate::execution::TransactionAttempt::after_guard(&mut tx);
            diagnostics::phase("operation");
            match attempt.operation(operation(&mut tx)).await {
                Ok(value) => {
                    diagnostics::phase("commit");
                    attempt.commit(tx).await.map(|()| value).map_err(|error| {
                        diagnostics::record(&error);
                        E::from(error)
                    })
                }
                Err(error) => {
                    if let Some(cause) = error.database_error() {
                        diagnostics::record(cause);
                    }
                    // Preserve the failing statement before rollback's query event.
                    diagnostics::phase("rollback");
                    attempt.rollback(tx).await.map_err(|error| {
                        diagnostics::record(&error);
                        E::from(error)
                    })?;
                    Err(error)
                }
            }
        })
        .await;
        match result {
            Err(error) if error.retryable_transaction() => {
                observation.conflict();
                if attempt_index == 5 {
                    observation.event(&report, attempt_index + 1, "retry_exhausted", 0);
                    observation.finish(false, true);
                    return Err(error);
                }
                let delay = rand::random_range(10..40) * (1_u64 << attempt_index);
                observation.event(&report, attempt_index + 1, "retry", delay);
                last_conflict = Some(report);
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            Ok(value) => {
                if let Some(report) = last_conflict {
                    observation.event(&report, attempt_index + 1, "succeeded_after_retry", 0);
                }
                observation.finish(true, false);
                return Ok(value);
            }
            Err(error) => {
                observation.event(&report, attempt_index + 1, "failed", 0);
                observation.finish(false, false);
                return Err(error);
            }
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
        // Lock-wait helpers match pg_stat_activity.application_name so they
        // only observe this test's sessions; relays inherit it via connect_options().
        .connect_with(
            connect_options(&database_url)
                .expect("valid TEST_DATABASE_URL")
                .application_name(&schema),
        )
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
