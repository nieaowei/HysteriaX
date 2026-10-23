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

mod cleanup;
mod metadata;
mod probes;
mod publication;
mod release;
pub(crate) mod superseded;
mod trust;
mod users;

pub(super) async fn run_preparation(state: AppState) {
    tokio::join!(
        trust::run(state.clone()),
        users::run(state.clone()),
        publication::run(state.clone()),
        metadata::run(state.clone()),
        release::run(state.clone()),
        probes::run(state.clone()),
        superseded::run(state.clone()),
        cleanup::run(state)
    );
}

#[cfg(test)]
pub async fn members_on(connection: &mut sqlx::PgConnection, ca: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT DISTINCT node_id FROM (SELECT node_id FROM mtls_node_trust WHERE ca_id=$1 AND (desired OR deployed) UNION SELECT node_id FROM mtls_live_activation_trust WHERE ca_id=$1) members ORDER BY node_id").bind(ca).fetch_all(connection).await?)
}

async fn member_on(connection: &mut sqlx::PgConnection, node: &str, ca: &str) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_node_trust WHERE node_id=$1 AND ca_id=$2 AND (desired OR deployed) UNION ALL SELECT 1 FROM mtls_live_activation_trust WHERE node_id=$1 AND ca_id=$2)")
        .bind(node).bind(ca).fetch_one(connection).await?)
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

/// One bounded attempt per stage, shared with the independent workers. All
/// mutations and error persistence belong to claimed work and its resources.
pub async fn run_pass(state: &AppState) -> Result<()> {
    trust::scheduled_pass(state).await?;
    users::scheduled_pass(state).await?;
    release::scheduled_pass(state).await?;
    publication::scheduled_pass(state).await?;
    metadata::scheduled_pass(state).await?;
    probes::scheduled_pass(state).await?;
    superseded::scheduled_pass(state).await?;
    cleanup::scheduled_pass(state).await?;
    Ok(())
}

#[cfg(test)]
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
        publication::pass(state, id).await?;
        metadata::pass(state, id).await?;
        if metadata::pending_on(&state.pool, id).await? {
            return Ok(());
        }
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

#[cfg(test)]
async fn publish_ready(state: &AppState, id: &str, ca: &str, version: i64) -> Result<()> {
    if release::pass(state, id, ca, version).await? {
        publication::pass(state, id).await?;
        metadata::pass(state, id).await?;
    }
    Ok(())
}

