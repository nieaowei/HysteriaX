use super::*;

pub(crate) fn identity(node: &str, hash: &str) -> String {
    format!("probe-{node}-{hash}")
}

pub(super) struct Paths {
    pub stage: String,
    pub probe: String,
}
pub(super) fn paths(job: &JobInput, hash: &str) -> Result<Paths> {
    let node = job
        .node_id
        .as_deref()
        .context("probe job requires a node")?;
    // These node-lease fields also live in execution_operations, allowing
    // explicit recovery to locate an older execution's artifacts.
    let incarnation = match crate::execution::job_leases(&job.id, node)? {
        Some((_, lease)) => json!([
            lease.owner,
            lease.work,
            lease.resource,
            lease.generation,
            lease.epoch
        ]),
        None => json!(["fixture", job.id, node, hash]),
    };
    let key = hex::encode(Sha256::digest(incarnation.to_string().as_bytes()));
    Ok(Paths {
        stage: format!("/tmp/hysteriax-stage-{key}"),
        probe: format!("/tmp/hysteriax-probe-{key}"),
    })
}

pub(super) async fn install(pool: &PgPool, job: &JobInput, hash: &str) -> Result<()> {
    let node = job
        .node_id
        .as_deref()
        .context("probe job requires a node")?;
    let context = crate::execution::job_leases(&job.id, node)?;
    let (job, hash) = (job.clone(), hash.to_owned());
    db::write(pool,move |tx| {let (job,hash,context)=(job.clone(),hash.clone(),context.clone());Box::pin(async move {
        let row=sqlx::query("SELECT kind,node_id,target_revision,payload_json,attempts,resource_key FROM jobs WHERE id=$1 AND status='running' FOR UPDATE")
            .bind(&job.id).fetch_optional(&mut **tx).await?.context("probe job is no longer running")?;
        anyhow::ensure!(row.get::<String,_>("kind")==job.kind && matches!(job.kind.as_str(),"deploy"|"sync"|"rollback")
            && row.get::<Option<String>,_>("node_id")==job.node_id && row.get::<Option<i64>,_>("target_revision")==job.target_revision
            && row.get::<Value,_>("payload_json")==job.payload && row.get::<i64,_>("attempts")==job.attempts,"probe job source changed");
        if let Some(resource)=row.get::<Option<String>,_>("resource_key") {crate::execution::require_job_resource_scope(&job.id,&resource)?;}
        sqlx::query("SELECT id FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
            .bind(&job.node_id).fetch_optional(&mut **tx).await?.context("probe node was removed")?;
        sqlx::query("INSERT INTO deployment_probe_tokens(node_id,token_hash,expires_at,job_id,owner,node_generation,job_generation,epoch) VALUES($1,$2,clock_timestamp()+interval '15 minutes',$3,$4,$5,$6,$7) ON CONFLICT(node_id) DO UPDATE SET token_hash=excluded.token_hash,expires_at=excluded.expires_at,job_id=excluded.job_id,owner=excluded.owner,node_generation=excluded.node_generation,job_generation=excluded.job_generation,epoch=excluded.epoch")
            .bind(&job.node_id).bind(hash).bind(&job.id).bind(context.as_ref().map(|(j,_)|j.owner.as_str()))
            .bind(context.as_ref().map(|(_,n)|n.generation)).bind(context.as_ref().map(|(j,_)|j.generation)).bind(context.as_ref().map(|(j,_)|j.epoch.as_str())).execute(&mut **tx).await?;
        Ok(())
    })}).await
}

pub(super) async fn remove(pool: &PgPool, job: &JobInput, hash: &str) -> Result<()> {
    let node = job
        .node_id
        .as_deref()
        .context("probe job requires a node")?;
    let context = crate::execution::job_leases(&job.id, node)?;
    let (job, hash) = (job.clone(), hash.to_owned());
    db::write(pool,move |tx| {let (job,hash,context)=(job.clone(),hash.clone(),context.clone());Box::pin(async move {
        sqlx::query("DELETE FROM deployment_probe_tokens WHERE node_id=$1 AND token_hash=$2 AND job_id=$3 AND owner IS NOT DISTINCT FROM $4 AND node_generation IS NOT DISTINCT FROM $5 AND job_generation IS NOT DISTINCT FROM $6 AND epoch IS NOT DISTINCT FROM $7")
            .bind(&job.node_id).bind(hash).bind(&job.id).bind(context.as_ref().map(|(j,_)|j.owner.as_str()))
            .bind(context.as_ref().map(|(_,n)|n.generation)).bind(context.as_ref().map(|(j,_)|j.generation)).bind(context.as_ref().map(|(j,_)|j.epoch.as_str())).execute(&mut **tx).await?;
        Ok(())
    })}).await
}

#[cfg(test)]
mod tests;
