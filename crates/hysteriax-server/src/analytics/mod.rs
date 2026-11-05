//! Transactional flow totals and idempotently rebuilt online-cycle analytics.
use crate::{db, error::ApiError, state::AppState};
use serde_json::Value;
use sqlx::PgPool;
mod refresh;
pub use refresh::refresh;
#[cfg(test)]
use refresh::{Queue, claim_on, refresh_queue_on};

pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if let Err(error) = refresh(&state.pool).await {
            tracing::error!(%error, "analytics refresh failed");
        }
    }
}

pub async fn snapshot(
    pool: &PgPool,
    end: chrono::DateTime<chrono::Utc>,
    needs_cycles: bool,
) -> Result<crate::read_snapshot::Reader, ApiError> {
    if let Some(reader) = crate::read_snapshot::current().await {
        return Ok(reader);
    }
    Ok(crate::read_snapshot::Reader::owned(
        snapshot_transaction(pool, end, needs_cycles).await?,
    ))
}

pub(crate) async fn snapshot_transaction(
    pool: &PgPool,
    _end: chrono::DateTime<chrono::Utc>,
    _needs_cycles: bool,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, ApiError> {
    // History overlays dirty source keys inside this same snapshot. A reader
    // neither drains another worker's queue nor falls back to a global writer.
    Ok(crate::read_snapshot::transaction(pool).await?)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod followup_tests;

#[cfg(test)]
mod node_deletion_tests;

#[cfg(test)]
mod dirty_queue_tests;
