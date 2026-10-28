//! Fenced, single-shot destructive traffic capture and uncertain-response recovery.
use anyhow::{Result, ensure};
use sqlx::{PgPool, Row};

use crate::db;

#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Send,
    AlreadyRequested,
    Captured,
}

/// Commit the one-shot marker in the same transaction as both lease and work
/// checks. A caller must never send after AlreadyRequested, even if it owns the
/// same lease: the previous process may have lost the commit acknowledgement.
pub async fn prepare(pool: &PgPool, work: &crate::background::Work) -> Result<Admission> {
    let work = work.clone();
    let node_lease = work
        .leases
        .iter()
        .find(|l| l.resource == format!("node:{}", work.node))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing node lease"))?;
    let work_lease = work
        .leases
        .iter()
        .find(|l| l.resource == format!("work:{}", work.id))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing work lease"))?;
    ensure!(
        node_lease.work == work.id
            && work_lease.work == work.id
            && node_lease.owner == work_lease.owner
            && node_lease.epoch == work_lease.epoch,
        "mismatched traffic execution leases"
    );
    db::write(pool, move |tx| {
        let (work, node_lease, work_lease) = (work.clone(), node_lease.clone(), work_lease.clone());
        Box::pin(async move {
            let mut leases = [node_lease.clone(), work_lease.clone()];
            leases.sort_by(|a,b| a.resource.cmp(&b.resource));
            for lease in &leases { lease.guard(tx).await?; }
            let valid: Option<bool> = sqlx::query_scalar("SELECT true FROM background_work WHERE id=$1 AND kind='traffic' AND node_id=$2 AND owner=$3 AND generation=$4 AND state='running' AND deadline_at>clock_timestamp() FOR UPDATE")
                .bind(&work.id).bind(&work.node).bind(&work_lease.owner).bind(work_lease.generation)
                .fetch_optional(&mut **tx).await?;
            ensure!(valid == Some(true), "traffic work ownership was lost or its slot expired");
            let active: Option<bool> = sqlx::query_scalar("SELECT true FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
                .bind(&work.node).fetch_optional(&mut **tx).await?;
            ensure!(active == Some(true), "node is missing or being deleted");
            let row = sqlx::query("SELECT node_id,cycle_id,state,payload_digest,clear_requested_at FROM traffic_sample_batches WHERE sample_id=$1 FOR UPDATE")
                .bind(&work.id).fetch_one(&mut **tx).await?;
            ensure!(row.get::<String,_>("node_id")==work.node && row.get::<Option<uuid::Uuid>,_>("cycle_id")==Some(work.cycle), "traffic batch does not belong to work");
            if row.get::<Option<String>,_>("payload_digest").is_some() { return Ok(Admission::Captured); }
            ensure!(row.get::<String,_>("state")=="capturing", "traffic batch is no longer capturing");
            if row.get::<Option<chrono::DateTime<chrono::Utc>>,_>("clear_requested_at").is_some() {
                return Ok(Admission::AlreadyRequested);
            }
            let earlier: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM traffic_sample_batches WHERE node_id=$1 AND sample_order<(SELECT sample_order FROM traffic_sample_batches WHERE sample_id=$2) AND state IN ('capturing','ready'))")
                .bind(&work.node).bind(&work.id).fetch_one(&mut **tx).await?;
            ensure!(!earlier, "earlier traffic batch must finish before a clear request");
            // A node or batch lock may have delayed admission past the slot or
            // lease expiry after the first check. Recheck immediately before
            // committing the one-shot marker.
            for lease in &leases { lease.guard(tx).await?; }
            let timely: bool = sqlx::query_scalar("SELECT deadline_at>clock_timestamp() FROM background_work WHERE id=$1")
                .bind(&work.id).fetch_one(&mut **tx).await?;
            ensure!(timely, "traffic work slot expired before clear request");
            sqlx::query("UPDATE traffic_sample_batches SET clear_requested_at=clock_timestamp(),clear_owner=$2,clear_work_generation=$3,clear_node_generation=$4,clear_epoch=$5 WHERE sample_id=$1")
                .bind(&work.id).bind(&work_lease.owner).bind(work_lease.generation)
                .bind(node_lease.generation).bind(&work_lease.epoch).execute(&mut **tx).await?;
            for lease in &leases { lease.guard(tx).await?; }
            Ok(Admission::Send)
        })
    }).await
}

