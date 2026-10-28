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
            match operation(&mut tx).await {
                Ok(value) => tx.commit().await.map(|()| value).map_err(E::from),
                Err(error) => {
                    // Finish rollback before retrying or returning a connection.
                    tx.rollback().await.map_err(E::from)?;
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
            match operation(&mut tx).await {
                Ok(value) => tx.commit().await.map(|()| value).map_err(E::from),
                Err(error) => {
                    // Finish rollback before retrying or returning a connection.
                    tx.rollback().await.map_err(E::from)?;
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
mod tests {
    use super::{
        MIGRATOR, hold_retired_global_write_lock, serializable, test_pool,
        test_pool_with_max_connections,
    };
    use std::time::Duration;

    #[tokio::test]
    async fn serializable_retries_write_skew_from_a_fresh_snapshot() {
        let pool = test_pool_with_max_connections(4).await;
        sqlx::raw_sql("CREATE TABLE retry_counter(id int PRIMARY KEY, value bigint NOT NULL); INSERT INTO retry_counter VALUES(1,0);")
            .execute(&pool).await.unwrap();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..2 {
            let pool = pool.clone();
            let barrier = barrier.clone();
            tasks.spawn(async move {
                let mut first = true;
                serializable::<_, sqlx::Error, _>(&pool, move |connection| {
                    let wait = first;
                    first = false;
                    let barrier = barrier.clone();
                    Box::pin(async move {
                        let old: i64 =
                            sqlx::query_scalar("SELECT value FROM retry_counter WHERE id=1")
                                .fetch_one(&mut **connection)
                                .await?;
                        if wait {
                            barrier.wait().await;
                        }
                        sqlx::query("UPDATE retry_counter SET value=$1 WHERE id=1")
                            .bind(old + 1)
                            .execute(&mut **connection)
                            .await?;
                        Ok(())
                    })
                })
                .await
                .unwrap();
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT value FROM retry_counter WHERE id=1")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn serializable_bounds_retries_and_preserves_business_conflicts() {
        use crate::error::ApiError;
        let pool = test_pool().await;
        let mut attempts = 0;
        let error = serializable::<(), sqlx::Error, _>(&pool, |connection| {
            attempts += 1;
            Box::pin(async move {
                sqlx::query("DO $$ BEGIN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END $$;")
                    .execute(&mut **connection)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap_err();
        assert_eq!(attempts, 6);
        assert_eq!(
            ApiError::from(anyhow::Error::from(error)).status,
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        let mut attempts = 0;
        let error = serializable::<(), ApiError, _>(&pool, |_| {
            attempts += 1;
            Box::pin(async { Err(ApiError::conflict("revision changed")) })
        })
        .await
        .unwrap_err();
        assert_eq!(attempts, 1);
        assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn fresh_database_has_postgresql_native_columns_and_repeatable_migrations() {
        let pool = test_pool().await;
        super::run_migrations(&pool).await.unwrap();
        let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version, MIGRATOR.iter().last().unwrap().version);

        let types: Vec<(String, String)> = sqlx::query_as(
            "SELECT column_name, data_type FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'users' AND column_name IN ('enabled', 'expires_at', 'quota_reset_at', 'usage_bytes') ORDER BY column_name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            types,
            vec![
                ("enabled".to_owned(), "boolean".to_owned()),
                (
                    "expires_at".to_owned(),
                    "timestamp with time zone".to_owned()
                ),
                (
                    "quota_reset_at".to_owned(),
                    "timestamp with time zone".to_owned()
                ),
                ("usage_bytes".to_owned(), "bigint".to_owned()),
            ]
        );

        let non_native_timestamps: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = current_schema() AND column_name LIKE '%\\_at' ESCAPE '\\' AND data_type != 'timestamp with time zone'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(non_native_timestamps, 0);

        let json_types: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = current_schema() AND column_name IN ('payload_json', 'result_json', 'logs_json', 'detail_json') AND data_type = 'jsonb'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(json_types, 6);

        let buckets: Vec<(String, i64, i16, i16)> = sqlx::query_as(
            "SELECT domain,count(*),min(bucket),max(bucket) FROM cache_bucket_versions GROUP BY domain ORDER BY domain",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(buckets.len(), 17);
        assert!(
            buckets
                .iter()
                .all(|(_, count, min, max)| (*count, *min, *max) == (64, 0, 63))
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT total FROM audit_totals WHERE id")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT head FROM event_publication_stream WHERE id")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        assert!(sqlx::query_scalar::<_, bool>(
            "SELECT to_regclass('traffic_history_cleanup_runs') IS NULL AND to_regclass('traffic_history_cleanup_nodes') IS NULL AND to_regclass('cache_generation') IS NULL AND to_regclass('cache_domain_versions') IS NULL",
        ).fetch_one(&pool).await.unwrap());
        let epoch: String = sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let other = test_pool().await;
        let other_epoch: String =
            sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
                .fetch_one(&other)
                .await
                .unwrap();
        assert_ne!(
            epoch, other_epoch,
            "independent installations must not share cache epochs"
        );
    }

    #[tokio::test]
    async fn legacy_fixture_waiters_leave_connections_available_for_reads() {
        let pool = test_pool_with_max_connections(3).await;
        let holder = hold_retired_global_write_lock(&pool).await.unwrap();
        let mut writers = tokio::task::JoinSet::new();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(13));
        for _ in 0..12 {
            let pool = pool.clone();
            let barrier = barrier.clone();
            writers.spawn(async move {
                barrier.wait().await;
                hold_retired_global_write_lock(&pool)
                    .await
                    .unwrap()
                    .commit()
                    .await
                    .unwrap();
            });
        }
        barrier.wait().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        for _ in 0..10 {
            let value = tokio::time::timeout(
                Duration::from_millis(500),
                sqlx::query_scalar::<_, i64>("SELECT 1::bigint").fetch_one(&pool),
            )
            .await
            .expect("waiting writers must not starve readiness queries")
            .unwrap();
            assert_eq!(value, 1);
        }
        assert_eq!(writers.len(), 12);
        holder.commit().await.unwrap();
        while let Some(result) = writers.join_next().await {
            result.unwrap();
        }
    }

    #[tokio::test]
    async fn direct_writes_have_bounded_lock_waits() {
        let pool = test_pool_with_max_connections(2).await;
        let mut holder = pool.begin().await.unwrap();
        sqlx::query(
            "SELECT * FROM cache_bucket_versions WHERE domain='nodes' AND bucket=0 FOR UPDATE",
        )
        .execute(&mut *holder)
        .await
        .unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            sqlx::query("UPDATE cache_bucket_versions SET generation=generation+1 WHERE domain='nodes' AND bucket=0").execute(&pool),
        )
        .await
        .expect("direct writers must release their connection after a bounded wait")
        .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("55P03")
        );
        holder.rollback().await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT 1::bigint")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn legacy_fixture_serializes_its_own_events() {
        let pool = test_pool_with_max_connections(2).await;
        let timestamp = chrono::Utc::now();
        sqlx::query("INSERT INTO jobs (id, kind, status, stage, available_at, created_at, updated_at) VALUES ('serialized-job', 'sync', 'queued', 'queued', $1, $2, $3)")
            .bind(timestamp)
            .bind(timestamp)
            .bind(timestamp)
            .execute(&pool)
            .await
            .unwrap();

        let mut first = hold_retired_global_write_lock(&pool).await.unwrap();
        let first_id: i64 = sqlx::query_scalar("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ('serialized-job', 'first', $1, $2) RETURNING id")
            .bind(serde_json::json!({}))
            .bind(timestamp)
            .fetch_one(&mut *first)
            .await
            .unwrap();

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let worker_pool = pool.clone();
        let second = tokio::spawn(async move {
            let _ = started_tx.send(());
            let mut transaction = hold_retired_global_write_lock(&worker_pool).await.unwrap();
            let id: i64 = sqlx::query_scalar("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ('serialized-job', 'second', $1, $2) RETURNING id")
                .bind(serde_json::json!({}))
                .bind(timestamp)
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
            transaction.commit().await.unwrap();
            id
        });
        let _ = started_rx.await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!second.is_finished());

        first.commit().await.unwrap();
        let second_id = second.await.unwrap();
        assert!(second_id > first_id);
    }
}

#[cfg(test)]
#[path = "db/runtime_timeout_tests.rs"]
mod runtime_timeout_tests;

#[cfg(test)]
#[path = "db/cutover_tests.rs"]
mod cutover_tests;

#[cfg(test)]
#[path = "db/transaction_cancellation_tests.rs"]
mod transaction_cancellation_tests;
