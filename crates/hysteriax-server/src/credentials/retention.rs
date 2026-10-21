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
        let count=db::serializable::<_,anyhow::Error,_>(&state.pool,move |tx|Box::pin(async move {
            super::reference_index::assert_clean_on(tx).await?;
            let query="WITH expired AS (SELECT credential_id,credential_version,max(expires_at) expires_at FROM (SELECT credential_id,credential_version,expires_at FROM mtls_user_versions WHERE expires_at<$1 UNION ALL SELECT credential_id,credential_version,expires_at FROM mtls_probe_versions WHERE expires_at<$1) e GROUP BY credential_id,credential_version) SELECT e.credential_id,e.credential_version FROM expired e JOIN credentials c ON c.id=e.credential_id JOIN credential_versions cv ON cv.credential_id=e.credential_id AND cv.version=e.credential_version WHERE e.credential_version<c.latest_version AND NOT EXISTS(SELECT 1 FROM mtls_user_versions v WHERE v.credential_id=e.credential_id AND v.credential_version=e.credential_version AND v.expires_at>=$1) AND NOT EXISTS(SELECT 1 FROM mtls_probe_versions p WHERE p.credential_id=e.credential_id AND p.credential_version=e.credential_version AND p.expires_at>=$1) AND NOT EXISTS(SELECT 1 FROM mtls_activations a WHERE a.probe_credential_id=e.credential_id AND a.probe_credential_version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM mtls_active_probes p WHERE p.credential_id=e.credential_id AND p.credential_version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM credential_reference_edges r WHERE r.credential_id=e.credential_id AND r.version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM credential_batches b WHERE b.credential_id=e.credential_id AND b.version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM node_assignments a WHERE a.mtls_credential_id=e.credential_id AND a.mtls_credential_version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM nodes n WHERE n.ssh_credential_id=e.credential_id AND n.ssh_credential_version=e.credential_version)";
            let candidates:Vec<(String,i64)>=sqlx::query_as(&format!("{query} ORDER BY e.expires_at,e.credential_id,e.credential_version LIMIT $2"))
                .bind(deadline).bind(PAGE).fetch_all(&mut **tx).await?;
            if candidates.is_empty() {return Ok(0);}
            let ids:std::collections::BTreeSet<_>=candidates.iter().map(|(id,_)|id.clone()).collect();
            let ids:Vec<_>=ids.into_iter().collect();
            let parents:Vec<String>=sqlx::query_scalar("SELECT id FROM credentials WHERE id=ANY($1::text[]) ORDER BY id FOR KEY SHARE SKIP LOCKED")
                .bind(ids).fetch_all(&mut **tx).await?;
            let rows=sqlx::query(&format!("{query} AND c.id=ANY($3::text[]) ORDER BY cv.credential_id,cv.version LIMIT $2 FOR UPDATE OF cv SKIP LOCKED"))
                .bind(deadline).bind(PAGE).bind(parents).fetch_all(&mut **tx).await?;
            let mut count=0;
            for row in rows {
                let id:String=row.get("credential_id");
                let version:i64=row.get("credential_version");
                sqlx::query("DELETE FROM mtls_user_versions WHERE credential_id=$1 AND credential_version=$2").bind(&id).bind(version).execute(&mut **tx).await?;
                sqlx::query("DELETE FROM mtls_probe_versions WHERE credential_id=$1 AND credential_version=$2").bind(&id).bind(version).execute(&mut **tx).await?;
                count+=sqlx::query("DELETE FROM credential_versions WHERE credential_id=$1 AND version=$2").bind(id).bind(version).execute(&mut **tx).await?.rows_affected();
            }
            Ok(count)
        })).await?;
        removed += count;
        if count == 0 {
            return Ok(removed);
        }
    }
}
