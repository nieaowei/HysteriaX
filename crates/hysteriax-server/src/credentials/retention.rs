//! Remove expired superseded private material only after every reference is gone.
//! One leased page per pass; skipped parents/versions are revisited after wrap.
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgPool, Row};

use crate::{background::maintenance, db, state::AppState};

const PAGE: i64 = 100;
const KIND: &str = "credential-retention";
const KEY: &str = "credentials:expired-material";
const CANDIDATES: &str = "WITH expired AS (SELECT credential_id,credential_version,max(expires_at) expires_at FROM (SELECT credential_id,credential_version,expires_at FROM mtls_user_versions WHERE expires_at<$1 UNION ALL SELECT credential_id,credential_version,expires_at FROM mtls_probe_versions WHERE expires_at<$1) e GROUP BY credential_id,credential_version) SELECT e.credential_id,e.credential_version FROM expired e JOIN credentials c ON c.id=e.credential_id JOIN credential_versions cv ON cv.credential_id=e.credential_id AND cv.version=e.credential_version WHERE e.credential_version<c.latest_version AND NOT EXISTS(SELECT 1 FROM mtls_user_versions v WHERE v.credential_id=e.credential_id AND v.credential_version=e.credential_version AND v.expires_at>=$1) AND NOT EXISTS(SELECT 1 FROM mtls_probe_versions p WHERE p.credential_id=e.credential_id AND p.credential_version=e.credential_version AND p.expires_at>=$1) AND NOT EXISTS(SELECT 1 FROM mtls_activations a WHERE a.probe_credential_id=e.credential_id AND a.probe_credential_version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM mtls_active_probes p WHERE p.credential_id=e.credential_id AND p.credential_version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM credential_reference_edges r WHERE r.credential_id=e.credential_id AND r.version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM credential_batches b WHERE b.credential_id=e.credential_id AND b.version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM node_assignments a WHERE a.mtls_credential_id=e.credential_id AND a.mtls_credential_version=e.credential_version) AND NOT EXISTS(SELECT 1 FROM nodes n WHERE n.ssh_credential_id=e.credential_id AND n.ssh_credential_version=e.credential_version) AND ($3::text IS NULL OR (e.credential_id,e.credential_version)>($3,$4))";

pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {
            _=shutdown.wait_for(|stop|*stop)=>return,
            _=interval.tick()=>{}
        }
        if let Err(error) = prune(&state).await {
            tracing::warn!(%error,"credential retention deferred");
        }
    }
}

pub async fn prune(state: &AppState) -> Result<u64> {
    maintenance::ensure(
        &state.pool,
        KEY,
        KIND,
        json!({"after_id":null,"after_version":0}),
    )
    .await?;
    let Some(work) = maintenance::claim(&state.pool, KIND, &state.instance_id).await? else {
        return Ok(0);
    };
    crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
        match prune_work(&state.pool, &work).await {
            Ok(count) => Ok(count),
            Err(error) => {
                work.retry(&state.pool).await?;
                Err(error)
            }
        }
    })
    .await
    .transpose()
    .map(|count| count.unwrap_or(0))
}

async fn prune_work(pool: &PgPool, work: &maintenance::Work) -> Result<u64> {
    let work = work.clone();
    db::serializable::<_,anyhow::Error,_>(pool,move|tx| {
        let work=work.clone();
        Box::pin(async move {
            work.guard_on(tx).await?;
            super::reference_index::assert_clean_on(tx).await?;
            let deadline:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()-interval '5 minutes'").fetch_one(&mut **tx).await?;
            let after=work.payload["after_id"].as_str();
            let version=work.payload["after_version"].as_i64().unwrap_or(0);
            let candidates:Vec<(String,i64)>=sqlx::query_as(sqlx::AssertSqlSafe(format!("{CANDIDATES} ORDER BY e.credential_id,e.credential_version LIMIT $2")))
                .bind(deadline).bind(PAGE).bind(after).bind(version).fetch_all(&mut **tx).await?;
            let ids:std::collections::BTreeSet<_>=candidates.iter().map(|(id,_)|id.clone()).collect();
            let parents:Vec<String>=sqlx::query_scalar("SELECT id FROM credentials WHERE id=ANY($1::text[]) ORDER BY id FOR KEY SHARE SKIP LOCKED")
                .bind(ids.into_iter().collect::<Vec<_>>()).fetch_all(&mut **tx).await?;
            let rows=sqlx::query(sqlx::AssertSqlSafe(format!("{CANDIDATES} AND c.id=ANY($5::text[]) ORDER BY cv.credential_id,cv.version LIMIT $2 FOR UPDATE OF cv SKIP LOCKED")))
                .bind(deadline).bind(PAGE).bind(after).bind(version).bind(parents).fetch_all(&mut **tx).await?;
            let mut removed=0;
            for row in rows {
                let id:String=row.get("credential_id");
                let version:i64=row.get("credential_version");
                sqlx::query("DELETE FROM mtls_user_versions WHERE credential_id=$1 AND credential_version=$2").bind(&id).bind(version).execute(&mut **tx).await?;
                sqlx::query("DELETE FROM mtls_probe_versions WHERE credential_id=$1 AND credential_version=$2").bind(&id).bind(version).execute(&mut **tx).await?;
                removed+=sqlx::query("DELETE FROM credential_versions WHERE credential_id=$1 AND version=$2").bind(id).bind(version).execute(&mut **tx).await?.rows_affected();
            }
            let last=if candidates.len()==PAGE as usize {candidates.last()} else {None};
            sqlx::query("UPDATE background_work SET payload=$2 WHERE id=$1")
                .bind(&work.id).bind(json!({"after_id":last.map(|(id,_)|id),"after_version":last.map(|(_,v)|*v).unwrap_or(0)})).execute(&mut **tx).await?;
            work.finish_on(tx,false,if candidates.is_empty() && after.is_none(){60}else{1},None).await?;
            Ok(removed)
        })
    }).await
}

#[cfg(test)]
mod tests;
