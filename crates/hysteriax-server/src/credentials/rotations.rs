//! Persistent CA coordination. Remote work is queued through the ordinary node
//! worker (one running job per node); publication is a single write transaction.
use super::{
    identities,
    node_mtls::{Settings, TrustVersion},
};
use crate::{
    api::enqueue_job_with_payload_in_tx, db, deployment::JobInput, security::SecretBox,
    state::AppState,
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

const PAGE: i64 = 100;

mod trust;
mod users;

pub(super) async fn run_preparation(state: AppState) {
    tokio::join!(trust::run(state.clone()), users::run(state));
}

pub async fn members_on(connection: &mut sqlx::PgConnection, ca: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT DISTINCT node_id FROM (SELECT node_id FROM mtls_node_trust WHERE ca_id=$1 AND (desired OR deployed) UNION SELECT node_id FROM mtls_live_activation_trust WHERE ca_id=$1) members ORDER BY node_id").bind(ca).fetch_all(connection).await?)
}

async fn proof_on(
    connection: &mut sqlx::PgConnection,
    node: &str,
    ca: &str,
    version: i64,
) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM nodes n JOIN mtls_activations a ON a.id=n.mtls_activation_id JOIN mtls_probe_versions p ON p.node_id=n.id AND p.credential_id=a.probe_credential_id AND p.credential_version=a.probe_credential_version JOIN mtls_node_trust t ON t.node_id=n.id AND t.ca_id=p.ca_id AND t.ca_version=p.ca_version WHERE n.id=$1 AND n.mtls_state='ready' AND a.phase='committed' AND n.deployed_content_sha256=a.target_digest AND p.ca_id=$2 AND p.ca_version=$3 AND NOT p.discarded AND p.expires_at>clock_timestamp() AND t.deployed)")
        .bind(node).bind(ca).bind(version).fetch_one(connection).await?)
}

pub async fn run_pass(state: &AppState) -> Result<()> {
    let now = Utc::now();
    let mut cursor = (
        DateTime::<Utc>::from_timestamp(0, 0).expect("Unix epoch"),
        String::new(),
    );
    loop {
        let rows:Vec<(String,String,DateTime<Utc>)>=sqlx::query_as("SELECT id,ca_id,next_action_at FROM mtls_rotations WHERE phase<>'complete' AND next_action_at<=$1 AND (next_action_at,id)>($2,$3) ORDER BY next_action_at,id LIMIT $4").bind(now).bind(cursor.0).bind(&cursor.1).bind(PAGE).fetch_all(&state.pool).await?;
        if rows.is_empty() {
            break;
        }
        for (id, ca, due) in rows {
            cursor = (due, id.clone());
            // Session lock is independent of the global transaction write lock.
            // Closing on drop also releases it if this future is cancelled.
            let mut lease = state.pool.acquire().await?;
            lease.close_on_drop();
            let locked: bool =
                sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1,0))")
                    .bind(format!("hysteriax:mtls-ca:{ca}"))
                    .fetch_one(&mut *lease)
                    .await?;
            if !locked {
                continue;
            }
            let result = advance(state, &id).await;
            let _ = sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1,0))")
                .bind(format!("hysteriax:mtls-ca:{ca}"))
                .execute(&mut *lease)
                .await;
            if let Err(error) = result {
                record_error(&state.pool, &id, &error.to_string()).await?;
            }
        }
    }
    cleanup_pass(state).await?;
    Ok(())
}

async fn record_error(pool: &PgPool, id: &str, error: &str) -> Result<()> {
    let mut tx = db::begin_write(pool).await?;
    sqlx::query("UPDATE mtls_rotations SET error_message=$2,next_action_at=$3,updated_at=$4 WHERE id=$1 AND phase<>'complete'").bind(id).bind(error).bind(Utc::now()+Duration::minutes(1)).bind(Utc::now()).execute(&mut *tx).await?;
    audit(&mut tx, id, "ca.rotation_failed", json!({"reason":error})).await?;
    tx.commit().await?;
    Ok(())
}

async fn refresh_members(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    ca: &str,
    version: i64,
) -> Result<Vec<String>> {
    let members = members_on(tx, ca).await?;
    sqlx::query("DELETE FROM mtls_rotation_nodes WHERE rotation_id=$1 AND NOT(node_id=ANY($2))")
        .bind(id)
        .bind(&members)
        .execute(&mut **tx)
        .await?;
    for node in &members {
        let proof = proof_on(tx, node, ca, version).await?;
        sqlx::query("INSERT INTO mtls_rotation_nodes(rotation_id,node_id,expected_revision,trust_prepared) SELECT $1,id,desired_revision,$3 FROM nodes WHERE id=$2 ON CONFLICT(rotation_id,node_id) DO UPDATE SET expected_revision=excluded.expected_revision,trust_prepared=$3")
            .bind(id).bind(node).bind(proof).execute(&mut **tx).await?;
    }
    Ok(members)
}