#[cfg(test)]
async fn switch_probes(state: &AppState, id: &str, ca: &str, version: i64) -> Result<()> {
    probes::pass(state, id, ca, version).await
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

const RETENTION_DEADLINE: &str = "SELECT coalesce(greatest((SELECT expires_at FROM mtls_user_versions WHERE ca_id=$1 AND ca_version=$2 ORDER BY expires_at DESC LIMIT 1),(SELECT expires_at FROM mtls_probe_versions WHERE ca_id=$1 AND ca_version=$2 ORDER BY expires_at DESC LIMIT 1))+interval '5 minutes',clock_timestamp())";

pub(crate) async fn retention_deadline(
    connection: &mut sqlx::PgConnection,
    ca: &str,
    version: i64,
) -> Result<DateTime<Utc>> {
    // Each existing CA/expiry index contributes at most one row, including
    // archived/discarded leaves whose private history still protects retention.
    Ok(sqlx::query_scalar(RETENTION_DEADLINE)
        .bind(ca)
        .bind(version)
        .fetch_one(connection)
        .await?)
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
            cleanup::retention_due_on(
                connection,
                &row.get::<String, _>("ca_id"),
                row.get("ca_version")
            )
            .await?
            .0,
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
        member_on(
            connection,
            &row.get::<String, _>("node_id"),
            &row.get::<String, _>("ca_id")
        )
        .await?,
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
        let remains = cleanup::member_on(tx, &node, &ca, version).await?;
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
        member_on(tx, &node, &ca).await?,
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
    let row=sqlx::query("WITH selected AS (SELECT DISTINCT ON(t.node_id,i.user_id) t.node_id,i.user_id,v.expires_at,g.expires_at AS issuer_expires,v.ca_version,a.published_version FROM mtls_rotation_nodes m JOIN mtls_rotations r ON r.id=m.rotation_id JOIN mtls_node_trust t ON t.node_id=m.node_id AND t.ca_id=r.ca_id AND t.deployed JOIN node_assignments na ON na.node_id=t.node_id JOIN users u ON u.id=na.user_id AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$2) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes) JOIN mtls_user_identities i ON i.user_id=u.id AND i.ca_id=r.ca_id AND NOT i.archived JOIN mtls_user_version_publication v ON v.identity_id=i.id AND v.ca_version=t.ca_version AND v.released AND NOT v.discarded JOIN mtls_ca_generations g ON g.ca_id=v.ca_id AND g.ca_version=v.ca_version JOIN mtls_authorities a ON a.credential_id=r.ca_id WHERE r.id=$1 ORDER BY t.node_id,i.user_id,(v.not_before<=$2 AND v.expires_at>$2) DESC,(v.ca_version=a.published_version) DESC,v.ca_version DESC,v.credential_version DESC),probes AS(SELECT p.expires_at,g.expires_at AS issuer_expires,p.ca_version,a.published_version FROM mtls_rotation_nodes m JOIN mtls_rotations r ON r.id=m.rotation_id JOIN mtls_active_probes active ON active.node_id=m.node_id AND active.ca_id=r.ca_id JOIN mtls_probe_versions p ON p.node_id=active.node_id AND p.credential_id=active.credential_id AND p.credential_version=active.credential_version JOIN mtls_ca_generations g ON g.ca_id=p.ca_id AND g.ca_version=p.ca_version JOIN mtls_authorities a ON a.credential_id=r.ca_id WHERE r.id=$1) SELECT r.phase,r.ca_version,coalesce(r.previous_version,r.ca_version-1) AS previous_version,g.expires_at AS old_ca_expires,(SELECT min(expires_at) FROM selected) AS client_expires,(SELECT min(expires_at) FROM probes) AS probe_expires,least(g.expires_at-interval '5 minutes',(SELECT min(CASE WHEN ca_version=published_version THEN issuer_expires-interval '5 minutes' ELSE expires_at END) FROM selected),(SELECT min(CASE WHEN ca_version=published_version THEN issuer_expires-interval '5 minutes' ELSE expires_at END) FROM probes)) AS deadline,coalesce((SELECT jsonb_agg(node_id ORDER BY node_id) FROM mtls_rotation_nodes WHERE rotation_id=r.id AND (NOT trust_prepared OR r.phase='probes' AND NOT probe_switched)),'[]'::jsonb) AS blocked_nodes,(SELECT count(*) FROM mtls_user_identities i JOIN users u ON u.id=i.user_id WHERE i.ca_id=r.ca_id AND NOT i.archived AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$2) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes)) AS eligible_users,(SELECT count(DISTINCT v.identity_id) FROM mtls_user_version_publication v JOIN mtls_user_identities i ON i.id=v.identity_id JOIN users u ON u.id=i.user_id WHERE v.ca_id=r.ca_id AND v.ca_version=r.ca_version AND NOT i.archived AND NOT v.discarded AND v.expires_at>$2 AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$2) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes)) AS prepared_users FROM mtls_rotations r JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=coalesce(r.previous_version,r.ca_version-1) WHERE r.id=$1")
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
    let rows=sqlx::query("SELECT c.id,c.name,c.revision,c.created_at,c.updated_at,a.published_version,coalesce(min(v.expires_at),min(i.created_at)) AS deadline,min(i.maintenance_error) AS reason,count(DISTINCT i.id) AS affected FROM mtls_user_identities i JOIN users u ON u.id=i.user_id AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$1) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes) JOIN mtls_authorities a ON a.credential_id=i.ca_id JOIN credentials c ON c.id=i.ca_id AND NOT c.archived LEFT JOIN LATERAL(SELECT expires_at FROM mtls_user_version_publication WHERE identity_id=i.id AND ca_version=a.published_version AND released AND NOT discarded ORDER BY credential_version DESC LIMIT 1) v ON TRUE WHERE NOT i.archived AND i.maintenance_error IS NOT NULL GROUP BY c.id,a.published_version ORDER BY c.id")
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

#[cfg(test)]
#[path = "rotations/coordinator_tests.rs"]
mod coordinator_tests;
