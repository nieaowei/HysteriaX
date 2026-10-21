use std::{str::FromStr, time::Duration};

use anyhow::{Context, Result, bail};
use sqlx::{
    Connection, PgConnection, PgPool, Postgres, Transaction,
    migrate::Migrator,
    postgres::{PgConnectOptions, PgPoolOptions},
};

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

const INSTANCE_LOCK_KEY: i64 = 62_177_345_902_812;
const WRITE_LOCK_KEY: i64 = 62_177_345_902_811;

pub fn connect_options(database_url: &str) -> Result<PgConnectOptions> {
    let options = PgConnectOptions::from_str(database_url)
        .context("DATABASE_URL is not a valid PostgreSQL connection URL")?;
    Ok(options)
}

pub fn pool_options() -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .after_connect(|connection, _| {
            Box::pin(async move {
                // Collectors also issue writes outside begin_write. Bound their
                // row/trigger lock waits so they cannot consume the whole pool.
                sqlx::query("SET lock_timeout = '1s'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
}

pub async fn acquire_instance_lock(database_url: &str) -> Result<PgConnection> {
    let options = connect_options(database_url)?;
    let mut connection = PgConnection::connect_with(&options)
        .await
        .context("connect to PostgreSQL to acquire the single-instance lock")?;
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(INSTANCE_LOCK_KEY)
        .fetch_one(&mut connection)
        .await
        .context("acquire the single-instance lock")?;
    if !acquired {
        bail!("another HysteriaX server instance is already using this database");
    }
    Ok(connection)
}

pub async fn check_instance_lock(connection: &mut PgConnection) -> Result<()> {
    let held: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype = 'advisory' AND pid = pg_backend_pid() AND granted AND objsubid = 1 AND classid::bigint * 4294967296 + objid::bigint = $1)",
    )
        .bind(INSTANCE_LOCK_KEY)
        .fetch_one(&mut *connection)
        .await
        .context("single-instance lock connection was lost")?;
    if !held {
        bail!("single-instance lock is no longer held");
    }
    Ok(())
}

pub async fn try_begin_write(
    pool: &PgPool,
) -> Result<Option<Transaction<'_, Postgres>>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(WRITE_LOCK_KEY)
        .fetch_one(&mut *transaction)
        .await?;
    if acquired {
        crate::execution::guard_current(&mut transaction).await?;
        Ok(Some(transaction))
    } else {
        transaction.rollback().await?;
        Ok(None)
    }
}

pub async fn begin_write(pool: &PgPool) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
    let deadline = tokio::time::Instant::now() + pool.options().get_acquire_timeout();
    loop {
        if let Some(transaction) = try_begin_write(pool).await? {
            return Ok(transaction);
        }
        // Finish BEGIN/ROLLBACK before applying our contention deadline.
        // Cancelling an in-flight BEGIN can return a server-side transaction
        // to the pool before SQLx has recorded its transaction depth.
        if tokio::time::Instant::now() >= deadline {
            return Err(sqlx::Error::PoolTimedOut);
        }
        // Waiting writers release their connections before retrying.
        tokio::time::sleep(Duration::from_millis(rand::random_range(25..75))).await;
    }
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
        self.chain()
            .filter_map(|error| error.downcast_ref::<sqlx::Error>())
            .any(RetryableTransactionError::retryable_transaction)
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
        let mut tx = pool.begin().await.map_err(E::from)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
            .execute(&mut *tx)
            .await
            .map_err(E::from)?;
        crate::execution::guard_current(&mut tx)
            .await
            .map_err(E::from)?;
        let result = match operation(&mut tx).await {
            Ok(value) => tx.commit().await.map(|()| value).map_err(E::from),
            Err(error) => {
                // Finish rollback before retrying or returning a connection.
                tx.rollback().await.map_err(E::from)?;
                Err(error)
            }
        };
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
    // Startup migrations may legitimately wait on DDL or the migration lock.
    // Apply the short lock deadline only to normal application queries.
    let mut connection = pool.acquire().await?;
    let previous: String = sqlx::query_scalar("SHOW lock_timeout")
        .fetch_one(&mut *connection)
        .await?;
    sqlx::query("SET lock_timeout='0'")
        .execute(&mut *connection)
        .await?;
    let result = migrator.run(&mut *connection).await;
    let restored = sqlx::query("SELECT set_config('lock_timeout',$1,false)")
        .bind(previous)
        .execute(&mut *connection)
        .await;
    if restored.is_err() {
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
    test_pool_through_version(max_connections, i64::MAX).await
}

#[cfg(test)]
pub async fn test_pool_through_version(max_connections: u32, through_version: i64) -> PgPool {
    use uuid::Uuid;

    let database_url = std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .expect("TEST_DATABASE_URL must point to a disposable PostgreSQL database");
    let schema = format!("hysteriax_test_{}", Uuid::new_v4().simple());
    let mut admin = PgConnection::connect(&database_url)
        .await
        .expect("connect to test PostgreSQL database");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
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
                sqlx::query(&format!("SET search_path TO {schema}"))
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("SET lock_timeout = '1s'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&database_url)
        .await
        .expect("connect to isolated PostgreSQL test schema");
    let migrator = Migrator {
        migrations: MIGRATOR
            .iter()
            .filter(|migration| migration.version <= through_version)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    migrate_with(&pool, &migrator)
        .await
        .expect("run PostgreSQL migrations in isolated test schema");
    pool
}

#[cfg(test)]
mod tests {
    use super::{
        MIGRATOR, acquire_instance_lock, begin_write, check_instance_lock, serializable, test_pool,
        test_pool_with_max_connections,
    };
    use sqlx::Connection;
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
            ApiError::from(error).status,
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
    }

    #[tokio::test]
    async fn database_allows_only_one_server_instance_lock_holder() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .expect("TEST_DATABASE_URL must be configured");
        let mut first = acquire_instance_lock(&database_url).await.unwrap();
        assert!(acquire_instance_lock(&database_url).await.is_err());
        check_instance_lock(&mut first).await.unwrap();
        first.close().await.unwrap();
        acquire_instance_lock(&database_url)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn waiting_writers_leave_connections_available_for_reads() {
        let pool = test_pool_with_max_connections(3).await;
        let holder = begin_write(&pool).await.unwrap();
        let mut writers = tokio::task::JoinSet::new();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(13));
        for _ in 0..12 {
            let pool = pool.clone();
            let barrier = barrier.clone();
            writers.spawn(async move {
                barrier.wait().await;
                begin_write(&pool).await.unwrap().commit().await.unwrap();
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
        sqlx::query("SELECT * FROM cache_generation FOR UPDATE")
            .execute(&mut *holder)
            .await
            .unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            sqlx::query("UPDATE cache_generation SET generation=generation+1").execute(&pool),
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
    async fn write_transactions_keep_sse_event_ids_in_commit_order() {
        let pool = test_pool_with_max_connections(2).await;
        let timestamp = chrono::Utc::now();
        sqlx::query("INSERT INTO jobs (id, kind, status, stage, available_at, created_at, updated_at) VALUES ('serialized-job', 'sync', 'queued', 'queued', $1, $2, $3)")
            .bind(timestamp)
            .bind(timestamp)
            .bind(timestamp)
            .execute(&pool)
            .await
            .unwrap();

        let mut first = begin_write(&pool).await.unwrap();
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
            let mut transaction = begin_write(&worker_pool).await.unwrap();
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

    #[tokio::test]
    async fn job_snapshot_migration_backfills_existing_jobs_and_preserves_node_ids() {
        let pool = test_pool().await;
        let mut tx = pool.begin().await.unwrap();
        // Recreate the version-one jobs schema to exercise an existing database upgrade.
        sqlx::raw_sql("ALTER TABLE jobs DROP COLUMN node_name; ALTER TABLE jobs ADD CONSTRAINT jobs_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE SET NULL;")
                .execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO nodes (id, name, ssh_host, ssh_port, ssh_username, ssh_auth_type, ssh_secret_enc, public_host, public_port, listen_addr, node_token_hash, node_token_enc, traffic_stats_secret_enc, desired_config_enc, created_at, updated_at) VALUES ('migration-node', 'Existing node', '127.0.0.1', 22, 'root', 'private_key', 'ssh-secret', 'node.example.test', 443, ':443', 'node-hash', 'node-token', 'stats-secret', '{}', now(), now())")
                .execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO jobs (id, kind, node_id, status, stage, available_at, created_at, updated_at) VALUES ('existing-job', 'sync', 'migration-node', 'succeeded', 'done', now(), now(), now()), ('unlinked-job', 'sync', NULL, 'succeeded', 'done', now(), now(), now())")
                .execute(&mut *tx).await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/0002_job_node_snapshot.sql"))
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("DELETE FROM nodes WHERE id = 'migration-node'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let snapshots: Vec<(String, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT id, node_id, node_name FROM jobs ORDER BY id")
                .fetch_all(&mut *tx)
                .await
                .unwrap();
        assert_eq!(
            snapshots,
            vec![
                (
                    "existing-job".into(),
                    Some("migration-node".into()),
                    Some("Existing node".into())
                ),
                ("unlinked-job".into(), None, None),
            ]
        );
        tx.rollback().await.unwrap();
    }
}