pub async fn advance(state: &AppState, id: &str) -> Result<()> {
    let row = sqlx::query("SELECT * FROM mtls_rotations WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    let ca: String = row.get("ca_id");
    let version: i64 = row.get("ca_version");
    let phase: String = row.get("phase");
    if phase == "complete" {
        return Ok(());
    }
    if phase == "probes" {
        return switch_probes(state, id, &ca, version).await;
    }
    if !trust::pass(state, id, &ca, version).await? {
        return Ok(());
    }
    if !users::pass(state, id, &ca, version).await? {
        return Ok(());
    }
    publish_ready(state, id, &ca, version).await
}

async fn publish_ready(state: &AppState, id: &str, ca: &str, version: i64) -> Result<()> {
    let mut tx = db::begin_write(&state.pool).await?;
    let current = refresh_members(&mut tx, id, ca, version).await?;
    for node in current {
        if !proof_on(&mut tx, &node, ca, version).await? {
            tx.rollback().await?;
            return Ok(());
        }
    }
    // A user assigned while an earlier identity page was being processed must
    // not be omitted merely because it has no identity row yet.
    if users::missing_identity_on(&mut tx, ca).await? {
        tx.rollback().await?;
        return Ok(());
    }
    let missing:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_user_identities i JOIN users u ON u.id=i.user_id WHERE i.ca_id=$1 AND NOT i.archived AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$3) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes) AND NOT EXISTS(SELECT 1 FROM mtls_user_versions v WHERE v.identity_id=i.id AND v.ca_version=$2 AND NOT v.discarded AND v.not_before<=$3 AND v.expires_at>$3))")
        .bind(ca).bind(version).bind(Utc::now()).fetch_one(&mut *tx).await?;
    if missing {
        tx.commit().await?;
        return Ok(());
    }
    let phase: Option<String> = sqlx::query_scalar("SELECT phase FROM mtls_rotations WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    ensure!(
        phase
            .as_deref()
            .is_some_and(|s| matches!(s, "identities" | "publish")),
        "rotation no longer ready for publication"
    );
    let old: i64 =
        sqlx::query_scalar("SELECT published_version FROM mtls_authorities WHERE credential_id=$1")
            .bind(ca)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query(
        "UPDATE mtls_ca_generations SET state='retired' WHERE ca_id=$1 AND state='published'",
    )
    .bind(ca)
    .execute(&mut *tx)
    .await?;
    let changed=sqlx::query("UPDATE mtls_ca_generations SET state='published',published_at=$3 WHERE ca_id=$1 AND ca_version=$2 AND state='preparing' AND expires_at>$3").bind(ca).bind(version).bind(Utc::now()).execute(&mut *tx).await?;
    ensure!(
        changed.rows_affected() == 1,
        "target CA is no longer valid or preparing"
    );
    sqlx::query("UPDATE mtls_authorities SET published_version=$2,maintenance_error=NULL,next_action_at=CASE WHEN source='generated' THEN (SELECT expires_at-interval '365 days' FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=$2) ELSE NULL END WHERE credential_id=$1").bind(ca).bind(version).execute(&mut *tx).await?;
    sqlx::query("UPDATE mtls_user_versions SET released=TRUE WHERE ca_id=$1 AND ca_version=$2 AND NOT discarded").bind(ca).bind(version).execute(&mut *tx).await?;
    sqlx::query("UPDATE mtls_user_identities i SET next_action_at=CASE WHEN EXISTS(SELECT 1 FROM users u WHERE u.id=i.user_id AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$3) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes)) THEN (SELECT CASE WHEN v.next_action_at<=$3 AND v.expires_at>=(SELECT expires_at-interval '5 minutes' FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=$2) THEN $3+interval '1 hour' ELSE greatest($3,v.next_action_at) END FROM mtls_user_versions v WHERE v.identity_id=i.id AND v.ca_version=$2 AND v.released AND NOT v.discarded ORDER BY v.credential_version DESC LIMIT 1) ELSE NULL END,maintenance_error=CASE WHEN (SELECT v.next_action_at<=$3 AND v.expires_at>=(SELECT expires_at-interval '5 minutes' FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=$2) FROM mtls_user_versions v WHERE v.identity_id=i.id AND v.ca_version=$2 AND v.released AND NOT v.discarded ORDER BY v.credential_version DESC LIMIT 1) THEN 'CA update required: cannot extend the existing certificate' ELSE NULL END,revision=revision+1,updated_at=$3 WHERE i.ca_id=$1 AND NOT i.archived").bind(ca).bind(version).bind(Utc::now()).execute(&mut *tx).await?;
    let deadline = retention_deadline(&mut tx, ca, old).await?;
    sqlx::query("INSERT INTO mtls_trust_cleanup(ca_id,ca_version,next_action_at) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(ca).bind(old).bind(deadline).execute(&mut *tx).await?;
    update_node_metadata(&mut tx, &state.secrets, ca, version).await?;
    sqlx::query("UPDATE mtls_rotations SET phase='probes',published_at=$2,error_message=NULL,next_action_at=$2,updated_at=$2 WHERE id=$1").bind(id).bind(Utc::now()).execute(&mut *tx).await?;
    audit(
        &mut tx,
        id,
        "ca.published",
        json!({"version":version,"retired_version":old,"cleanup_after":deadline}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn switch_probes(state: &AppState, id: &str, ca: &str, version: i64) -> Result<()> {
    let mut tx = db::begin_write(&state.pool).await?;
    let members = refresh_members(&mut tx, id, ca, version).await?;
    let mut all = true;
    let mut waiting = Vec::new();
    for node in members {
        let switched = switch_probe_on(&mut tx, &node, ca, version).await?;
        all &= switched;
        if !switched {
            waiting.push(node.clone());
        }
        sqlx::query(
            "UPDATE mtls_rotation_nodes SET probe_switched=$3 WHERE rotation_id=$1 AND node_id=$2",
        )
        .bind(id)
        .bind(node)
        .bind(switched)
        .execute(&mut *tx)
        .await?;
    }
    if all {
        sqlx::query("UPDATE mtls_rotations SET phase='complete',error_message=NULL,updated_at=$2 WHERE id=$1").bind(id).bind(Utc::now()).execute(&mut *tx).await?;
        audit(
            &mut tx,
            id,
            "ca.rotation_completed",
            json!({"version":version}),
        )
        .await?;
    }
    if !all {
        sqlx::query("UPDATE mtls_rotations SET error_message=$2,next_action_at=$3 WHERE id=$1")
            .bind(id)
            .bind(format!(
                "Waiting for probe activation on nodes: {}",
                waiting.join(", ")
            ))
            .bind(Utc::now() + Duration::minutes(1))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn switch_probe_on(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    ca: &str,
    version: i64,
) -> Result<bool> {
    let probe:Option<(String,i64)>=sqlx::query_as("SELECT p.credential_id,p.credential_version FROM mtls_probe_versions p JOIN mtls_node_trust t ON t.node_id=p.node_id AND t.ca_id=p.ca_id AND t.ca_version=p.ca_version JOIN nodes n ON n.id=p.node_id WHERE p.node_id=$1 AND p.ca_id=$2 AND p.ca_version=$3 AND t.deployed AND n.mtls_state='ready' AND NOT p.discarded AND p.expires_at>clock_timestamp() ORDER BY p.credential_version DESC LIMIT 1").bind(node).bind(ca).bind(version).fetch_optional(&mut **tx).await?;
    let Some((credential, cv)) = probe else {
        return Ok(false);
    };
    sqlx::query("INSERT INTO mtls_active_probes(node_id,ca_id,ca_version,credential_id,credential_version) VALUES($1,$2,$3,$4,$5) ON CONFLICT(node_id) DO UPDATE SET ca_id=$2,ca_version=$3,credential_id=$4,credential_version=$5,revision=mtls_active_probes.revision+1").bind(node).bind(ca).bind(version).bind(credential).bind(cv).execute(&mut **tx).await?;
    Ok(true)
}

async fn enqueue_node(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    ca: &str,
    version: i64,
    rotation: Option<&str>,
    stage: &str,
) -> Result<String> {
    let row=sqlx::query("SELECT desired_revision,deployed_revision,mtls_activation_id,deployed_content_sha256,clock_timestamp() AS database_now FROM nodes WHERE id=$1").bind(node).fetch_one(&mut **tx).await?;
    let now: DateTime<Utc> = row.get("database_now");
    let base: Option<i64> = row.get("deployed_revision");
    let desired: i64 = row.get("desired_revision");
    let deployed_ca: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM mtls_node_trust WHERE node_id=$1 AND ca_id=$2 AND deployed)",
    )
    .bind(node)
    .bind(ca)
    .fetch_one(&mut **tx)
    .await?;
    let use_desired = !deployed_ca;
    let job = enqueue_job_with_payload_in_tx(
        tx,
        "deploy",
        Some(node),
        Some(if use_desired {
            desired
        } else {
            base.unwrap_or(desired)
        }),
        json!({"mtls_operation":true}),
    )
    .await
    .map_err(anyhow::Error::new)?;
    sqlx::query("INSERT INTO mtls_node_operations(job_id,node_id,ca_id,ca_version,rotation_id,stage,desired_revision,base_revision,base_activation_id,base_digest,use_desired,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
        .bind(&job).bind(node).bind(ca).bind(version).bind(rotation).bind(stage).bind(desired).bind(base).bind(row.get::<Option<String>,_>("mtls_activation_id")).bind(row.get::<Option<String>,_>("deployed_content_sha256")).bind(use_desired).bind(now).execute(&mut **tx).await?;
    Ok(job)
}

async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    rotation: &str,
    action: &str,
    detail: Value,
) -> Result<()> {
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) SELECT $1,actor_id,$2,'ca_rotation',id,$3::jsonb||jsonb_build_object('ca_credential_id',ca_id,'ca_version',ca_version),$4 FROM mtls_rotations WHERE id=$5").bind(Uuid::new_v4().to_string()).bind(action).bind(detail).bind(Utc::now()).bind(rotation).execute(&mut **tx).await?;
    Ok(())
}

async fn retention_deadline(
    connection: &mut sqlx::PgConnection,
    ca: &str,
    version: i64,
) -> Result<DateTime<Utc>> {
    Ok(sqlx::query_scalar::<_,Option<DateTime<Utc>>>("SELECT max(expires_at)+interval '5 minutes' FROM (SELECT expires_at FROM mtls_user_versions WHERE ca_id=$1 AND ca_version=$2 UNION ALL SELECT expires_at FROM mtls_probe_versions WHERE ca_id=$1 AND ca_version=$2) leaves").bind(ca).bind(version).fetch_one(connection).await?.unwrap_or_else(Utc::now))
}

async fn cleanup_pass(state: &AppState) -> Result<()> {
    let now = Utc::now();
    let mut cursor = (
        DateTime::<Utc>::from_timestamp(0, 0).expect("Unix epoch"),
        String::new(),
        0_i64,
    );
    loop {
        let rows:Vec<(String,i64,DateTime<Utc>)>=sqlx::query_as("SELECT ca_id,ca_version,next_action_at FROM mtls_trust_cleanup WHERE completed_at IS NULL AND next_action_at<=$1 AND (next_action_at,ca_id,ca_version)>($2,$3,$4) ORDER BY next_action_at,ca_id,ca_version LIMIT $5").bind(now).bind(cursor.0).bind(&cursor.1).bind(cursor.2).bind(PAGE).fetch_all(&state.pool).await?;
        if rows.is_empty() {
            break;
        }
        for (ca, version, due) in rows {
            cursor = (due, ca.clone(), version);
            let mut tx = db::begin_write(&state.pool).await?;
            let deadline = retention_deadline(&mut tx, &ca, version).await?;
            if deadline > Utc::now() {
                sqlx::query(
                "UPDATE mtls_trust_cleanup SET next_action_at=$3 WHERE ca_id=$1 AND ca_version=$2",
            )
            .bind(&ca)
            .bind(version)
            .bind(deadline)
            .execute(&mut *tx)
            .await?;
                tx.commit().await?;
                continue;
            }
            let nodes:Vec<String>=sqlx::query_scalar("SELECT node_id FROM mtls_node_trust WHERE ca_id=$1 AND ca_version=$2 AND deployed UNION SELECT node_id FROM mtls_live_activation_trust WHERE ca_id=$1 AND ca_version=$2 ORDER BY node_id").bind(&ca).bind(version).fetch_all(&mut *tx).await?;
            for node in &nodes {
                sqlx::query("INSERT INTO mtls_cleanup_nodes(ca_id,ca_version,node_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(&ca).bind(version).bind(node).execute(&mut *tx).await?;
                let prior:Option<(String,String)>=sqlx::query_as("SELECT j.id,j.status FROM mtls_cleanup_nodes c JOIN jobs j ON j.id=c.job_id WHERE c.ca_id=$1 AND c.ca_version=$2 AND c.node_id=$3").bind(&ca).bind(version).bind(node).fetch_optional(&mut *tx).await?;
                if let Some((job, status)) = prior
                    && (matches!(
                        status.as_str(),
                        "queued" | "running" | "waiting_confirmation"
                    ) || !operation_stale_on(&mut tx, &job).await?)
                {
                    continue;
                }
                let ready: bool =
                    sqlx::query_scalar("SELECT mtls_state='ready' FROM nodes WHERE id=$1")
                        .bind(node)
                        .fetch_one(&mut *tx)
                        .await?;
                if !ready {
                    continue;
                }
                let job = enqueue_node(&mut tx, node, &ca, version, None, "cleanup").await?;
                sqlx::query("UPDATE mtls_cleanup_nodes SET job_id=$4,completed=FALSE,error_message=NULL WHERE ca_id=$1 AND ca_version=$2 AND node_id=$3").bind(&ca).bind(version).bind(node).bind(job).execute(&mut *tx).await?;
            }
            if nodes.is_empty() {
                sqlx::query(
                "UPDATE mtls_trust_cleanup SET completed_at=$3 WHERE ca_id=$1 AND ca_version=$2",
            )
            .bind(&ca)
            .bind(version)
            .bind(Utc::now())
            .execute(&mut *tx)
            .await?;
            }
            tx.commit().await?;
        }
    }
    Ok(())
}

pub struct JobContext {
    pub trust: Vec<TrustVersion>,
    pub probe_version: i64,
    pub skip_users: bool,
    pub recovery: bool,
}

pub async fn validate_job_on(connection: &mut sqlx::PgConnection, job: &JobInput) -> Result<bool> {
    if job.payload["mtls_operation"] != true {
        return Ok(false);
    }
    let row=sqlx::query("SELECT o.*,n.desired_revision AS current_desired,n.deployed_revision AS current_base,n.mtls_activation_id AS current_activation,n.deployed_content_sha256 AS current_digest,g.state AS generation_state,r.phase AS rotation_phase,c.completed_at AS cleanup_completed FROM mtls_node_operations o JOIN nodes n ON n.id=o.node_id JOIN mtls_ca_generations g ON g.ca_id=o.ca_id AND g.ca_version=o.ca_version LEFT JOIN mtls_rotations r ON r.id=o.rotation_id LEFT JOIN mtls_trust_cleanup c ON c.ca_id=o.ca_id AND c.ca_version=o.ca_version WHERE o.job_id=$1 AND o.node_id=$2")
        .bind(&job.id).bind(&job.node_id).fetch_optional(&mut *connection).await?.context("managed node operation is missing or its node was removed")?;
    let stage: String = row.get("stage");
    if stage == "trust" {
        ensure!(
            row.get::<String, _>("generation_state") == "preparing"
                && row
                    .get::<Option<String>, _>("rotation_phase")
                    .is_some_and(|p| matches!(p.as_str(), "trust" | "identities" | "publish")),
            "trust operation phase was superseded"
        );
    } else {
        ensure!(
            row.get::<String, _>("generation_state") == "retired"
                && row
                    .get::<Option<DateTime<Utc>>, _>("cleanup_completed")
                    .is_none(),
            "cleanup generation is no longer pending"
        );
        ensure!(
            retention_deadline(
                connection,
                &row.get::<String, _>("ca_id"),
                row.get("ca_version")
            )
            .await?
                <= Utc::now(),
            "historical certificates still require the old trust anchor"
        );
    }
    let activation:Option<String>=sqlx::query_scalar("SELECT id FROM mtls_activations WHERE job_id=$1 AND phase IN ('activating','verified','recovery_required')").bind(&job.id).fetch_optional(&mut *connection).await?;
    if activation.is_none() {
        ensure!(
            row.get::<Option<i64>, _>("base_revision") == row.get::<Option<i64>, _>("current_base")
                && row.get::<Option<String>, _>("base_activation_id")
                    == row.get::<Option<String>, _>("current_activation")
                && row.get::<Option<String>, _>("base_digest")
                    == row.get::<Option<String>, _>("current_digest"),
            "managed node operation deployment baseline changed"
        );
        if row.get::<bool, _>("use_desired") {
            ensure!(
                row.get::<i64, _>("desired_revision") == row.get::<i64, _>("current_desired"),
                "managed node operation desired revision changed"
            );
        }
    } else {
        ensure!(
            activation == row.get::<Option<String>, _>("current_activation"),
            "managed node activation was superseded"
        );
    }
    ensure!(
        members_on(connection, &row.get::<String, _>("ca_id"))
            .await?
            .contains(&row.get::<String, _>("node_id")),
        "node no longer participates in this CA identity"
    );
    Ok(true)
}

pub async fn configure_job(
    pool: &PgPool,
    secrets: &SecretBox,
    job: &JobInput,
    snapshot: &mut Value,
) -> Result<Option<JobContext>> {
    if !validate_job_on(&mut *pool.acquire().await?, job).await? {
        return Ok(None);
    }
    let row=sqlx::query("SELECT o.*,a.published_version,g.expires_at AS published_expires,EXISTS(SELECT 1 FROM mtls_node_trust t WHERE t.node_id=o.node_id AND t.ca_id=o.ca_id AND t.deployed) AS deployed_ca FROM mtls_node_operations o JOIN mtls_authorities a ON a.credential_id=o.ca_id JOIN mtls_ca_generations g ON g.ca_id=a.credential_id AND g.ca_version=a.published_version WHERE o.job_id=$1").bind(&job.id).fetch_one(pool).await?;
    let stage: String = row.get("stage");
    let ca: String = row.get("ca_id");
    let published: i64 = row.get("published_version");
    let persisted:Option<String>=sqlx::query_scalar("SELECT snapshot_enc FROM mtls_activations WHERE job_id=$1 AND phase IN ('activating','verified','recovery_required')").bind(&job.id).fetch_optional(pool).await?;
    if let Some(cipher) = persisted {
        *snapshot = serde_json::from_str(&secrets.decrypt(&cipher)?)?;
    } else if !row.get::<bool, _>("use_desired")
        && let Some(activation) = row.get::<Option<String>, _>("base_activation_id")
    {
        let cipher: String = sqlx::query_scalar(
            "SELECT snapshot_enc FROM mtls_activations WHERE id=$1 AND phase='committed'",
        )
        .bind(activation)
        .fetch_one(pool)
        .await?;
        *snapshot = serde_json::from_str(&secrets.decrypt(&cipher)?)?;
    }
    let options = snapshot
        .get_mut("server_config")
        .context("managed snapshot config missing")?;
    let policy = Settings::from_config(options)?;
    ensure!(
        policy.authority().is_some_and(|(id, _)| id == ca),
        "managed operation no longer matches its node policy"
    );
    options["mtls"]["ca_credential_version"] = json!(published);
    let policy = Settings::from_config(options)?;
    let mut trust = super::node_mtls::desired_trust(
        pool,
        job.node_id.as_deref().context("node required")?,
        &policy,
    )
    .await?;
    if stage == "cleanup" {
        let old: i64 = row.get("ca_version");
        trust.retain(|v| v.version != old);
    }
    let existing = super::activation::persisted_trust(pool, &job.id).await?;
    if stage == "cleanup" && existing.as_ref().is_some_and(|old| old != &trust) {
        anyhow::bail!("cleanup trust set changed; recover and retry with a fresh operation");
    }
    let version = if stage == "trust" {
        row.get("ca_version")
    } else {
        published
    };
    sqlx::query("UPDATE mtls_node_operations SET effective_version=$2 WHERE job_id=$1")
        .bind(&job.id)
        .bind(published)
        .execute(pool)
        .await?;
    Ok(Some(JobContext {
        trust,
        probe_version: version,
        recovery: row.get("recovery"),
        skip_users: stage == "trust"
            && (row.get::<bool, _>("deployed_ca")
                || row.get::<bool, _>("recovery")
                    && row.get::<DateTime<Utc>, _>("published_expires")
                        <= Utc::now() + Duration::minutes(5)),
    }))
}

async fn update_node_metadata(
    tx: &mut Transaction<'_, Postgres>,
    secrets: &SecretBox,
    ca: &str,
    version: i64,
) -> Result<()> {
    let nodes = members_on(tx, ca).await?;
    for node in nodes {
        let row = sqlx::query("SELECT * FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&mut **tx)
            .await?;
        let desired: i64 = row.get("desired_revision");
        let mut desired_options: Value =
            serde_json::from_str(&secrets.decrypt(&row.get::<String, _>("desired_config_enc"))?)?;
        let desired_matches = Settings::from_config(&desired_options)?
            .authority()
            .is_some_and(|(id, _)| id == ca);
        let live = row
            .get::<Option<String>, _>("deployed_config_enc")
            .map(|v| {
                secrets
                    .decrypt(&v)
                    .and_then(|v| Ok(serde_json::from_str::<Value>(&v)?))
            })
            .transpose()?;
        let live_matches = live.as_ref().is_some_and(|v| {
            Settings::from_config(v)
                .ok()
                .and_then(|p| p.authority().map(|(id, _)| id.to_owned()))
                .as_deref()
                == Some(ca)
        });
        if desired_matches {
            desired_options["mtls"]["ca_credential_version"] = json!(version);
        }
        let mut next = desired;
        if live_matches {
            next += 1;
            let activation: String = row
                .get::<Option<String>, _>("mtls_activation_id")
                .context("published node proof is missing")?;
            let cipher: String = sqlx::query_scalar(
                "SELECT snapshot_enc FROM mtls_activations WHERE id=$1 AND phase='committed'",
            )
            .bind(&activation)
            .fetch_one(&mut **tx)
            .await?;
            let mut snapshot: Value = serde_json::from_str(&secrets.decrypt(&cipher)?)?;
            snapshot["server_config"]["mtls"]["ca_credential_version"] = json!(version);
            save_snapshot(tx, secrets, &node, next, &snapshot, true).await?;
            sqlx::query("INSERT INTO mtls_deployment_snapshots(node_id,revision,activation_id,snapshot_enc,created_at) VALUES($1,$2,$3,$4,$5)")
                .bind(&node).bind(next).bind(&activation).bind(secrets.encrypt(&snapshot.to_string())?).bind(Utc::now()).execute(&mut **tx).await?;
            sqlx::query("UPDATE nodes SET deployed_revision=$2,deployed_config_enc=$3 WHERE id=$1")
                .bind(&node)
                .bind(next)
                .bind(secrets.encrypt(&snapshot["server_config"].to_string())?)
                .execute(&mut **tx)
                .await?;
        }
        if desired_matches || live_matches {
            if !live_matches || row.get::<Option<i64>, _>("deployed_revision") != Some(desired) {
                next += 1;
                let cipher: String = sqlx::query_scalar(
                    "SELECT config_enc FROM config_versions WHERE node_id=$1 AND revision=$2",
                )
                .bind(&node)
                .bind(desired)
                .fetch_one(&mut **tx)
                .await?;
                let mut snapshot: Value = serde_json::from_str(&secrets.decrypt(&cipher)?)?;
                snapshot["server_config"] = desired_options.clone();
                save_snapshot(tx, secrets, &node, next, &snapshot, false).await?;
            }
            sqlx::query("UPDATE nodes SET desired_revision=$2,desired_config_enc=$3,updated_at=$4 WHERE id=$1").bind(&node).bind(next).bind(secrets.encrypt(&desired_options.to_string())?).bind(Utc::now()).execute(&mut **tx).await?;
            super::node_mtls::save_target(
                tx,
                &node,
                next,
                &Settings::from_config(&desired_options)?,
            )
            .await?;
        }
        super::reference_index::flush_node_on(tx, secrets, &node).await?;
    }
    Ok(())
}

async fn save_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    secrets: &SecretBox,
    node: &str,
    revision: i64,
    snapshot: &Value,
    deployed: bool,
) -> Result<()> {
    let plain = snapshot.to_string();
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,deployed_success,created_at) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(Uuid::new_v4().to_string()).bind(node).bind(revision).bind(secrets.encrypt(&plain)?).bind(hex::encode(Sha256::digest(plain.as_bytes()))).bind(deployed).bind(Utc::now()).execute(&mut **tx).await?;
    Ok(())
}

pub async fn operation_completed(tx: &mut Transaction<'_, Postgres>, job: &JobInput) -> Result<()> {
    if job.payload["mtls_operation"] != true {
        return Ok(());
    }
    let row = sqlx::query("SELECT * FROM mtls_node_operations WHERE job_id=$1")
        .bind(&job.id)
        .fetch_one(&mut **tx)
        .await?;
    let node: String = row.get("node_id");
    let ca: String = row.get("ca_id");
    let version: i64 = row.get("ca_version");
    let stage: String = row.get("stage");
    if stage == "trust" {
        let proof = proof_on(tx, &node, &ca, version).await?;
        ensure!(
            proof,
            "new CA trust was not proven by the target-generation probe"
        );
        sqlx::query("UPDATE mtls_rotation_nodes SET trust_prepared=TRUE,error_message=NULL WHERE rotation_id=$1 AND node_id=$2 AND job_id=$3").bind(row.get::<Option<String>,_>("rotation_id")).bind(&node).bind(&job.id).execute(&mut **tx).await?;
    } else {
        let remains:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_node_trust WHERE node_id=$1 AND ca_id=$2 AND ca_version=$3 AND deployed)").bind(&node).bind(&ca).bind(version).fetch_one(&mut **tx).await?;
        ensure!(!remains, "old CA trust was not actually removed");
        sqlx::query("UPDATE mtls_cleanup_nodes SET completed=TRUE,error_message=NULL WHERE ca_id=$1 AND ca_version=$2 AND node_id=$3 AND job_id=$4").bind(ca).bind(version).bind(node).bind(&job.id).execute(&mut **tx).await?;
    }
    Ok(())
}

pub async fn retry_node_job(
    tx: &mut Transaction<'_, Postgres>,
    original: &str,
    expected: i64,
) -> Result<String> {
    let row=sqlx::query("SELECT o.*,n.desired_revision AS current_revision,n.mtls_state FROM mtls_node_operations o JOIN nodes n ON n.id=o.node_id WHERE o.job_id=$1").bind(original).fetch_one(&mut **tx).await?;
    ensure!(
        row.get::<i64, _>("current_revision") == expected,
        "node changed; reload before retrying"
    );
    ensure!(
        row.get::<String, _>("mtls_state") == "ready",
        "recover the node activation before retrying this CA operation"
    );
    let node: String = row.get("node_id");
    let ca: String = row.get("ca_id");
    let version: i64 = row.get("ca_version");
    let stage: String = row.get("stage");
    let rotation: Option<String> = row.get("rotation_id");
    ensure!(
        members_on(tx, &ca).await?.contains(&node),
        "node no longer uses this CA identity"
    );
    if let Some(id) = &rotation {
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_rotations WHERE id=$1 AND phase IN ('trust','identities','publish'))").bind(id).fetch_one(&mut **tx).await?;
        ensure!(valid, "rotation phase already completed");
    }
    let child = enqueue_node(tx, &node, &ca, version, rotation.as_deref(), &stage).await?;
    sqlx::query("UPDATE jobs SET retry_of_job_id=$2 WHERE id=$1")
        .bind(&child)
        .bind(original)
        .execute(&mut **tx)
        .await?;
    if let Some(id) = rotation {
        sqlx::query("UPDATE mtls_rotation_nodes SET job_id=$3,error_message=NULL,trust_prepared=FALSE WHERE rotation_id=$1 AND node_id=$2").bind(&id).bind(&node).bind(&child).execute(&mut **tx).await?;
        sqlx::query("UPDATE mtls_rotations SET next_action_at=$2,error_message=NULL WHERE id=$1")
            .bind(id)
            .bind(Utc::now())
            .execute(&mut **tx)
            .await?;
    } else {
        sqlx::query("UPDATE mtls_cleanup_nodes SET job_id=$4,completed=FALSE,error_message=NULL WHERE ca_id=$1 AND ca_version=$2 AND node_id=$3").bind(ca).bind(version).bind(node).bind(&child).execute(&mut **tx).await?;
    }
    Ok(child)
}