/// The caller must already have fenced the worker, or have established that
/// its lease expired while holding the appropriate maintenance locks.
pub(crate) async fn uncertain_on(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    id: &str,
) -> Result<bool> {
    let node: Option<String> =
        sqlx::query_scalar("SELECT node_id FROM traffic_sample_batches WHERE sample_id=$1")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(node) = node else {
        return Ok(false);
    };
    let active: Option<bool> =
        sqlx::query_scalar("SELECT true FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
            .bind(&node)
            .fetch_optional(&mut **tx)
            .await?;
    if active.is_none() {
        return Ok(false);
    }
    let row = sqlx::query("SELECT state,payload_digest,clear_requested_at,cycle_id,sampled_at FROM traffic_sample_batches WHERE sample_id=$1 FOR UPDATE")
        .bind(id).fetch_one(&mut **tx).await?;
    if row.get::<String, _>("state") != "capturing"
        || row.get::<Option<String>, _>("payload_digest").is_some()
        || row
            .get::<Option<chrono::DateTime<chrono::Utc>>, _>("clear_requested_at")
            .is_none()
    {
        return Ok(false);
    }
    sqlx::query("UPDATE traffic_sample_batches SET state='discarded',completed_at=clock_timestamp() WHERE sample_id=$1")
        .bind(id).execute(&mut **tx).await?;
    // Keep each lost destructive response visible even when another kind of
    // gap is already open. The batch state transition makes this insert once.
    sqlx::query("INSERT INTO data_gaps(id,node_id,opened_at,reason) VALUES($1,$2,$3,'traffic_clear_uncertain')")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&node)
        .bind(row.get::<chrono::DateTime<chrono::Utc>,_>("sampled_at")).execute(&mut **tx).await?;
    sqlx::query("UPDATE online_samples SET traffic_status='failed',pending=false WHERE node_id=$1 AND cycle_id=$2")
        .bind(&node).bind(row.get::<Option<uuid::Uuid>,_>("cycle_id")).execute(&mut **tx).await?;
    Ok(true)
}

/// Finalize an uncertain response under the current execution's two leases.
/// The sampling deadline is intentionally not checked after dispatch.
pub async fn uncertain(pool: &PgPool, work: &crate::background::Work) -> Result<bool> {
    let work = work.clone();
    db::write(pool, move |tx| {
        let work = work.clone();
        Box::pin(async move {
            let node_lease = work.leases.iter().find(|l| l.resource == format!("node:{}", work.node))
                .ok_or_else(|| anyhow::anyhow!("missing node lease"))?;
            let work_lease = work.leases.iter().find(|l| l.resource == format!("work:{}", work.id))
                .ok_or_else(|| anyhow::anyhow!("missing work lease"))?;
            ensure!(node_lease.work == work.id && work_lease.work == work.id && node_lease.owner == work_lease.owner && node_lease.epoch == work_lease.epoch, "mismatched traffic execution leases");
            let mut leases = [node_lease, work_lease];
            leases.sort_by(|a,b| a.resource.cmp(&b.resource));
            for lease in &leases { lease.guard(tx).await?; }
            let valid: Option<bool> = sqlx::query_scalar("SELECT true FROM background_work WHERE id=$1 AND kind='traffic' AND node_id=$2 AND owner=$3 AND generation=$4 AND state='running' FOR UPDATE")
                .bind(&work.id).bind(&work.node).bind(&work_lease.owner).bind(work_lease.generation)
                .fetch_optional(&mut **tx).await?;
            ensure!(valid == Some(true), "traffic work ownership was lost");
            let changed = uncertain_on(tx, &work.id).await?;
            for lease in &leases { lease.guard(tx).await?; }
            Ok(changed)
        })
    }).await
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod response_tests;
