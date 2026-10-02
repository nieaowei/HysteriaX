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

pub async fn begin_write(pool: &PgPool) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(WRITE_LOCK_KEY)
        .execute(&mut *transaction)
        .await?;
    Ok(transaction)
}

pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    MIGRATOR
        .run(pool)
        .await
        .context("run PostgreSQL migrations")
}

#[cfg(test)]
pub async fn test_pool() -> PgPool {
    test_pool_with_max_connections(1).await
}

#[cfg(test)]
pub async fn test_pool_with_max_connections(max_connections: u32) -> PgPool {
    use sqlx::postgres::PgPoolOptions;
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
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .after_connect(move |connection, _| {
            let schema = connection_schema.clone();
            Box::pin(async move {
                sqlx::query(&format!("SET search_path TO {schema}"))
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&database_url)
        .await
        .expect("connect to isolated PostgreSQL test schema");
    MIGRATOR
        .run(&pool)
        .await
        .expect("run PostgreSQL migrations in isolated test schema");
    pool
}

#[cfg(test)]
mod tests {
    use super::{
        MIGRATOR, acquire_instance_lock, begin_write, check_instance_lock, test_pool,
        test_pool_with_max_connections,
    };
    use sqlx::Connection;

    #[tokio::test]
    async fn fresh_database_has_postgresql_native_columns_and_repeatable_migrations() {
        let pool = test_pool().await;
        MIGRATOR.run(&pool).await.unwrap();
        let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version, 1);

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
        assert_eq!(json_types, 5);
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
}