/// Public time boundaries for blocked rotation warnings. Renewable current-
/// generation leaves use the issuer boundary; retired compatible leaves use
/// their actual expiry because they cannot be extended.
pub async fn status_on(connection: &mut sqlx::PgConnection, id: &str) -> Result<Value> {
    let row=sqlx::query("WITH selected AS (SELECT DISTINCT ON(t.node_id,i.user_id) t.node_id,i.user_id,v.expires_at,g.expires_at AS issuer_expires,v.ca_version,a.published_version FROM mtls_rotation_nodes m JOIN mtls_rotations r ON r.id=m.rotation_id JOIN mtls_node_trust t ON t.node_id=m.node_id AND t.ca_id=r.ca_id AND t.deployed JOIN node_assignments na ON na.node_id=t.node_id JOIN users u ON u.id=na.user_id AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$2) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes) JOIN mtls_user_identities i ON i.user_id=u.id AND i.ca_id=r.ca_id AND NOT i.archived JOIN mtls_user_versions v ON v.identity_id=i.id AND v.ca_version=t.ca_version AND v.released AND NOT v.discarded JOIN mtls_ca_generations g ON g.ca_id=v.ca_id AND g.ca_version=v.ca_version JOIN mtls_authorities a ON a.credential_id=r.ca_id WHERE r.id=$1 ORDER BY t.node_id,i.user_id,(v.not_before<=$2 AND v.expires_at>$2) DESC,(v.ca_version=a.published_version) DESC,v.ca_version DESC,v.credential_version DESC),probes AS(SELECT p.expires_at,g.expires_at AS issuer_expires,p.ca_version,a.published_version FROM mtls_rotation_nodes m JOIN mtls_rotations r ON r.id=m.rotation_id JOIN mtls_active_probes active ON active.node_id=m.node_id AND active.ca_id=r.ca_id JOIN mtls_probe_versions p ON p.node_id=active.node_id AND p.credential_id=active.credential_id AND p.credential_version=active.credential_version JOIN mtls_ca_generations g ON g.ca_id=p.ca_id AND g.ca_version=p.ca_version JOIN mtls_authorities a ON a.credential_id=r.ca_id WHERE r.id=$1) SELECT r.phase,r.ca_version,coalesce(r.previous_version,r.ca_version-1) AS previous_version,g.expires_at AS old_ca_expires,(SELECT min(expires_at) FROM selected) AS client_expires,(SELECT min(expires_at) FROM probes) AS probe_expires,least(g.expires_at-interval '5 minutes',(SELECT min(CASE WHEN ca_version=published_version THEN issuer_expires-interval '5 minutes' ELSE expires_at END) FROM selected),(SELECT min(CASE WHEN ca_version=published_version THEN issuer_expires-interval '5 minutes' ELSE expires_at END) FROM probes)) AS deadline,coalesce((SELECT jsonb_agg(node_id ORDER BY node_id) FROM mtls_rotation_nodes WHERE rotation_id=r.id AND (NOT trust_prepared OR r.phase='probes' AND NOT probe_switched)),'[]'::jsonb) AS blocked_nodes,(SELECT count(*) FROM mtls_user_identities i JOIN users u ON u.id=i.user_id WHERE i.ca_id=r.ca_id AND NOT i.archived AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$2) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes)) AS eligible_users,(SELECT count(DISTINCT v.identity_id) FROM mtls_user_versions v JOIN mtls_user_identities i ON i.id=v.identity_id JOIN users u ON u.id=i.user_id WHERE v.ca_id=r.ca_id AND v.ca_version=r.ca_version AND NOT i.archived AND NOT v.discarded AND v.expires_at>$2 AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$2) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes)) AS prepared_users FROM mtls_rotations r JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=coalesce(r.previous_version,r.ca_version-1) WHERE r.id=$1")
        .bind(id).bind(Utc::now()).fetch_one(connection).await?;
    let deadline: DateTime<Utc> = row.get("deadline");
    Ok(
        json!({"previous_version":row.get::<i64,_>("previous_version"),"old_ca_expires_at":row.get::<DateTime<Utc>,_>("old_ca_expires"),"client_certificate_expires_at":row.get::<Option<DateTime<Utc>>,_>("client_expires"),"probe_certificate_expires_at":row.get::<Option<DateTime<Utc>>,_>("probe_expires"),"deadline":deadline,"days_remaining":(deadline-Utc::now()).num_seconds().div_euclid(86400),"blocked_node_ids":row.get::<Value,_>("blocked_nodes"),"eligible_user_count":row.get::<i64,_>("eligible_users"),"prepared_user_count":row.get::<i64,_>("prepared_users")}),
    )
}

