//! Indexed, restart-safe maintenance. Shared renewals publish one version per
//! user/CA and never enqueue connection kicks or increment users.revision.
use anyhow::{Result, ensure};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use super::{ca::Authority, identities};
use crate::{db, state::AppState};

const PAGE: i64 = 100;

pub async fn run(state: AppState) {
    tokio::join!(
        maintenance_loop(state.clone()),
        super::retention::run(state)
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
    let now = Utc::now();
    let mut cursor = (
        DateTime::<Utc>::from_timestamp(0, 0).expect("Unix epoch"),
        String::new(),
    );
    loop {
        let rows = sqlx::query("SELECT id,user_id,ca_id,next_action_at FROM mtls_user_identities WHERE NOT archived AND next_action_at<=$1 AND (next_action_at,id)>($2,$3) ORDER BY next_action_at,id LIMIT $4")
            .bind(now).bind(cursor.0).bind(&cursor.1).bind(PAGE).fetch_all(&state.pool).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            cursor = (row.get("next_action_at"), row.get("id"));
            renew_user(
                state,
                &row.get::<String, _>("id"),
                &row.get::<String, _>("user_id"),
                &row.get::<String, _>("ca_id"),
            )
            .await?;
        }
    }
    // Paused and first-time pending identities carry no due time. Eligibility is
    // checked live so quota resets and the passage of time need no write event.
    let mut pending_cursor = String::new();
    loop {
        let rows=sqlx::query("SELECT i.id,i.user_id,i.ca_id FROM mtls_user_identities i JOIN users u ON u.id=i.user_id WHERE NOT i.archived AND i.next_action_at IS NULL AND i.id>$1 AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$2) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes) ORDER BY i.id LIMIT $3")
            .bind(&pending_cursor).bind(now).bind(PAGE).fetch_all(&state.pool).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            pending_cursor = row.get("id");
            renew_user(
                state,
                &pending_cursor,
                &row.get::<String, _>("user_id"),
                &row.get::<String, _>("ca_id"),
            )
            .await?;
        }
    }
    renew_probes(state, now).await?;
    rotate_authorities(state, now).await?;
    super::rotations::run_pass(state).await?;
    super::retention::prune(state).await?;
    Ok(())
}

