use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use sqlx::{
    Connection, SqliteConnection, SqlitePool, migrate::Migrator, sqlite::SqliteConnectOptions,
};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

pub async fn run_migrations(pool: &SqlitePool, backup_directory: &Path) -> Result<()> {
    let migration_table_exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations')",
    )
    .fetch_one(pool)
    .await?;
    let current_version: i64 = if migration_table_exists == 0 {
        0
    } else {
        sqlx::query_scalar(
            "SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations WHERE success = 1",
        )
        .fetch_one(pool)
        .await?
    };
    let has_application_schema: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations')",
    )
    .fetch_one(pool)
    .await?;
    let pending_versions = MIGRATOR
        .iter()
        .map(|migration| migration.version)
        .filter(|version| *version > current_version)
        .collect::<Vec<_>>();

    if has_application_schema != 0
        && let (Some(first_pending), Some(last_pending)) =
            (pending_versions.first(), pending_versions.last())
        && let Some(database_file) = database_file(pool).await?
        && !database_file.is_empty()
    {
        let metadata = tokio::fs::metadata(&database_file)
            .await
            .with_context(|| format!("read SQLite database metadata for {database_file}"))?;
        if metadata.len() > 0 {
            create_migration_backup(
                pool,
                backup_directory,
                current_version,
                *first_pending,
                *last_pending,
            )
            .await?;
        }
    }

    MIGRATOR.run(pool).await?;
    Ok(())
}

async fn database_file(pool: &SqlitePool) -> Result<Option<String>> {
    sqlx::query_scalar("SELECT file FROM pragma_database_list WHERE name = 'main'")
        .fetch_optional(pool)
        .await
        .context("locate SQLite database for pre-migration backup")
}

async fn create_migration_backup(
    pool: &SqlitePool,
    backup_directory: &Path,
    current_version: i64,
    first_pending: i64,
    last_pending: i64,
) -> Result<()> {
    tokio::fs::create_dir_all(backup_directory)
        .await
        .with_context(|| {
            format!(
                "create pre-migration backup directory {}",
                backup_directory.display()
            )
        })?;
    #[cfg(unix)]
    tokio::fs::set_permissions(backup_directory, std::fs::Permissions::from_mode(0o700))
        .await
        .with_context(|| {
            format!(
                "restrict pre-migration backup directory {}",
                backup_directory.display()
            )
        })?;

    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.6fZ");
    let backup_path = backup_directory.join(format!(
        "hysteriax-v{current_version}-to-v{last_pending}-before-{timestamp}-{}.sqlite3",
        std::process::id()
    ));
    let backup_path_string = backup_path
        .to_str()
        .context("pre-migration backup path is not valid UTF-8")?;
    sqlx::query("VACUUM INTO ?")
        .bind(backup_path_string)
        .execute(pool)
        .await
        .with_context(|| {
            format!(
                "create pre-migration SQLite snapshot at {}",
                backup_path.display()
            )
        })?;
    #[cfg(unix)]
    tokio::fs::set_permissions(&backup_path, std::fs::Permissions::from_mode(0o600))
        .await
        .with_context(|| {
            format!(
                "restrict pre-migration SQLite snapshot {}",
                backup_path.display()
            )
        })?;

    let options = SqliteConnectOptions::new()
        .filename(&backup_path)
        .read_only(true)
        .create_if_missing(false);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .with_context(|| {
            format!(
                "open pre-migration SQLite snapshot {} for verification",
                backup_path.display()
            )
        })?;
    let integrity: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&mut connection)
        .await
        .context("verify pre-migration SQLite snapshot")?;
    connection.close().await?;
    if integrity != "ok" {
        bail!(
            "pre-migration SQLite snapshot {} failed integrity check: {integrity}",
            backup_path.display()
        );
    }
    tracing::info!(
        path = %backup_path.display(),
        current_version,
        first_pending,
        last_pending,
        "created verified SQLite snapshot before schema migration"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::run_migrations;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    #[tokio::test]
    async fn first_start_runs_migrations_without_creating_an_empty_snapshot() {
        let root = std::env::temp_dir().join(format!(
            "hysteriax-first-migration-{}",
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let database = root.join("service.db");
        let backup_directory = root.join("backups");
        let options = SqliteConnectOptions::new()
            .filename(&database)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();

        run_migrations(&pool, &backup_directory).await.unwrap();

        let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(version, 4);
        assert!(!backup_directory.exists());
        pool.close().await;
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
