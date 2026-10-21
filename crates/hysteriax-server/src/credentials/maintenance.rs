//! Indexed, restart-safe maintenance. Shared renewals publish one version per
//! user/CA and never enqueue connection kicks or increment users.revision.
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use sqlx::Row;

use crate::{db, state::AppState};

const PAGE: i64 = 100;

pub async fn run(state: AppState) {
    tokio::join!(
        maintenance_loop(state.clone()),
        super::retention::run(state.clone()),
        super::identity_maintenance::run(state.clone()),
        super::probe_maintenance::run(state)
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
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await?;
    super::probe_maintenance::pass(state).await?;
    rotate_authorities(state, now).await?;
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

async fn rotate_authorities(state: &AppState, now: DateTime<Utc>) -> Result<()> {
    use crate::api::{
        AdminActor,
        managed_mtls::{PublishAuthority, publish},
    };
    use axum::{
        Json,
        extract::{Extension, Path, State},
    };
    let mut cursor = (
        DateTime::<Utc>::from_timestamp(0, 0).expect("Unix epoch"),
        String::new(),
    );
    loop {
        let rows=sqlx::query("SELECT a.credential_id,c.revision,a.published_version,a.next_action_at FROM mtls_authorities a JOIN credentials c ON c.id=a.credential_id WHERE a.source='generated' AND NOT c.archived AND a.next_action_at<=$1 AND (a.next_action_at,a.credential_id)>($2,$3) AND NOT EXISTS(SELECT 1 FROM mtls_rotations r WHERE r.ca_id=a.credential_id AND r.phase<>'complete') ORDER BY a.next_action_at,a.credential_id LIMIT $4")
        .bind(now).bind(cursor.0).bind(&cursor.1).bind(PAGE).fetch_all(&state.pool).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            cursor = (row.get("next_action_at"), row.get("credential_id"));
            let id: String = row.get("credential_id");
            let version: i64 = row.get("published_version");
            let result = publish(
                State(state.clone()),
                Extension(AdminActor {
                    id: "system:mtls-maintenance".into(),
                }),
                Path(id.clone()),
                Json(PublishAuthority {
                    expected_revision: row.get("revision"),
                    certificate: None,
                    private_key: None,
                    replace_rotation_id: None,
                    idempotency_key: format!("auto-ca:{id}:{version}"),
                }),
            )
            .await;
            if let Err(error) = result {
                let mut tx = db::begin_write(&state.pool).await?;
                sqlx::query("UPDATE mtls_authorities SET maintenance_error=$2,next_action_at=$3 WHERE credential_id=$1 AND published_version=$4").bind(&id).bind(error.message).bind(now+Duration::hours(1)).bind(version).execute(&mut *tx).await?;
                tx.commit().await?;
            }
        }
    }
    Ok(())
}
