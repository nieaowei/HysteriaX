//! Indexed, restart-safe maintenance. Shared renewals publish one version per
//! user/CA and never enqueue connection kicks or increment users.revision.
use anyhow::Result;

use crate::state::AppState;

#[cfg(test)]
pub use super::identity_maintenance::pass as issue_pending;

pub async fn run(state: AppState) {
    tokio::join!(
        maintenance_loop(state.clone()),
        super::retention::run(state.clone()),
        super::identity_maintenance::run(state.clone()),
        super::probe_maintenance::run(state.clone()),
        super::authority_maintenance::run(state.clone()),
        super::rotations::run_preparation(state)
    );
}

async fn maintenance_loop(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = shutdown.wait_for(|stop| *stop) => return,
            _ = interval.tick() => {}
        }
        if let Err(error) = maintain(&state).await {
            tracing::error!(%error,"managed certificate maintenance failed");
        }
    }
}

pub async fn maintain(state: &AppState) -> Result<()> {
    super::identity_maintenance::pass(state).await?;
    super::probe_maintenance::pass(state).await?;
    super::authority_maintenance::pass(state).await?;
    super::rotations::run_pass(state).await?;
    super::retention::prune(state).await?;
    Ok(())
}

/// Each probe belongs to a node. It is independent of user eligibility/policy.
#[cfg(test)]
pub async fn renew_probe(
    state: &AppState,
    node: &str,
    ca: &str,
    ca_version: i64,
) -> Result<(String, i64)> {
    renew_probe_on(&state.pool, &state.secrets, node, ca, ca_version).await
}

pub async fn renew_probe_on(
    pool: &sqlx::PgPool,
    secrets: &crate::security::SecretBox,
    node: &str,
    ca: &str,
    ca_version: i64,
) -> Result<(String, i64)> {
    let prepared = super::probe::prepare(pool, secrets, node, ca, ca_version).await?;
    super::probe::commit(pool, prepared).await
}
