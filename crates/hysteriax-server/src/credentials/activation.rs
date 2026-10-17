//! A database/remote activation barrier. An uncertain remote switch remains
//! inaccessible until its immutable marker and actual configuration agree.
use super::node_mtls::{Settings, TrustVersion};
use crate::{db, security::SecretBox};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Record {
    pub id: String,
    pub generation: i64,
    pub target_digest: String,
    pub previous_digest: Option<String>,
    pub phase: String,
    pub fresh: bool,
}

pub async fn historical_trust(
    pool: &PgPool,
    node: &str,
    revision: i64,
) -> Result<Option<Vec<TrustVersion>>> {
    let trust:Option<Value>=sqlx::query_scalar("SELECT a.trust FROM (SELECT activation_id,created_at FROM mtls_deployment_snapshots WHERE node_id=$1 AND revision=$2 UNION ALL SELECT id,created_at FROM mtls_activations WHERE node_id=$1 AND target_revision=$2 AND phase='committed') history JOIN mtls_activations a ON a.id=history.activation_id WHERE a.phase='committed' ORDER BY history.created_at DESC,a.generation DESC LIMIT 1")
        .bind(node).bind(revision).fetch_optional(pool).await?;
    trust
        .map(serde_json::from_value)
        .transpose()
        .context("invalid historical trust snapshot")
}

pub async fn persisted_trust(pool: &PgPool, job: &str) -> Result<Option<Vec<TrustVersion>>> {
    let trust: Option<Value> = sqlx::query_scalar(
        "SELECT trust FROM mtls_activations WHERE job_id=$1 AND phase<>'rolled_back'",
    )
    .bind(job)
    .fetch_optional(pool)
    .await?;
    trust
        .map(serde_json::from_value)
        .transpose()
        .context("invalid activation trust snapshot")
}

