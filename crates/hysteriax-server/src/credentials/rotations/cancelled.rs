//! Bounded cancellation of queued jobs belonging to a superseded CA target.
use super::*;
use crate::background::maintenance::{self, Work};

const KIND: &str = "mtls-superseded-node-jobs";
const SCAN: &str = "mtls-superseded-node-job-admission";
const PENDING: &str = "SELECT o.node_id,o.job_id FROM mtls_node_operations o JOIN jobs j ON j.id=o.job_id WHERE o.rotation_id=$1 AND o.ca_id=$2 AND o.ca_version=$3 AND o.pending_job AND j.status IN ('queued','waiting_confirmation') AND (o.node_id,o.job_id)>($4,$5) ORDER BY o.node_id,o.job_id LIMIT $6";

pub(crate) async fn enqueue_on(
    tx: &mut Transaction<'_, Postgres>,
    rotation: &str,
    ca: &str,
    version: i64,
) -> Result<()> {
    sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',available_at=clock_timestamp(),error_code=NULL,payload=EXCLUDED.payload WHERE background_work.state IN ('complete','failed') AND background_work.available_at<=clock_timestamp()")
        .bind(format!("superseded-node-jobs:{rotation}")).bind(KIND)
        .bind(json!({"rotation_id":rotation,"ca_id":ca,"ca_version":version,"cursor_node":"","cursor_job":"","lease_resources":[format!("ca:{ca}")]}))
        .execute(&mut **tx).await?;
    Ok(())
}
pub(super) async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        if let Err(error) = scheduled_pass(&state).await {
            tracing::warn!(%error,"superseded job admission deferred");
        }
    }
}
pub(super) async fn scheduled_pass(state: &AppState) -> Result<()> {
    scan_pass(state).await?;
    if let Some(work) = maintenance::claim(&state.pool, KIND, &state.instance_id).await? {
        let _ = crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = page(&state.pool, &work).await {
                tracing::warn!(%error,work_id=%work.id,"superseded job cancellation deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
    }
    Ok(())
}
async fn scan_pass(state: &AppState) -> Result<()> {
    maintenance::ensure(&state.pool, "mtls:superseded-node-jobs", SCAN, json!({})).await?;
    if let Some(work) = maintenance::claim(&state.pool, SCAN, &state.instance_id).await? {
        let _ = crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = scan(&state.pool, &work).await {
                tracing::warn!(%error,"superseded historical job scan deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await;
    }
    Ok(())
}
async fn scan(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::serializable(pool,move |tx|{let work=work.clone();Box::pin(async move {
        work.guard_on(tx).await?;
        ensure!(work.id=="maintenance:mtls:superseded-node-jobs","wrong superseded job admission scope");
        // This also discovers historical supersessions and tasks requeued by
        // lease recovery after a cancellation outbox had previously completed.
        let rows=sqlx::query("SELECT DISTINCT r.id,r.ca_id,r.ca_version FROM mtls_node_operations o JOIN mtls_rotations r ON r.id=o.rotation_id WHERE o.pending_job AND o.rotation_id IS NOT NULL AND r.phase='complete' AND r.outcome IN ('superseded_unpublished_target','superseded_expired_publication') AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id='superseded-node-jobs:'||r.id AND (w.state IN ('queued','running') OR w.available_at>clock_timestamp())) ORDER BY r.id LIMIT $1")
            .bind(PAGE).fetch_all(&mut **tx).await?;
        for row in rows {enqueue_on(tx,&row.get::<String,_>("id"),&row.get::<String,_>("ca_id"),row.get("ca_version")).await?;}
        work.finish_on(tx,true,1,None).await
    })}).await
}
async fn page(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::serializable(pool,move |tx| {let work=work.clone();Box::pin(async move {
        work.guard_on(tx).await?;
        let rotation=work.payload["rotation_id"].as_str().context("missing rotation")?;
        let ca=work.payload["ca_id"].as_str().context("missing CA")?;
        let version=work.payload["ca_version"].as_i64().context("missing generation")?;
        let cursor_node=work.payload["cursor_node"].as_str().context("missing node cursor")?;
        let cursor_job=work.payload["cursor_job"].as_str().context("missing job cursor")?;
        ensure!(work.id==format!("superseded-node-jobs:{rotation}") && work.leases.iter().any(|l|l.resource==format!("ca:{ca}")),"wrong superseded job scope");
        let rows=sqlx::query(PENDING).bind(rotation).bind(ca).bind(version).bind(cursor_node).bind(cursor_job).bind(PAGE).fetch_all(&mut **tx).await?;
        let candidates:Vec<String>=rows.iter().map(|r|r.get("node_id")).collect();
        let nodes:Vec<String>=sqlx::query_scalar("SELECT id FROM nodes WHERE id=ANY($1::text[]) ORDER BY id FOR NO KEY UPDATE SKIP LOCKED").bind(candidates).fetch_all(&mut **tx).await?;
        sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR KEY SHARE").bind(ca).fetch_optional(&mut **tx).await?;
        sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE").bind(ca).fetch_optional(&mut **tx).await?;
        let valid:Option<bool>=sqlx::query_scalar("SELECT r.phase='complete' AND coalesce(r.outcome IN ('superseded_unpublished_target','superseded_expired_publication'),false) FROM mtls_rotations r JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.id=$1 AND r.ca_id=$2 AND r.ca_version=$3 FOR UPDATE OF r,g").bind(rotation).bind(ca).bind(version).fetch_optional(&mut **tx).await?;
        if valid!=Some(true) {return work.finish_on(tx,false,0,Some("superseded_job_source_changed")).await;}
        let ids:Vec<String>=rows.iter().map(|r|r.get("job_id")).collect();
        // Never clear a healthy executor or an unresolved external intent. The
        // source fact already forbids new dispatches; recovery owns old intents.
        let jobs=sqlx::query("SELECT j.id,j.kind,j.node_id FROM jobs j WHERE j.id=ANY($1::text[]) AND j.node_id=ANY($2::text[]) AND j.status IN ('queued','waiting_confirmation') AND NOT EXISTS(SELECT 1 FROM resource_leases l WHERE l.resource_key=ANY(ARRAY['job:'||j.id,'node:'||j.node_id]) AND (l.blocked OR l.owner IS NOT NULL AND l.expires_at>clock_timestamp())) AND NOT EXISTS(SELECT 1 FROM execution_operations o WHERE (o.work_id=j.id OR o.resource_key='node:'||j.node_id) AND o.state IN ('dispatching','reconciling','needs_attention')) ORDER BY j.id FOR UPDATE OF j SKIP LOCKED")
            .bind(ids).bind(nodes).fetch_all(&mut **tx).await?;
        let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
        for job in &jobs {
            let id:String=job.get("id");
            sqlx::query("UPDATE jobs SET status='cancelled',stage='ca_target_superseded',updated_at=$2,finished_at=$2 WHERE id=$1").bind(&id).bind(now).execute(&mut **tx).await?;
            crate::kick_requests::event(tx,&id,"job.cancelled",json!({"id":id,"kind":job.get::<String,_>("kind"),"node_id":job.get::<Option<String>,_>("node_id"),"status":"cancelled","stage":"ca_target_superseded"})).await?;
        }
        let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_node_operations o JOIN jobs j ON j.id=o.job_id WHERE o.rotation_id=$1 AND o.ca_id=$2 AND o.ca_version=$3 AND o.pending_job AND j.status IN ('queued','waiting_confirmation'))").bind(rotation).bind(ca).bind(version).fetch_one(&mut **tx).await?;
        let (next_node,next_job)=if rows.len()==PAGE as usize {let last=rows.last().unwrap();(last.get::<String,_>("node_id"),last.get::<String,_>("job_id"))} else {(String::new(),String::new())};
        sqlx::query("UPDATE background_work SET payload=jsonb_set(jsonb_set(payload,'{cursor_node}',to_jsonb($2::text)),'{cursor_job}',to_jsonb($3::text)) WHERE id=$1").bind(&work.id).bind(next_node).bind(next_job).execute(&mut **tx).await?;
        let delay=if !jobs.is_empty() || rows.len()==PAGE as usize {1} else {60};
        work.finish_on(tx,pending,if pending {delay} else {0},None).await
    })}).await
}

#[cfg(test)]
mod tests;
