//! Explicit maintenance migration and read-only runtime compatibility checks.
use anyhow::{Context, Result, ensure};
use sqlx::PgPool;

use crate::{db::MIGRATOR, security::SecretBox};

// Bumped together with routing, OpenAPI and the macOS client at v2 cutover.
pub const MANAGEMENT_PROTOCOL: i32 = 2;

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
    crate::credentials::reference_index::assert_ready(pool).await?;
    crate::traffic::work::assert_ready(pool).await?;
    crate::traffic::access::assert_ready(pool).await?;
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
    crate::credentials::reference_index::assert_ready(pool).await?;
    crate::traffic::work::assert_ready(pool).await?;
    crate::traffic::access::assert_ready(pool).await?;
    let mut tx = pool.begin().await?;
    // Maintenance-only gate: wait for existing guarded transactions, and stop
    // new claims/guards until their epoch and the runtime contract are replaced.
    // EXCLUSIVE also conflicts with SELECT FOR UPDATE's ROW SHARE table lock.
    sqlx::query("LOCK TABLE resource_leases IN EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let epoch: String = sqlx::query_scalar(
        "UPDATE cache_version_epoch SET epoch=gen_random_uuid()::text WHERE id RETURNING epoch",
    )
    .fetch_one(&mut *tx)
    .await?;
    // Keep canonical row order and uncertain-operation barriers intact.
    sqlx::query("SELECT resource_key FROM resource_leases WHERE owner IS NOT NULL OR blocked ORDER BY resource_key FOR UPDATE")
        .execute(&mut *tx).await?;
    sqlx::query("UPDATE resource_leases SET epoch=$1,expires_at=CASE WHEN owner IS NOT NULL THEN clock_timestamp() ELSE expires_at END WHERE owner IS NOT NULL OR blocked")
        .bind(&epoch).execute(&mut *tx).await?;
    // Maintenance, including a restored database, starts a new resume timeline
    // while retaining all historical events and publication sequence numbers.
    sqlx::query("UPDATE event_publication_stream SET epoch=gen_random_uuid()::text WHERE id")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO runtime_contract(id,schema_version,management_protocol,master_key_fingerprint) VALUES(true,$1,$2,$3) ON CONFLICT(id) DO UPDATE SET schema_version=EXCLUDED.schema_version,management_protocol=EXCLUDED.management_protocol,initialized_at=clock_timestamp()")
        .bind(MIGRATOR.iter().last().unwrap().version).bind(MANAGEMENT_PROTOCOL).bind(secrets.fingerprint())
        .execute(&mut *tx).await?;
    tx.commit().await?;
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
        crate::credentials::reference_index::initialize(&pool, &secrets)
            .await
            .unwrap();
        crate::traffic::work::initialize(&pool).await.unwrap();
        crate::traffic::access::initialize(&pool).await.unwrap();
        finalize(&pool, &secrets).await.unwrap();
        let event_epoch: String =
            sqlx::query_scalar("SELECT epoch FROM event_publication_stream WHERE id")
                .fetch_one(&pool)
                .await
                .unwrap();
        sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('retained','sync','queued','queued',now(),now(),now())")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('retained','retained','{}',now())")
            .execute(&pool).await.unwrap();
        crate::events::publish_pending(&pool).await.unwrap();
        let epoch: String = sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap();
        check_runtime(&pool, &secrets).await.unwrap();
        let unchanged: String =
            sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            epoch, unchanged,
            "normal startup must not rotate cache epochs"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT epoch FROM event_publication_stream WHERE id")
                .fetch_one(&pool)
                .await
                .unwrap(),
            event_epoch
        );
        assert!(check_runtime(&pool, &other).await.is_err());
        assert!(finalize(&pool, &other).await.is_err());
        let unchanged: String =
            sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            epoch, unchanged,
            "rejected maintenance must not alter cache identity"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT epoch FROM event_publication_stream WHERE id")
                .fetch_one(&pool)
                .await
                .unwrap(),
            event_epoch
        );
        finalize(&pool, &secrets).await.unwrap();
        let rotated: String = sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(
            epoch, rotated,
            "explicit maintenance invalidates restored Redis state"
        );
        assert_ne!(
            sqlx::query_scalar::<_, String>("SELECT epoch FROM event_publication_stream WHERE id")
                .fetch_one(&pool)
                .await
                .unwrap(),
            event_epoch
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT head FROM event_publication_stream WHERE id")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_publications")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM job_events WHERE event_type='retained'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        check_runtime(&pool, &secrets).await.unwrap();
        sqlx::query("UPDATE runtime_contract SET management_protocol=1 WHERE id")
            .execute(&pool)
            .await
            .unwrap();
        assert!(check_runtime(&pool, &secrets).await.is_err());
    }

    #[tokio::test]
    async fn startup_rejects_missing_future_and_modified_migrations_without_writing() {
        for mutation in [
            "DELETE FROM _sqlx_migrations",
            "UPDATE _sqlx_migrations SET checksum='broken'::bytea WHERE version=1",
            "UPDATE _sqlx_migrations SET success=false WHERE version=1",
            "UPDATE _sqlx_migrations SET version=10000 WHERE version=1",
        ] {
            let pool = crate::db::test_pool().await;
            check_schema(&pool).await.unwrap();
            sqlx::query(sqlx::AssertSqlSafe(mutation))
                .execute(&pool)
                .await
                .unwrap();
            let before: serde_json::Value = sqlx::query_scalar(
                "SELECT coalesce(jsonb_agg(to_jsonb(m)), '[]') FROM _sqlx_migrations m",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert!(check_schema(&pool).await.is_err());
            let after: serde_json::Value = sqlx::query_scalar(
                "SELECT coalesce(jsonb_agg(to_jsonb(m)), '[]') FROM _sqlx_migrations m",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(before, after);
        }
    }

    #[tokio::test]
    async fn legacy_history_is_rejected_without_changing_the_database() {
        let pool = crate::db::test_pool().await;
        sqlx::raw_sql("UPDATE _sqlx_migrations SET checksum='legacy checksum'::bytea; INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(74,'legacy schema',true,'legacy checksum'::bytea,0)")
            .execute(&pool).await.unwrap();
        assert!(check_schema(&pool).await.is_err());
        assert!(crate::db::run_migrations(&pool).await.is_err());
        let history: Vec<(i64, Vec<u8>)> =
            sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            history,
            vec![
                (1, b"legacy checksum".to_vec()),
                (74, b"legacy checksum".to_vec())
            ]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_totals WHERE total=0")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }
}
