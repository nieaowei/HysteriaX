//! Materialize superseded staged-leaf flags without locking the user graph.
//! Authoritative visibility changes atomically with generation retirement.
use super::*;
use crate::background::maintenance::{self, Work};

const KIND: &str = "mtls-superseded-identity-flags";

pub(crate) async fn enqueue_on(
    tx: &mut Transaction<'_, Postgres>,
    rotation: &str,
    ca: &str,
    version: i64,
) -> Result<()> {
    sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING")
        .bind(format!("superseded-identities:{rotation}")).bind(KIND)
        .bind(json!({"rotation_id":rotation,"ca_id":ca,"ca_version":version,"lease_resources":[format!("ca:{ca}")]}))
        .execute(&mut **tx).await?;
    Ok(())
}

pub(super) async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        if let Err(error) = scheduled_pass(&state).await {
            tracing::warn!(%error,"superseded identity materialization deferred");
        }
    }
}

pub(super) async fn scheduled_pass(state: &AppState) -> Result<()> {
    if let Some(work) = maintenance::claim(&state.pool, KIND, &state.instance_id).await? {
        let _ = crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = page(&state.pool, &work).await {
                tracing::warn!(%error,work_id=%work.id,"superseded identity page deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
    }
    Ok(())
}

async fn page(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::serializable(pool, move |tx| {
        let work = work.clone();
        Box::pin(async move {
            work.guard_on(tx).await?;
            let rotation = work.payload["rotation_id"].as_str().context("missing rotation")?;
            let ca = work.payload["ca_id"].as_str().context("missing CA")?;
            let version = work.payload["ca_version"].as_i64().context("missing generation")?;
            ensure!(work.id == format!("superseded-identities:{rotation}")
                && work.leases.iter().any(|l| l.resource == format!("ca:{ca}")),
                "wrong superseded identity scope");
            sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR KEY SHARE")
                .bind(ca).fetch_optional(&mut **tx).await?;
            sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE")
                .bind(ca).fetch_optional(&mut **tx).await?;
            let source: Option<bool> = sqlx::query_scalar("SELECT g.state='retired' AND g.published_at IS NULL AND r.phase='complete' AND coalesce(r.outcome='superseded_unpublished_target',false) FROM mtls_rotations r JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.id=$1 AND r.ca_id=$2 AND r.ca_version=$3 FOR UPDATE OF r,g")
                .bind(rotation).bind(ca).bind(version).fetch_optional(&mut **tx).await?;
            if source != Some(true) {
                return work.finish_on(tx, false, 0, Some("superseded_identity_source_changed")).await;
            }
            // The existing release-pending index also bounds discard candidates.
            // Locked leaves stay pending; an empty SKIP LOCKED page is not done.
            sqlx::query("UPDATE mtls_user_versions SET discarded=true WHERE (identity_id,credential_version) IN (SELECT identity_id,credential_version FROM mtls_user_versions WHERE ca_id=$1 AND ca_version=$2 AND NOT released AND NOT discarded ORDER BY identity_id,credential_version LIMIT $3 FOR UPDATE SKIP LOCKED)")
                .bind(ca).bind(version).bind(PAGE).execute(&mut **tx).await?;
            let pending: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_user_versions WHERE ca_id=$1 AND ca_version=$2 AND NOT released AND NOT discarded)")
                .bind(ca).bind(version).fetch_one(&mut **tx).await?;
            work.finish_on(tx, pending, if pending {1} else {0}, None).await
        })
    }).await
}

#[cfg(test)]
mod tests;
