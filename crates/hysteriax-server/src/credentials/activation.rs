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
#[derive(Clone)]
struct Prepared {
    id: String,
    node: String,
    job: String,
    revision: i64,
    digest: String,
    cipher: String,
    trust: Value,
    versions: Vec<TrustVersion>,
    probe: Option<(String, i64)>,
}
fn prepare(secrets: &SecretBox, input: Begin<'_>) -> Result<Prepared> {
    crate::execution::require_job_scope(input.job, Some(input.node))?;
    Ok(Prepared {
        id: Uuid::new_v4().to_string(),
        node: input.node.into(),
        job: input.job.into(),
        revision: input.revision,
        digest: input.digest.into(),
        cipher: secrets.encrypt(&input.snapshot.to_string())?,
        trust: serde_json::to_value(input.trust)?,
        versions: input.trust.to_vec(),
        probe: input.probe.map(|(id, version)| (id.to_owned(), version)),
    })
}

/// Lock the actual job and its node rather than inventing a deploy job context.
async fn job_on(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<(crate::deployment::JobInput, sqlx::postgres::PgRow)> {
    let row = sqlx::query("SELECT * FROM jobs WHERE id=$1 AND status='running' FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .context("activation job is no longer running")?;
    let mut payload: Value = row.get("payload_json");
    if sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM mtls_node_operations WHERE job_id=$1)",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?
    {
        payload["mtls_operation"] = json!(true);
    }
    let job = crate::deployment::JobInput {
        id: id.into(),
        kind: row.get("kind"),
        node_id: row.get("node_id"),
        target_revision: row.get("target_revision"),
        payload,
        attempts: row.get("attempts"),
    };
    crate::execution::require_job_scope(id, job.node_id.as_deref())?;
    if let Some(resource) = row.get::<Option<String>, _>("resource_key") {
        crate::execution::require_job_resource_scope(id, &resource)?;
    }
    ensure!(
        matches!(job.kind.as_str(), "deploy" | "sync" | "rollback"),
        "job cannot change an activation"
    );
    let node=sqlx::query("SELECT * FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
        .bind(&job.node_id).fetch_optional(&mut **tx).await?.context("activation node was removed or is being deleted")?;
    Ok((job, node))
}

pub async fn begin(pool: &PgPool, secrets: &SecretBox, input: Begin<'_>) -> Result<Record> {
    begin_prepared(pool, prepare(secrets, input)?).await
}
async fn begin_prepared(pool: &PgPool, input: Prepared) -> Result<Record> {
    db::serializable(pool,move |tx| {let input=input.clone();Box::pin(async move {
        let (job,node)=job_on(tx,&input.job).await?;
        ensure!(job.node_id.as_deref()==Some(input.node.as_str()) && job.target_revision==Some(input.revision),"activation does not match the job target");
        super::rotations::pin_job_source_on(tx,&job).await?;
        if let Some(row)=sqlx::query("SELECT * FROM mtls_activations WHERE job_id=$1 FOR UPDATE").bind(&input.job).fetch_optional(&mut **tx).await? {
            ensure!(row.get::<String,_>("node_id")==input.node && row.get::<i64,_>("target_revision")==input.revision && row.get::<String,_>("target_digest")==input.digest,"activation target changed; explicit recovery is required");
            let record=read(&row);
            ensure!(matches!(record.phase.as_str(),"activating"|"verified"|"recovery_required"),"activation already finished; start a new deployment");
            ensure!(node.get::<Option<String>,_>("mtls_activation_id").as_deref()==Some(record.id.as_str()) && node.get::<i64,_>("mtls_generation")==record.generation,"activation was superseded; start a new deployment");
            ensure!(row.get::<Value,_>("trust")==input.trust && row.get::<Option<String>,_>("probe_credential_id")==input.probe.as_ref().map(|p|p.0.clone()) && row.get::<Option<i64>,_>("probe_credential_version")==input.probe.as_ref().map(|p|p.1),"activation verification inputs changed; recover with a fresh operation");
            sqlx::query("UPDATE nodes SET mtls_state='activating',mtls_error=NULL WHERE id=$1").bind(&input.node).execute(&mut **tx).await?;
            return Ok(record);
        }
        let generation=node.get::<i64,_>("mtls_generation").checked_add(1).context("activation generation exhausted")?;
        let previous:Option<String>=node.get("deployed_content_sha256");
        let timestamp:chrono::DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
        sqlx::query("INSERT INTO mtls_activations(id,node_id,job_id,generation,target_revision,phase,previous_state,previous_digest,target_digest,snapshot_enc,trust,probe_credential_id,probe_credential_version,created_at,updated_at,previous_activation_id) VALUES($1,$2,$3,$4,$5,'activating',$6,$7,$8,$9,$10,$11,$12,$13,$13,$14)")
            .bind(&input.id).bind(&input.node).bind(&input.job).bind(generation).bind(input.revision).bind(node.get::<String,_>("mtls_state")).bind(&previous).bind(&input.digest).bind(&input.cipher).bind(&input.trust).bind(input.probe.as_ref().map(|p|p.0.as_str())).bind(input.probe.as_ref().map(|p|p.1)).bind(timestamp).bind(node.get::<Option<String>,_>("mtls_activation_id")).execute(&mut **tx).await?;
        let mut versions=input.versions.clone();versions.sort_by(|a,b|(&a.ca_id,a.version).cmp(&(&b.ca_id,b.version)));
        for ca in versions {
            sqlx::query("INSERT INTO mtls_activation_trust(activation_id,ca_id,ca_version) VALUES($1,$2,$3)")
                .bind(&input.id).bind(ca.ca_id).bind(ca.version).execute(&mut **tx).await?;
        }
        sqlx::query("UPDATE nodes SET mtls_state='activating',mtls_error=NULL,mtls_generation=$2,mtls_activation_id=$3 WHERE id=$1").bind(&input.node).bind(generation).bind(&input.id).execute(&mut **tx).await?;
        Ok(Record{id:input.id,generation,target_digest:input.digest,previous_digest:previous,phase:"activating".into(),fresh:true})
    })}).await
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

/// Bind an explicitly inspected recovery baseline once, without replacing the
/// immutable evidence of a resumed or superseded activation.
pub async fn record_previous(
    pool: &PgPool,
    job: &str,
    record: &Record,
    digest: Option<&str>,
) -> Result<Record> {
    crate::execution::require_job_scope(job, None)?;
    let (job, record, digest) = (job.to_owned(), record.clone(), digest.map(str::to_owned));
    db::serializable(pool,move |tx| {let (id,expected,digest)=(job.clone(),record.clone(),digest.clone());Box::pin(async move {
        let (job,node)=job_on(tx,&id).await?;
        super::rotations::pin_job_source_on(tx,&job).await?;
        let row=sqlx::query("SELECT * FROM mtls_activations WHERE job_id=$1 FOR UPDATE").bind(&id).fetch_one(&mut **tx).await?;
        ensure!(row.get::<String,_>("id")==expected.id && row.get::<i64,_>("generation")==expected.generation && row.get::<String,_>("target_digest")==expected.target_digest
            && Some(row.get::<i64,_>("target_revision"))==job.target_revision && Some(row.get::<String,_>("node_id"))==job.node_id
            && node.get::<Option<String>,_>("mtls_activation_id").as_deref()==Some(expected.id.as_str()) && node.get::<i64,_>("mtls_generation")==expected.generation,"recovery activation was superseded");
        let mut current=read(&row);current.fresh=expected.fresh;
        ensure!(matches!(current.phase.as_str(),"activating"|"verified"|"recovery_required"),"recovery activation already finished");
        if current.previous_digest==digest {return Ok(current);}
        ensure!(expected.fresh && current.phase=="activating" && current.previous_digest==expected.previous_digest
            && current.previous_digest==node.get::<Option<String>,_>("deployed_content_sha256"),"recovery baseline changed; use a new verified operation");
        sqlx::query("UPDATE mtls_activations SET previous_digest=$2,updated_at=clock_timestamp() WHERE id=$1")
            .bind(&expected.id).bind(&digest).execute(&mut **tx).await?;
        current.previous_digest=digest;
        Ok(current)
    })}).await
}

#[derive(Clone)]
enum Transition {
    Verified,
    Uncertain,
    RolledBack(Option<String>),
}

pub async fn verified(pool: &PgPool, job: &str) -> Result<()> {
    transition(pool, job, Transition::Verified).await
}
pub async fn uncertain(pool: &PgPool, job: &str) -> Result<()> {
    transition(pool, job, Transition::Uncertain).await
}
pub async fn rolled_back(pool: &PgPool, job: &str, actual_digest: Option<&str>) -> Result<()> {
    transition(
        pool,
        job,
        Transition::RolledBack(actual_digest.map(str::to_owned)),
    )
    .await
}
async fn transition(pool: &PgPool, job: &str, change: Transition) -> Result<()> {
    crate::execution::require_job_scope(job, None)?;
    let job = job.to_owned();
    db::serializable(pool,move |tx| {let (id,change)=(job.clone(),change.clone());Box::pin(async move {
        // Deployment can fail before an activation was ever created.
        if matches!(change,Transition::Uncertain) && !sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM mtls_activations WHERE job_id=$1)").bind(&id).fetch_one(&mut **tx).await? {return Ok(());}
        let (job,node)=job_on(tx,&id).await?;
        if matches!(change,Transition::Verified) {super::rotations::pin_job_source_on(tx,&job).await?;}
        let row=sqlx::query("SELECT * FROM mtls_activations WHERE job_id=$1 FOR UPDATE").bind(&id).fetch_one(&mut **tx).await?;
        ensure!(Some(row.get::<String,_>("node_id"))==job.node_id && Some(row.get::<i64,_>("target_revision"))==job.target_revision,"activation no longer matches its job");
        let phase:String=row.get("phase");
        if matches!(change,Transition::Uncertain) && matches!(phase.as_str(),"committed"|"rolled_back") {return Ok(());}
        let active:String=row.get("id");let generation:i64=row.get("generation");
        if let Transition::RolledBack(ref digest)=change {
            ensure!(row.get::<Option<String>,_>("previous_digest")==*digest,"remote rollback digest does not match the previous deployment");
            if phase=="rolled_back" {
                ensure!(node.get::<i64,_>("mtls_generation")==generation && node.get::<Option<String>,_>("mtls_activation_id")==row.get::<Option<String>,_>("previous_activation_id"),"rollback was superseded by another activation");
                return Ok(());
            }
        }
        ensure!(node.get::<Option<String>,_>("mtls_activation_id").as_deref()==Some(active.as_str()) && node.get::<i64,_>("mtls_generation")==generation && matches!(phase.as_str(),"activating"|"verified"|"recovery_required"),"activation was superseded during result writeback");
        if !matches!(change,Transition::Uncertain) {
            let unresolved:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM execution_operations WHERE (work_id=$1 OR resource_key=ANY(ARRAY['job:'||$1,'node:'||$2]::text[]) OR resource_key=(SELECT resource_key FROM jobs WHERE id=$1)) AND state IN ('dispatching','reconciling','needs_attention'))")
                .bind(&id).bind(&job.node_id).fetch_one(&mut **tx).await?;
            ensure!(!unresolved,"remote steps require explicit verification before activation acknowledgement");
        }
        let timestamp:chrono::DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
        let next=match change {Transition::Verified=>"verified",Transition::Uncertain=>"recovery_required",Transition::RolledBack(_)=>"rolled_back"};
        sqlx::query("UPDATE mtls_activations SET phase=$2,updated_at=$3 WHERE id=$1").bind(&active).bind(next).bind(timestamp).execute(&mut **tx).await?;
        match change {
            Transition::Verified=>{},
            Transition::Uncertain=>{sqlx::query("UPDATE nodes SET mtls_state='recovery_required',mtls_error='Remote activation outcome is unconfirmed; explicit verification is required before recovery' WHERE id=$1").bind(&job.node_id).execute(&mut **tx).await?;},
            Transition::RolledBack(_)=>{sqlx::query("UPDATE nodes SET mtls_state=$2,mtls_error=NULL,mtls_activation_id=$3 WHERE id=$1")
                .bind(&job.node_id).bind(row.get::<String,_>("previous_state")).bind(row.get::<Option<String>,_>("previous_activation_id")).execute(&mut **tx).await?;},
        }
        Ok(())
    })}).await
}

/// Called within the exact job-completion transaction that publishes deployed
/// config and connection parameters. There is no earlier subscription release.
pub async fn commit(tx: &mut Transaction<'_, Postgres>, job: &str, digest: &str) -> Result<()> {
    crate::execution::require_job_scope(job, None)?;
    let source=sqlx::query("SELECT node_id,kind,target_revision,resource_key FROM jobs WHERE id=$1 AND status IN ('running','succeeded') FOR UPDATE")
        .bind(job).fetch_optional(&mut **tx).await?.context("activation completion job is missing")?;
    ensure!(
        matches!(
            source.get::<String, _>("kind").as_str(),
            "deploy" | "sync" | "rollback"
        ),
        "job cannot commit an activation"
    );
    let parent: String = source
        .get::<Option<String>, _>("node_id")
        .context("activation completion node is missing")?;
    crate::execution::require_job_scope(job, Some(&parent))?;
    if let Some(resource) = source.get::<Option<String>, _>("resource_key") {
        crate::execution::require_job_resource_scope(job, &resource)?;
    }
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(&parent)
        .fetch_one(&mut **tx)
        .await?;
    let Some(row)=sqlx::query("SELECT a.* FROM mtls_activations a JOIN nodes n ON n.mtls_activation_id=a.id AND n.mtls_generation=a.generation WHERE a.node_id=n.id AND a.job_id=$1 AND a.phase='verified' AND a.target_digest=$2 AND a.node_id=$3 FOR UPDATE OF a")
        .bind(job).bind(digest).bind(&parent).fetch_optional(&mut **tx).await? else {
        anyhow::bail!("activation is missing, superseded, or not remotely verified");
    };
    ensure!(
        Some(row.get::<i64, _>("target_revision"))
            == source.get::<Option<i64>, _>("target_revision"),
        "activation completion target changed"
    );
    let node: String = row.get("node_id");
    crate::execution::require_job_scope(job, Some(&node))?;
    let timestamp: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    let trust: Vec<TrustVersion> = serde_json::from_value(row.get("trust"))
        .map_err(|_| sqlx::Error::Protocol("invalid activation trust".into()))?;
    let mut ordered = trust.clone();
    ordered.sort_by(|a, b| (&a.ca_id, a.version).cmp(&(&b.ca_id, b.version)));
    sqlx::query("UPDATE mtls_node_trust SET deployed=FALSE,revision=revision+1 WHERE node_id=$1 AND deployed").bind(&node).execute(&mut **tx).await?;
    for ca in &ordered {
        sqlx::query("INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,deployed) VALUES($1,$2,$3,TRUE) ON CONFLICT(node_id,ca_id,ca_version) DO UPDATE SET deployed=TRUE,revision=mtls_node_trust.revision+1")
            .bind(&node).bind(&ca.ca_id).bind(ca.version).execute(&mut **tx).await?;
    }
    sqlx::query("UPDATE mtls_activations SET phase='committed',updated_at=$2 WHERE job_id=$1")
        .bind(job)
        .bind(timestamp)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE nodes SET mtls_state='ready',mtls_error=NULL WHERE id=$1")
        .bind(&node)
        .execute(&mut **tx)
        .await?;
    super::authorization::reconcile_node(tx, &node, false).await?;
    for version in &ordered {
        sqlx::query("INSERT INTO mtls_trust_cleanup(ca_id,ca_version,next_action_at) SELECT ca_id,ca_version,$3 FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=$2 AND state='retired' ON CONFLICT(ca_id,ca_version) DO UPDATE SET completed_at=NULL,next_action_at=least(mtls_trust_cleanup.next_action_at,excluded.next_action_at)").bind(&version.ca_id).bind(version.version).bind(timestamp).execute(&mut **tx).await?;
    }
    if let Some(first) = trust.first() {
        let published: i64 = sqlx::query_scalar(
            "SELECT published_version FROM mtls_authorities WHERE credential_id=$1",
        )
        .bind(&first.ca_id)
        .fetch_one(&mut **tx)
        .await?;
        super::rotations::switch_probe_on(tx, &node, &first.ca_id, published).await?;
    } else {
        sqlx::query("DELETE FROM mtls_active_probes WHERE node_id=$1")
            .bind(&node)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("INSERT INTO mtls_deployment_snapshots(node_id,revision,activation_id,snapshot_enc,created_at) SELECT node_id,target_revision,id,snapshot_enc,$2 FROM mtls_activations WHERE job_id=$1 ON CONFLICT DO NOTHING").bind(job).bind(timestamp).execute(&mut **tx).await?;
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

#[cfg(test)]
mod ownership_tests;
