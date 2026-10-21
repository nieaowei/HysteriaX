//! Explicit maintenance migration and read-only runtime compatibility checks.
use anyhow::{Context, Result, ensure};
use sqlx::PgPool;

use crate::{db::MIGRATOR, security::SecretBox};

// Bumped together with routing, OpenAPI and the macOS client at v2 cutover.
pub const MANAGEMENT_PROTOCOL: i32 = 1;

pub async fn check_schema(pool: &PgPool) -> Result<()> {
    let installed: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version,success,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .context("database is not initialized; run --migrate-only during maintenance")?;
    let expected = MIGRATOR.iter().collect::<Vec<_>>();
    ensure!(
        installed.len() == expected.len(),
        "database schema differs from this server; run --migrate-only during maintenance"
    );
    for ((version, success, checksum), migration) in installed.iter().zip(expected) {
        ensure!(
            *version == migration.version
                && *success
                && checksum.as_slice() == migration.checksum.as_ref(),
            "database migration {version} is incompatible with this server"
        );
    }
    Ok(())
}

pub async fn check_runtime(pool: &PgPool, secrets: &SecretBox) -> Result<()> {
    check_schema(pool).await?;
    let contract: Option<(i64, i32, String)> = sqlx::query_as(
        "SELECT schema_version,management_protocol,master_key_fingerprint FROM runtime_contract WHERE id",
    ).fetch_optional(pool).await?;
    let (schema, protocol, fingerprint) =
        contract.context("runtime contract missing; run --migrate-only during maintenance")?;
    ensure!(
        schema == MIGRATOR.iter().last().unwrap().version,
        "runtime schema initialization is incomplete"
    );
    ensure!(
        protocol == MANAGEMENT_PROTOCOL,
        "management protocol differs from the initialized database"
    );
    ensure!(
        fingerprint == secrets.fingerprint(),
        "HYSTERIAX_MASTER_KEY differs from the initialized database"
    );
    Ok(())
}

/// Fail before running any initialization if a different key was registered.
/// Never overwrite the saved fingerprint to make a mismatched key pass.
pub async fn check_existing_key(pool: &PgPool, secrets: &SecretBox) -> Result<()> {
    let exists: bool =
        sqlx::query_scalar("SELECT to_regclass(current_schema()||'.runtime_contract') IS NOT NULL")
            .fetch_one(pool)
            .await?;
    if exists {
        check_migration_key(pool, secrets).await?;
    }
    Ok(())
}

pub async fn check_migration_key(pool: &PgPool, secrets: &SecretBox) -> Result<()> {
    let fingerprint: Option<String> =
        sqlx::query_scalar("SELECT master_key_fingerprint FROM runtime_contract WHERE id")
            .fetch_optional(pool)
            .await?;
    if let Some(fingerprint) = fingerprint {
        ensure!(
            fingerprint == secrets.fingerprint(),
            "HYSTERIAX_MASTER_KEY differs from the initialized database"
        );
    }
    Ok(())
}

pub async fn finalize(pool: &PgPool, secrets: &SecretBox) -> Result<()> {
    check_schema(pool).await?;
    check_migration_key(pool, secrets).await?;
    sqlx::query("INSERT INTO runtime_contract(id,schema_version,management_protocol,master_key_fingerprint) VALUES(true,$1,$2,$3) ON CONFLICT(id) DO UPDATE SET schema_version=EXCLUDED.schema_version,management_protocol=EXCLUDED.management_protocol,initialized_at=clock_timestamp()")
        .bind(MIGRATOR.iter().last().unwrap().version).bind(MANAGEMENT_PROTOCOL).bind(secrets.fingerprint())
        .execute(pool).await?;
    check_runtime(pool, secrets).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn startup_requires_explicit_initialization_and_matching_key_and_protocol() {
        let pool = crate::db::test_pool().await;
        let secrets =
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
        let other = SecretBox::from_base64("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE").unwrap();
        assert!(check_runtime(&pool, &secrets).await.is_err());
        finalize(&pool, &secrets).await.unwrap();
        check_runtime(&pool, &secrets).await.unwrap();
        assert!(check_runtime(&pool, &other).await.is_err());
        assert!(finalize(&pool, &other).await.is_err());
        check_runtime(&pool, &secrets).await.unwrap();
        sqlx::query("UPDATE runtime_contract SET management_protocol=999 WHERE id")
            .execute(&pool)
            .await
            .unwrap();
        assert!(check_runtime(&pool, &secrets).await.is_err());
    }

    #[tokio::test]
    async fn startup_rejects_missing_future_and_modified_migrations_without_writing() {
        let pool = crate::db::test_pool_through_version(1, 40).await;
        assert!(check_schema(&pool).await.is_err());
        crate::db::run_migrations(&pool).await.unwrap();
        check_schema(&pool).await.unwrap();
        sqlx::query("UPDATE _sqlx_migrations SET checksum='broken'::bytea WHERE version=1")
            .execute(&pool)
            .await
            .unwrap();
        assert!(check_schema(&pool).await.is_err());
    }
}
