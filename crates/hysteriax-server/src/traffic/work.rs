//! Read-only validation of durable traffic execution ownership.
use anyhow::{Result, ensure};
use sqlx::PgPool;

pub async fn assert_ready(pool: &PgPool) -> Result<()> {
    assert_no_orphans(pool).await
}
async fn assert_no_orphans(pool: &PgPool) -> Result<()> {
    let orphan:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM traffic_sample_batches b WHERE b.state IN ('capturing','ready') AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id=b.sample_id AND w.kind='traffic' AND w.node_id=b.node_id))").fetch_one(pool).await?;
    ensure!(
        !orphan,
        "unfinished traffic batch has no execution work; inspect durable traffic state before starting APIs"
    );
    Ok(())
}

#[cfg(test)]
#[path = "work_tests.rs"]
mod tests;
