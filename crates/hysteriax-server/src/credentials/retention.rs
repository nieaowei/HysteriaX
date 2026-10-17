//! Remove expired superseded private material only after every reference is gone.
//! The latest version and all history/task pins remain available for inspection.
use anyhow::Result;
use chrono::{Duration, Utc};
use sqlx::Row;

use crate::{db, state::AppState};

const PAGE: i64 = 100;

pub async fn prune(state: &AppState) -> Result<u64> {
    let deadline = Utc::now() - Duration::minutes(5);
    let mut removed = 0;
    loop {
        let mut tx = db::begin_write(&state.pool).await?;
        // Reference edges are derived from encrypted config snapshots. Flush
        // dirty sources under this same write lock before making deletion decisions.
        super::reference_index::ensure_on(&mut tx, &state.secrets)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "refresh certificate retention references: {}",
                    error.message
                )
            })?;
        let rows = sqlx::query("WITH expired AS (SELECT credential_id,credential_version,expires_at FROM mtls_user_versions WHERE expires_at<$1 UNION ALL SELECT credential_id,credential_version,expires_at FROM mtls_probe_versions WHERE expires_at<$1) SELECT e.credential_id,e.credential_version FROM expired e JOIN credentials c ON c.id=e.credential_id WHERE e.credential_version<c.latest_version AND NOT EXISTS(SELECT 1 FROM mtls_user_versions v WHERE v.credential_id=e.credential_id AND v.credential_version=e.credential_version AND v.expires_at>=$1) AND NOT EXISTS(SELECT 1 FROM mtls_probe_versions p WHERE p.credential_id=e.credential_id AND p.credential_version=e.credential_version AND p.expires_at>=$1) AND NOT EXISTS(SELECT 1 FROM mtls_activations a WHERE a.probe_credential_id=e.credential_id AND a.probe_credential_version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM mtls_active_probes p WHERE p.credential_id=e.credential_id AND p.credential_version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM credential_reference_edges r WHERE r.credential_id=e.credential_id AND r.version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM credential_batches b WHERE b.credential_id=e.credential_id AND b.version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM node_assignments a WHERE a.mtls_credential_id=e.credential_id AND a.mtls_credential_version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM nodes n WHERE n.ssh_credential_id=e.credential_id AND n.ssh_credential_version=e.credential_version) ORDER BY e.expires_at,e.credential_id,e.credential_version LIMIT $2")
            .bind(deadline).bind(PAGE).fetch_all(&mut *tx).await?;
        if rows.is_empty() {
            tx.commit().await?;
            return Ok(removed);
        }
        for row in rows {
            let id: String = row.get("credential_id");
            let version: i64 = row.get("credential_version");
            sqlx::query(
                "DELETE FROM mtls_user_versions WHERE credential_id=$1 AND credential_version=$2",
            )
            .bind(&id)
            .bind(version)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "DELETE FROM mtls_probe_versions WHERE credential_id=$1 AND credential_version=$2",
            )
            .bind(&id)
            .bind(version)
            .execute(&mut *tx)
            .await?;
            removed += sqlx::query(
                "DELETE FROM credential_versions WHERE credential_id=$1 AND version=$2",
            )
            .bind(&id)
            .bind(version)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }
        tx.commit().await?;
    }
}
