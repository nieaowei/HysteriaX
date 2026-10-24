use super::super::identities::{self, PreparedIdentity};
use crate::{
    db,
    deployment::JobInput,
    execution::{self, Execution},
    security::SecretBox,
};
use anyhow::{Context, Result};
use sqlx::{PgPool, Postgres, Row, Transaction};

#[derive(Clone)]
pub(super) struct Source {
    job: JobInput,
    revision: i64,
    authorization_generation: i64,
    pub(super) users: Vec<String>,
    leases: Vec<Execution>,
}
async fn job_on(tx: &mut Transaction<'_, Postgres>, job: &JobInput) -> Result<()> {
    execution::require_job_scope(&job.id, job.node_id.as_deref())?;
    anyhow::ensure!(
        matches!(job.kind.as_str(), "deploy" | "sync" | "rollback"),
        "identity preparation requires a deployment job"
    );
    let row=sqlx::query("SELECT kind,node_id,target_revision,payload_json,attempts FROM jobs WHERE id=$1 AND status='running' FOR UPDATE")
        .bind(&job.id).fetch_optional(&mut **tx).await?.context("deployment job is no longer running")?;
    anyhow::ensure!(
        row.get::<String, _>("kind") == job.kind
            && row.get::<Option<String>, _>("node_id") == job.node_id
            && row.get::<Option<i64>, _>("target_revision") == job.target_revision
            && row.get::<serde_json::Value, _>("payload_json") == job.payload
            && row.get::<i64, _>("attempts") == job.attempts,
        "deployment job source changed"
    );
    let keys = vec![
        format!("job:{}", job.id),
        format!(
            "node:{}",
            job.node_id.as_deref().context("deployment node missing")?
        ),
    ];
    let uncertain:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM execution_operations WHERE (work_id=$1 OR resource_key=ANY($2)) AND state IN ('dispatching','reconciling','needs_attention'))")
        .bind(&job.id).bind(keys).fetch_one(&mut **tx).await?;
    anyhow::ensure!(!uncertain, "deployment has an unverified remote step");
    Ok(())
}
async fn node_on(tx: &mut Transaction<'_, Postgres>, job: &JobInput) -> Result<(i64, i64)> {
    let node = job.node_id.as_deref().context("deployment node missing")?;
    let row = sqlx::query("SELECT n.desired_revision,v.generation FROM nodes n JOIN node_authorization_versions v ON v.node_id=n.id WHERE n.id=$1 AND n.state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE OF n FOR SHARE OF v")
        .bind(node).fetch_optional(&mut **tx).await?.context("deployment node or authorization source is missing")?;
    Ok((row.get("desired_revision"), row.get("generation")))
}

pub(super) async fn capture(pool: &PgPool, job: &JobInput) -> Result<Source> {
    let job = job.clone();
    db::serializable(pool,move |tx|{let job=job.clone();Box::pin(async move {
        job_on(tx,&job).await?;
        let mut keys=vec![format!("job:{}",job.id),format!("node:{}",job.node_id.as_deref().context("deployment node missing")?)];keys.sort();
        let rows=sqlx::query("SELECT resource_key,owner,generation FROM resource_leases WHERE resource_key=ANY($1) AND work_id=$2 AND owner IS NOT NULL AND expires_at>clock_timestamp() AND NOT blocked ORDER BY resource_key FOR UPDATE")
            .bind(&keys).bind(&job.id).fetch_all(&mut **tx).await?;
        anyhow::ensure!(rows.len()==keys.len(),"deployment identity preparation requires full execution leases");
        let leases:Vec<Execution>=rows.iter().map(|r|Execution{resource:r.get("resource_key"),owner:r.get("owner"),generation:r.get("generation"),work:job.id.clone()}).collect();
        anyhow::ensure!(leases.iter().all(|l|l.owner==leases[0].owner),"deployment identity preparation has mixed owners");
        let (revision,authorization_generation)=node_on(tx,&job).await?;
        let users=sqlx::query_scalar("SELECT user_id FROM node_assignments WHERE node_id=$1 ORDER BY user_id")
            .bind(&job.node_id).fetch_all(&mut **tx).await?;
        Ok(Source{job,revision,authorization_generation,users,leases})
    })}).await
}
async fn current_on(tx: &mut Transaction<'_, Postgres>, s: &Source) -> Result<()> {
    job_on(tx, &s.job).await?;
    for lease in &s.leases {
        lease.guard(tx).await?;
    }
    let (revision, authorization_generation) = node_on(tx, &s.job).await?;
    anyhow::ensure!(
        revision == s.revision && authorization_generation == s.authorization_generation,
        "node permissions changed during signing; retry deployment"
    );
    sqlx::query("SELECT id FROM users WHERE id=ANY($1) ORDER BY id FOR NO KEY UPDATE")
        .bind(&s.users)
        .fetch_all(&mut **tx)
        .await?;
    Ok(())
}
pub(super) async fn validate(pool: &PgPool, source: &Source) -> Result<()> {
    let source = source.clone();
    db::serializable(pool, move |tx| {
        let source = source.clone();
        Box::pin(async move { current_on(tx, &source).await })
    })
    .await
}
pub(super) async fn commit(
    pool: &PgPool,
    secrets: &SecretBox,
    source: &Source,
    candidates: Vec<PreparedIdentity>,
) -> Result<()> {
    let (source, secrets) = (source.clone(), secrets.clone());
    db::serializable(pool, move |tx| {
        let (source, secrets, candidates) = (source.clone(), secrets.clone(), candidates.clone());
        Box::pin(async move {
            current_on(tx, &source).await?;
            for candidate in candidates {
                identities::commit_user(tx, &secrets, candidate).await?;
            }
            Ok(())
        })
    })
    .await
}

#[cfg(test)]
mod tests;