pub async fn notification_credentials_on(
    connection: &mut sqlx::PgConnection,
) -> Result<Vec<Value>> {
    let rows=sqlx::query("SELECT r.id AS rotation_id,c.id AS credential_id,r.ca_version,c.name,c.revision,c.created_at,c.updated_at FROM mtls_rotations r JOIN credentials c ON c.id=r.ca_id WHERE r.phase<>'complete' AND r.error_message IS NOT NULL AND NOT c.archived ORDER BY r.id").fetch_all(&mut *connection).await?;
    let mut output = Vec::new();
    for row in rows {
        let status = status_on(connection, &row.get::<String, _>("rotation_id")).await?;
        let days = status["days_remaining"]
            .as_i64()
            .context("rotation deadline is missing")?;
        if days > 30 {
            continue;
        }
        output.push(json!({"id":row.get::<String,_>("credential_id"),"name":row.get::<String,_>("name"),"kind":"ca_identity","owner_user_id":null,"revision":row.get::<i64,_>("revision"),"latest_version":row.get::<i64,_>("ca_version"),"archived":false,"expires_at":status["deadline"],"days_remaining":days,"status":if days<0{"expired"}else{"expiring"},"metadata":{"rotation_blocked":true,"rotation_id":row.get::<String,_>("rotation_id"),"rotation_status":status},"created_at":row.get::<DateTime<Utc>,_>("created_at"),"updated_at":row.get::<DateTime<Utc>,_>("updated_at")}));
    }
    Ok(output)
}

