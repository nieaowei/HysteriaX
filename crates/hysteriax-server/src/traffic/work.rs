//! Explicit maintenance adoption of legacy batches, without replacing counters,
//! IDs, checkpoints or historical cycle denominators.
use crate::db;
use anyhow::{Result, ensure};
use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

const INITIALIZATION_KEY: &str = "legacy-traffic-work-v1";

pub async fn assert_ready(pool: &PgPool) -> Result<()> {
    let initialized: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_initialization WHERE key=$1)")
            .bind(INITIALIZATION_KEY)
            .fetch_one(pool)
            .await?;
    ensure!(
        initialized,
        "traffic work initialization is incomplete; run --migrate-only during maintenance"
    );
    assert_no_orphans(pool).await
}
async fn assert_no_orphans(pool: &PgPool) -> Result<()> {
    let orphan:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM traffic_sample_batches b WHERE b.state IN ('capturing','ready') AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id=b.sample_id AND w.kind='traffic' AND w.node_id=b.node_id))").fetch_one(pool).await?;
    ensure!(
        !orphan,
        "unfinished traffic batch has no execution work; run --migrate-only during maintenance"
    );
    Ok(())
}

/// Called only after all previous server processes have stopped for maintenance.
/// Uncaptured reads cannot be resumed. Captured data is left byte-for-byte intact
/// and receives a private legacy membership record, never a new remote read.
pub async fn initialize(pool: &PgPool) -> Result<()> {
    sqlx::query("DELETE FROM background_initialization WHERE key=$1")
        .bind(INITIALIZATION_KEY)
        .execute(pool)
        .await?;
    loop {
        if adopt_batch(pool).await? == 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_no_orphans(pool).await?;
    sqlx::query("INSERT INTO background_initialization(key) VALUES($1) ON CONFLICT(key) DO UPDATE SET completed_at=clock_timestamp()")
        .bind(INITIALIZATION_KEY).execute(pool).await?;
    assert_ready(pool).await
}

pub(crate) async fn adopt_batch(pool: &PgPool) -> Result<usize> {
    db::write(pool,|tx|Box::pin(async move {
        let rows=sqlx::query("SELECT b.* FROM traffic_sample_batches b WHERE b.state IN ('capturing','ready') AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id=b.sample_id) ORDER BY b.sample_order LIMIT 256 FOR UPDATE OF b SKIP LOCKED")
            .fetch_all(&mut **tx).await?;
        for row in &rows {
            let id:String=row.get("sample_id");let node:String=row.get("node_id");
            let state:String=row.get("state");
            let digest:Option<String>=row.get("payload_digest");
            if state=="capturing" {
                ensure!(digest.is_none() && row.get::<i64,_>("item_count")==0,"inconsistent legacy capture; counters were preserved for inspection");
                sqlx::query("UPDATE traffic_sample_batches SET state='discarded',completed_at=clock_timestamp() WHERE sample_id=$1").bind(&id).execute(&mut **tx).await?;
                sqlx::query("UPDATE online_samples SET traffic_status='failed',pending=false WHERE node_id=$1 AND cycle_id=$2")
                    .bind(&node).bind(row.get::<Option<Uuid>,_>("cycle_id")).execute(&mut **tx).await?;
                continue;
            }
            ensure!(digest.is_some(),"legacy ready batch has no captured payload; data was preserved for inspection");
            let at:chrono::DateTime<chrono::Utc>=row.get("sampled_at");
            // Legacy cycles may contain multiple old reads per node and period.
            // Keep the original batch cycle untouched and create one membership
            // record per retained batch, outside normal aligned sampling kinds.
            let cycle:Uuid=sqlx::query_scalar("INSERT INTO sampling_cycles(id,kind,period_start,deadline_at,expected_nodes) VALUES(gen_random_uuid(),$1,$2,$2+interval '10 seconds',1) ON CONFLICT(kind,period_start) DO UPDATE SET expected_nodes=sampling_cycles.expected_nodes RETURNING id")
                .bind(format!("legacy-billing:{id}")).bind(at).fetch_one(&mut **tx).await?;
            sqlx::query("INSERT INTO background_work(id,kind,cycle_id,node_id,payload,deadline_at) VALUES($1,'traffic',$2,$3,$4,$5)")
                .bind(&id).bind(cycle).bind(&node).bind(json!({"captured_at":at,"legacy_billing":true})).bind(at+chrono::Duration::seconds(10)).execute(&mut **tx).await?;
        }
        Ok(rows.len())
    })).await
}

#[cfg(test)]
#[path = "work_tests.rs"]
mod tests;
