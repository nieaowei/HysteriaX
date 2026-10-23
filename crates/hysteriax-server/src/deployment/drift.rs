use super::*;

/// Persist the already-observed mismatch without rereading the remote host on
/// transaction retries. Only the captured deployment source may become drift.
pub(super) async fn record(
    pool: &PgPool,
    job: &JobInput,
    desired_revision: i64,
    deployed_revision: Option<i64>,
    deployed_hash: &str,
) -> Result<()> {
    let (job, hash) = (job.clone(), deployed_hash.to_owned());
    db::write(pool,move |tx|{let (job,hash)=(job.clone(),hash.clone());Box::pin(async move {
        crate::execution::job_source_on(tx,&job).await?.context("sync job is no longer running")?;
        let node=job.node_id.as_deref().context("sync node missing")?;
        anyhow::ensure!(job.kind=="sync" && job.target_revision==Some(desired_revision),"drift observation has another sync source");
        let keys=vec![format!("job:{}",job.id),format!("node:{node}")];
        let uncertain:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM execution_operations WHERE (work_id=$1 OR resource_key=ANY($2)) AND state IN ('dispatching','reconciling','needs_attention'))")
            .bind(&job.id).bind(keys).fetch_one(&mut **tx).await?;
        anyhow::ensure!(!uncertain,"drift observation has an unverified remote step");
        let current=sqlx::query("SELECT desired_revision,deployed_revision,deployed_content_sha256,state FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
            .bind(node).fetch_one(&mut **tx).await?;
        anyhow::ensure!(current.get::<i64,_>("desired_revision")==desired_revision
            && current.get::<Option<i64>,_>("deployed_revision")==deployed_revision
            && current.get::<Option<String>,_>("deployed_content_sha256").as_deref()==Some(hash.as_str())
            && !matches!(current.get::<String,_>("state").as_str(),"deleting"|"delete_failed"),"node deployment changed during drift inspection");
        if current.get::<String,_>("state")!="drift" {
            sqlx::query("UPDATE nodes SET state='drift',updated_at=clock_timestamp() WHERE id=$1").bind(node).execute(&mut **tx).await?;
            super::progress_on(tx,&job,"drift_detected","Remote configuration differs from the saved deployment.").await?;
        }
        Ok(())
    })}).await
}

#[cfg(test)]
mod tests;