/// Explicit force-sync may recover a CA operation, but only a known database
/// baseline or an exact persisted pending marker may be replaced remotely.
pub async fn enqueue_explicit_recovery(
    tx: &mut Transaction<'_, Postgres>,
    secrets: &SecretBox,
    node: &str,
) -> Result<Option<String>> {
    let row =
        sqlx::query("SELECT desired_revision,desired_config_enc,mtls_state FROM nodes WHERE id=$1")
            .bind(node)
            .fetch_one(&mut **tx)
            .await?;
    let options: Value =
        serde_json::from_str(&secrets.decrypt(&row.get::<String, _>("desired_config_enc"))?)?;
    let settings = Settings::from_config(&options)?;
    let Some((ca, _)) = settings.authority() else {
        return Ok(None);
    };
    let rotation=sqlx::query("SELECT r.id,r.ca_version,g.expires_at,a.published_version,p.expires_at AS published_expires FROM mtls_rotations r JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version JOIN mtls_authorities a ON a.credential_id=r.ca_id JOIN mtls_ca_generations p ON p.ca_id=a.credential_id AND p.ca_version=a.published_version WHERE r.ca_id=$1 AND r.phase IN ('trust','identities','publish') AND g.state='preparing'").bind(ca).fetch_optional(&mut **tx).await?;
    let Some(rotation) = rotation else {
        return Ok(None);
    };
    if row.get::<String, _>("mtls_state") == "ready"
        && rotation.get::<DateTime<Utc>, _>("published_expires") > Utc::now() + Duration::minutes(5)
    {
        return Ok(None);
    }
    ensure!(
        rotation.get::<DateTime<Utc>, _>("expires_at") > Utc::now(),
        "target CA expired; publish a valid replacement version first"
    );
    let id: String = rotation.get("id");
    let version: i64 = rotation.get("ca_version");
    let job = enqueue_node(tx, node, ca, version, Some(&id), "trust").await?;
    sqlx::query("UPDATE mtls_node_operations SET recovery=TRUE,use_desired=TRUE WHERE job_id=$1")
        .bind(&job)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE jobs SET target_revision=$2 WHERE id=$1")
        .bind(&job)
        .bind(row.get::<i64, _>("desired_revision"))
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE mtls_rotation_nodes SET job_id=$3,error_message=NULL,trust_prepared=FALSE WHERE rotation_id=$1 AND node_id=$2").bind(id).bind(node).bind(&job).execute(&mut **tx).await?;
    Ok(Some(job))
}