async fn renew_user(state: &AppState, id: &str, user: &str, ca: &str) -> Result<()> {
    let row=sqlx::query("SELECT a.published_version,u.enabled,u.expires_at,u.quota_bytes,u.usage_bytes,i.revision FROM mtls_authorities a JOIN mtls_user_identities i ON i.ca_id=a.credential_id JOIN users u ON u.id=i.user_id WHERE a.credential_id=$1 AND u.id=$2 AND i.id=$3")
        .bind(ca).bind(user).bind(id).fetch_optional(&state.pool).await?;
    let Some(row) = row else { return Ok(()) };
    let now = Utc::now();
    let eligible = row.get::<bool, _>("enabled")
        && row
            .get::<Option<DateTime<Utc>>, _>("expires_at")
            .is_none_or(|t| t > now)
        && row
            .get::<Option<i64>, _>("quota_bytes")
            .is_none_or(|q| row.get::<i64, _>("usage_bytes") < q);
    let revision: i64 = row.get("revision");
    if !eligible {
        let mut tx = db::begin_write(&state.pool).await?;
        sqlx::query(
            "UPDATE mtls_user_identities SET next_action_at=NULL WHERE id=$1 AND revision=$2",
        )
        .bind(id)
        .bind(revision)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    let result = async {
        let candidate = identities::prepare_user(
            &state.pool,
            &state.secrets,
            user,
            ca,
            row.get("published_version"),
            false,
        )
        .await?;
        let mut tx = db::begin_write(&state.pool).await?;
        identities::commit_user(&mut tx, &state.secrets, candidate).await?;
        tx.commit().await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        let mut tx = db::begin_write(&state.pool).await?;
        sqlx::query("UPDATE mtls_user_identities SET maintenance_error=$3,next_action_at=$4 WHERE id=$1 AND revision=$2").bind(id).bind(revision).bind(error.to_string()).bind(now+Duration::hours(1)).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Each probe belongs to a node. It is independent of user eligibility/policy.
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
    let now = Utc::now();
    let row=sqlx::query("SELECT n.desired_revision,g.state FROM nodes n CROSS JOIN mtls_ca_generations g WHERE n.id=$1 AND g.ca_id=$2 AND g.ca_version=$3")
        .bind(node).bind(ca).bind(ca_version).fetch_one(pool).await?;
    let generation: String = row.get("state");
    ensure!(generation != "retired", "retired CA cannot sign a probe");
    let expected: i64 = row.get("desired_revision");
    let existing=sqlx::query("SELECT p.credential_id,p.credential_version FROM mtls_probe_versions p WHERE node_id=$1 AND ca_id=$2 AND ca_version=$3 AND NOT discarded ORDER BY credential_version DESC LIMIT 1")
        .bind(node).bind(ca).bind(ca_version).fetch_optional(pool).await?;
    let expected_probe = existing.as_ref().map(|r| {
        (
            r.get::<String, _>("credential_id"),
            r.get::<i64, _>("credential_version"),
        )
    });
    let (_, _, _, payload, _) = super::load(pool, secrets, ca, ca_version).await?;
    let node_id = Uuid::parse_str(node)?;
    let issued = tokio::task::spawn_blocking(move || {
        Authority::from_payload(&payload, now)?.sign_probe(&node_id, now)
    })
    .await??;
    let mut metadata = super::validate_payload("tls_identity", &issued.material.payload())?;
    metadata["managed"] = json!(true);
    metadata["internal_probe"] = json!(true);
    metadata["ca_id"] = json!(ca);
    metadata["node_id"] = json!(node);
    metadata["ca_version"] = json!(ca_version);
    let mut tx = db::begin_write(pool).await?;
    let current:Option<(i64,String)>=sqlx::query_as("SELECT n.desired_revision,g.state FROM nodes n CROSS JOIN mtls_ca_generations g WHERE n.id=$1 AND g.ca_id=$2 AND g.ca_version=$3")
        .bind(node).bind(ca).bind(ca_version).fetch_optional(&mut *tx).await?;
    ensure!(
        current == Some((expected, generation.clone())),
        "node or CA changed during probe signing"
    );
    let current_probe:Option<(String,i64)>=sqlx::query_as("SELECT credential_id,credential_version FROM mtls_probe_versions WHERE node_id=$1 AND ca_id=$2 AND ca_version=$3 AND NOT discarded ORDER BY credential_version DESC LIMIT 1")
        .bind(node).bind(ca).bind(ca_version).fetch_optional(&mut *tx).await?;
    ensure!(
        current_probe == expected_probe,
        "probe identity changed during signing"
    );
    let (credential, version) = if let Some((credential, version)) = expected_probe {
        // Reuse the node identity but generate a fresh private key each time.
        let next: i64 = sqlx::query_scalar("SELECT latest_version+1 FROM credentials WHERE id=$1")
            .bind(&credential)
            .fetch_one(&mut *tx)
            .await?;
        let previous:DateTime<Utc>=sqlx::query_scalar("SELECT expires_at FROM mtls_probe_versions WHERE node_id=$1 AND credential_id=$2 AND credential_version=$3")
            .bind(node).bind(&credential).bind(version).fetch_one(&mut *tx).await?;
        ensure!(
            issued.expires_at > previous,
            "CA update required: cannot extend probe certificate"
        );
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES($1,$2,$3,$4,$5)").bind(&credential).bind(next).bind(secrets.encrypt(&issued.material.payload().to_string())?).bind(metadata).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE credentials SET latest_version=$2,revision=revision+1,updated_at=$3 WHERE id=$1").bind(&credential).bind(next).bind(now).execute(&mut *tx).await?;
        (credential, next)
    } else {
        (
            super::insert(
                &mut tx,
                secrets,
                &format!("Internal probe · {node}"),
                "tls_identity",
                None,
                &issued.material.payload(),
                &metadata,
                &json!({}),
            )
            .await?,
            1,
        )
    };
    sqlx::query("INSERT INTO mtls_probe_versions(node_id,credential_id,credential_version,ca_id,ca_version,expires_at,next_action_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(node).bind(&credential).bind(version).bind(ca).bind(ca_version).bind(issued.expires_at).bind(issued.next_action_at).execute(&mut *tx).await?;
    if generation == "published" {
        sqlx::query("UPDATE nodes SET mtls_error=NULL WHERE id=$1 AND mtls_state='ready' AND mtls_error LIKE 'Probe certificate renewal failed:%'").bind(node).execute(&mut *tx).await?;
        super::rotations::switch_probe_on(&mut tx, node, ca, ca_version).await?;
    }
    tx.commit().await?;
    Ok((credential, version))
}

async fn renew_probes(state: &AppState, now: DateTime<Utc>) -> Result<()> {
    let mut cursor = (
        DateTime::<Utc>::from_timestamp(0, 0).expect("Unix epoch"),
        String::new(),
        String::new(),
        0_i64,
    );
    loop {
        let rows=sqlx::query("SELECT p.node_id,p.ca_id,p.ca_version,p.next_action_at,p.credential_id,p.credential_version FROM mtls_probe_versions p JOIN mtls_ca_generations g ON g.ca_id=p.ca_id AND g.ca_version=p.ca_version WHERE g.state='published' AND NOT p.discarded AND p.next_action_at<=$1 AND (p.next_action_at,p.node_id,p.ca_id,p.ca_version)>($2,$3,$4,$5) AND NOT EXISTS(SELECT 1 FROM mtls_probe_versions newer WHERE newer.node_id=p.node_id AND newer.ca_id=p.ca_id AND newer.ca_version=p.ca_version AND newer.credential_version>p.credential_version AND NOT newer.discarded) AND EXISTS(SELECT 1 FROM mtls_node_trust t WHERE t.node_id=p.node_id AND t.ca_id=p.ca_id AND t.ca_version=p.ca_version AND (t.desired OR t.deployed)) ORDER BY p.next_action_at,p.node_id,p.ca_id,p.ca_version LIMIT $6")
            .bind(now).bind(cursor.0).bind(&cursor.1).bind(&cursor.2).bind(cursor.3).bind(PAGE).fetch_all(&state.pool).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let node: String = row.get("node_id");
            cursor = (
                row.get("next_action_at"),
                node.clone(),
                row.get("ca_id"),
                row.get("ca_version"),
            );
            if let Err(error) = renew_probe(
                state,
                &node,
                &row.get::<String, _>("ca_id"),
                row.get("ca_version"),
            )
            .await
            {
                tracing::warn!(node_id=node,%error,"probe certificate maintenance failed");
                let mut tx = db::begin_write(&state.pool).await?;
                sqlx::query("UPDATE nodes SET mtls_error=$2 WHERE id=$1 AND mtls_state='ready'")
                    .bind(&node)
                    .bind(format!("Probe certificate renewal failed: {error}"))
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("UPDATE mtls_probe_versions SET next_action_at=$4 WHERE node_id=$1 AND credential_id=$2 AND credential_version=$3").bind(&node).bind(row.get::<String,_>("credential_id")).bind(row.get::<i64,_>("credential_version")).bind(now+Duration::hours(1)).execute(&mut *tx).await?;
                tx.commit().await?;
            }
        }
    }
    Ok(())
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