pub struct Begin<'a> {
    pub node: &'a str,
    pub job: &'a str,
    pub revision: i64,
    pub digest: &'a str,
    pub snapshot: &'a Value,
    pub trust: &'a [TrustVersion],
    pub probe: Option<(&'a str, i64)>,
}
pub async fn begin(pool: &PgPool, secrets: &SecretBox, input: Begin<'_>) -> Result<Record> {
    let mut tx = db::begin_write(pool).await?;
    let job = crate::deployment::JobInput {
        id: input.job.into(),
        kind: "deploy".into(),
        node_id: Some(input.node.into()),
        target_revision: Some(input.revision),
        payload: json!({"mtls_operation":sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM mtls_node_operations WHERE job_id=$1)").bind(input.job).fetch_one(&mut *tx).await?}),
        attempts: 1,
    };
    super::rotations::validate_job_on(&mut tx, &job).await?;
    if let Some(row) = sqlx::query("SELECT * FROM mtls_activations WHERE job_id=$1")
        .bind(input.job)
        .fetch_optional(&mut *tx)
        .await?
    {
        ensure!(
            row.get::<String, _>("target_digest") == input.digest,
            "activation target changed; explicit recovery is required"
        );
        let record = read(&row);
        if record.phase == "rolled_back" {
            anyhow::bail!("previous attempt was rolled back; start a new deployment");
        }
        ensure!(
            matches!(
                record.phase.as_str(),
                "activating" | "verified" | "recovery_required"
            ),
            "activation already finished; start a new deployment"
        );
        let changed=sqlx::query("UPDATE nodes SET mtls_state='activating',mtls_error=NULL WHERE id=$1 AND mtls_activation_id=$2").bind(input.node).bind(&record.id).execute(&mut *tx).await?;
        ensure!(
            changed.rows_affected() == 1,
            "activation was superseded; start a new deployment"
        );
        tx.commit().await?;
        return Ok(record);
    }
    let row=sqlx::query("SELECT mtls_state,mtls_generation,mtls_activation_id,deployed_content_sha256 FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed')").bind(input.node).fetch_one(&mut *tx).await?;
    let generation = row.get::<i64, _>("mtls_generation") + 1;
    let id = Uuid::new_v4().to_string();
    let previous: Option<String> = row.get("deployed_content_sha256");
    let trust = serde_json::to_value(input.trust)?;
    sqlx::query("INSERT INTO mtls_activations(id,node_id,job_id,generation,target_revision,phase,previous_state,previous_digest,target_digest,snapshot_enc,trust,probe_credential_id,probe_credential_version,created_at,updated_at,previous_activation_id) VALUES($1,$2,$3,$4,$5,'activating',$6,$7,$8,$9,$10,$11,$12,$13,$13,$14)")
        .bind(&id).bind(input.node).bind(input.job).bind(generation).bind(input.revision).bind(row.get::<String,_>("mtls_state")).bind(&previous).bind(input.digest).bind(secrets.encrypt(&input.snapshot.to_string())?).bind(trust).bind(input.probe.map(|p|p.0)).bind(input.probe.map(|p|p.1)).bind(Utc::now()).bind(row.get::<Option<String>,_>("mtls_activation_id")).execute(&mut *tx).await?;
    for ca in input.trust {
        sqlx::query(
            "INSERT INTO mtls_activation_trust(activation_id,ca_id,ca_version) VALUES($1,$2,$3)",
        )
        .bind(&id)
        .bind(&ca.ca_id)
        .bind(ca.version)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE nodes SET mtls_state='activating',mtls_error=NULL,mtls_generation=$2,mtls_activation_id=$3 WHERE id=$1").bind(input.node).bind(generation).bind(&id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Record {
        id,
        generation,
        target_digest: input.digest.into(),
        previous_digest: previous,
        phase: "activating".into(),
        fresh: true,
    })
}
fn read(row: &sqlx::postgres::PgRow) -> Record {
    Record {
        id: row.get("id"),
        generation: row.get("generation"),
        target_digest: row.get("target_digest"),
        previous_digest: row.get("previous_digest"),
        phase: row.get("phase"),
        fresh: false,
    }
}

pub async fn verified(pool: &PgPool, job: &str) -> Result<()> {
    let mut tx = db::begin_write(pool).await?;
    let changed=sqlx::query("UPDATE mtls_activations a SET phase='verified',updated_at=$2 FROM nodes n WHERE a.job_id=$1 AND n.id=a.node_id AND n.mtls_activation_id=a.id AND n.mtls_generation=a.generation AND a.phase IN ('activating','verified','recovery_required')")
        .bind(job).bind(Utc::now()).execute(&mut *tx).await?;
    ensure!(
        changed.rows_affected() == 1,
        "activation was superseded during remote verification"
    );
    tx.commit().await?;
    Ok(())
}