pub async fn recovery_marker(pool: &PgPool, job: &str) -> Result<Option<Value>> {
    let row=sqlx::query("SELECT a.id,a.generation,a.target_digest FROM mtls_node_operations o JOIN mtls_activations a ON a.id=o.base_activation_id WHERE o.job_id=$1 AND o.recovery AND a.phase IN ('activating','verified','recovery_required')").bind(job).fetch_optional(pool).await?;
    Ok(row.map(|row|json!({"activation_id":row.get::<String,_>("id"),"generation":row.get::<i64,_>("generation"),"config_sha256":row.get::<String,_>("target_digest")})))
}

async fn operation_stale_on(connection: &mut sqlx::PgConnection, job: &str) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT o.base_activation_id IS DISTINCT FROM n.mtls_activation_id OR o.base_revision IS DISTINCT FROM n.deployed_revision OR o.base_digest IS DISTINCT FROM n.deployed_content_sha256 OR o.use_desired AND o.desired_revision<>n.desired_revision OR o.effective_version IS NOT NULL AND o.effective_version<>a.published_version FROM mtls_node_operations o JOIN nodes n ON n.id=o.node_id JOIN mtls_authorities a ON a.credential_id=o.ca_id WHERE o.job_id=$1").bind(job).fetch_optional(connection).await?.unwrap_or(false))
}

pub async fn maintenance_notifications_on(
    connection: &mut sqlx::PgConnection,
) -> Result<Vec<Value>> {
    let rows=sqlx::query("SELECT c.id,c.name,c.revision,c.created_at,c.updated_at,a.published_version,coalesce(min(v.expires_at),min(i.created_at)) AS deadline,min(i.maintenance_error) AS reason,count(DISTINCT i.id) AS affected FROM mtls_user_identities i JOIN users u ON u.id=i.user_id AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$1) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes) JOIN mtls_authorities a ON a.credential_id=i.ca_id JOIN credentials c ON c.id=i.ca_id AND NOT c.archived LEFT JOIN LATERAL(SELECT expires_at FROM mtls_user_versions WHERE identity_id=i.id AND ca_version=a.published_version AND released AND NOT discarded ORDER BY credential_version DESC LIMIT 1) v ON TRUE WHERE NOT i.archived AND i.maintenance_error IS NOT NULL GROUP BY c.id,a.published_version ORDER BY c.id")
        .bind(Utc::now()).fetch_all(connection).await?;
    let mut output = Vec::new();
    for row in rows {
        let deadline: DateTime<Utc> = row.get("deadline");
        let days = (deadline - Utc::now()).num_seconds().div_euclid(86400);
        if days > 30 {
            continue;
        }
        output.push(json!({"id":row.get::<String,_>("id"),"name":row.get::<String,_>("name"),"kind":"ca_identity","owner_user_id":null,"revision":row.get::<i64,_>("revision"),"latest_version":row.get::<i64,_>("published_version"),"archived":false,"expires_at":deadline,"days_remaining":days,"status":if days<0{"expired"}else{"expiring"},"metadata":{"maintenance_failed":true,"maintenance_error":row.get::<Option<String>,_>("reason"),"affected_user_count":row.get::<i64,_>("affected")},"created_at":row.get::<DateTime<Utc>,_>("created_at"),"updated_at":row.get::<DateTime<Utc>,_>("updated_at")}));
    }
    Ok(output)
}