pub async fn uncertain(pool: &PgPool, job: &str) -> Result<()> {
    let mut tx = db::begin_write(pool).await?;
    sqlx::query("UPDATE mtls_activations SET phase='recovery_required',updated_at=$2 WHERE job_id=$1 AND phase IN ('activating','verified','recovery_required')").bind(job).bind(Utc::now()).execute(&mut *tx).await?;
    sqlx::query("UPDATE nodes n SET mtls_state='recovery_required',mtls_error='Remote activation outcome is unconfirmed; retry deployment or run an explicit recovery sync' FROM mtls_activations a WHERE a.job_id=$1 AND a.id=n.mtls_activation_id AND a.phase='recovery_required'").bind(job).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn rolled_back(pool: &PgPool, job: &str, actual_digest: Option<&str>) -> Result<()> {
    let mut tx = db::begin_write(pool).await?;
    let row = sqlx::query("SELECT * FROM mtls_activations WHERE job_id=$1")
        .bind(job)
        .fetch_one(&mut *tx)
        .await?;
    ensure!(
        row.get::<Option<String>, _>("previous_digest").as_deref() == actual_digest,
        "remote rollback digest does not match the previous deployment"
    );
    sqlx::query("UPDATE mtls_activations SET phase='rolled_back',updated_at=$2 WHERE job_id=$1")
        .bind(job)
        .bind(Utc::now())
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE nodes SET mtls_state=$2,mtls_error=NULL,mtls_activation_id=$3 WHERE mtls_activation_id=$1")
        .bind(row.get::<String, _>("id"))
        .bind(row.get::<String, _>("previous_state"))
        .bind(row.get::<Option<String>,_>("previous_activation_id"))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Called within the exact job-completion transaction that publishes deployed
/// config and connection parameters. There is no earlier subscription release.
pub async fn commit(
    tx: &mut Transaction<'_, Postgres>,
    job: &str,
    digest: &str,
) -> Result<(), sqlx::Error> {
    let Some(row)=sqlx::query("SELECT a.* FROM mtls_activations a JOIN nodes n ON n.mtls_activation_id=a.id AND n.mtls_generation=a.generation WHERE a.job_id=$1 AND a.phase='verified' AND a.target_digest=$2")
        .bind(job).bind(digest).fetch_optional(&mut **tx).await? else {
        return Err(sqlx::Error::Protocol("activation is missing, superseded, or not remotely verified".into()));
    };
    let node: String = row.get("node_id");
    let trust: Vec<TrustVersion> = serde_json::from_value(row.get("trust"))
        .map_err(|_| sqlx::Error::Protocol("invalid activation trust".into()))?;
    sqlx::query("UPDATE mtls_node_trust SET deployed=FALSE,revision=revision+1 WHERE node_id=$1 AND deployed").bind(&node).execute(&mut **tx).await?;
    for ca in &trust {
        sqlx::query("INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,deployed) VALUES($1,$2,$3,TRUE) ON CONFLICT(node_id,ca_id,ca_version) DO UPDATE SET deployed=TRUE,revision=mtls_node_trust.revision+1")
            .bind(&node).bind(&ca.ca_id).bind(ca.version).execute(&mut **tx).await?;
    }
    sqlx::query("UPDATE mtls_activations SET phase='committed',updated_at=$2 WHERE job_id=$1")
        .bind(job)
        .bind(Utc::now())
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE nodes SET mtls_state='ready',mtls_error=NULL WHERE id=$1")
        .bind(&node)
        .execute(&mut **tx)
        .await?;
    super::authorization::reconcile_node(tx, &node, false)
        .await
        .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
    for version in &trust {
        sqlx::query("INSERT INTO mtls_trust_cleanup(ca_id,ca_version,next_action_at) SELECT ca_id,ca_version,$3 FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=$2 AND state='retired' ON CONFLICT(ca_id,ca_version) DO UPDATE SET completed_at=NULL,next_action_at=least(mtls_trust_cleanup.next_action_at,excluded.next_action_at)").bind(&version.ca_id).bind(version.version).bind(Utc::now()).execute(&mut **tx).await?;
    }
    if let Some(first) = trust.first() {
        let published: i64 = sqlx::query_scalar(
            "SELECT published_version FROM mtls_authorities WHERE credential_id=$1",
        )
        .bind(&first.ca_id)
        .fetch_one(&mut **tx)
        .await?;
        super::rotations::switch_probe_on(tx, &node, &first.ca_id, published)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
    } else {
        sqlx::query("DELETE FROM mtls_active_probes WHERE node_id=$1")
            .bind(&node)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("INSERT INTO mtls_deployment_snapshots(node_id,revision,activation_id,snapshot_enc,created_at) SELECT node_id,target_revision,id,snapshot_enc,$2 FROM mtls_activations WHERE job_id=$1 ON CONFLICT DO NOTHING").bind(job).bind(Utc::now()).execute(&mut **tx).await?;
    Ok(())
}

pub fn required(target: &Settings, deployed: Option<&Value>, state: &str) -> bool {
    target.authority().is_some()
        || state != "ready"
        || deployed.is_some_and(|config| {
            Settings::from_config(config)
                .ok()
                .is_some_and(|policy| policy.authority().is_some())
        })
}

pub fn marker(record: &Record) -> Value {
    json!({"activation_id":record.id,"generation":record.generation,"config_sha256":record.target_digest})
}

pub async fn historical_probe(
    pool: &PgPool,
    node: &str,
    revision: i64,
) -> Result<Option<(String, i64)>> {
    let row=sqlx::query("SELECT a.probe_credential_id,a.probe_credential_version,p.expires_at,p.discarded FROM (SELECT activation_id,created_at FROM mtls_deployment_snapshots WHERE node_id=$1 AND revision=$2 UNION ALL SELECT id,created_at FROM mtls_activations WHERE node_id=$1 AND target_revision=$2 AND phase='committed') history JOIN mtls_activations a ON a.id=history.activation_id LEFT JOIN mtls_probe_versions p ON p.credential_id=a.probe_credential_id AND p.credential_version=a.probe_credential_version AND p.node_id=a.node_id WHERE a.phase='committed' ORDER BY history.created_at DESC,a.generation DESC LIMIT 1")
        .bind(node).bind(revision).fetch_optional(pool).await?;
    let Some(row) = row else { return Ok(None) };
    let Some(id) = row.get::<Option<String>, _>("probe_credential_id") else {
        return Ok(None);
    };
    ensure!(
        row.get::<Option<chrono::DateTime<Utc>>, _>("expires_at")
            .is_some_and(|at| at > Utc::now())
            && row.get::<Option<bool>, _>("discarded") == Some(false),
        "rollback unavailable: historical probe identity expired or was discarded; rebuild or sync the node"
    );
    Ok(Some((id, row.get("probe_credential_version"))))
}

pub async fn probe_for_job(pool: &PgPool, job: &str) -> Result<Option<(String, i64)>> {
    let row=sqlx::query("SELECT a.probe_credential_id,a.probe_credential_version,p.expires_at,p.discarded FROM mtls_activations a LEFT JOIN mtls_probe_versions p ON p.credential_id=a.probe_credential_id AND p.credential_version=a.probe_credential_version AND p.node_id=a.node_id WHERE a.job_id=$1")
        .bind(job).fetch_optional(pool).await?;
    let Some(row) = row else { return Ok(None) };
    let Some(id) = row.get::<Option<String>, _>("probe_credential_id") else {
        return Ok(None);
    };
    ensure!(
        row.get::<Option<chrono::DateTime<Utc>>, _>("expires_at")
            .is_some_and(|at| at > Utc::now())
            && row.get::<Option<bool>, _>("discarded") == Some(false),
        "activation probe identity expired or was discarded; start a new deployment"
    );
    Ok(Some((id, row.get("probe_credential_version"))))
}

pub async fn historical_snapshot(
    pool: &PgPool,
    secrets: &SecretBox,
    node: &str,
    revision: i64,
) -> Result<Option<Value>> {
    let cipher:Option<String>=sqlx::query_scalar("SELECT history.snapshot_enc FROM (SELECT activation_id,snapshot_enc,created_at FROM mtls_deployment_snapshots WHERE node_id=$1 AND revision=$2 UNION ALL SELECT id,snapshot_enc,created_at FROM mtls_activations WHERE node_id=$1 AND target_revision=$2 AND phase='committed') history JOIN mtls_activations a ON a.id=history.activation_id WHERE a.phase='committed' ORDER BY history.created_at DESC,a.generation DESC LIMIT 1").bind(node).bind(revision).fetch_optional(pool).await?;
    cipher
        .map(|cipher| Ok(serde_json::from_str(&secrets.decrypt(&cipher)?)?))
        .transpose()
}
