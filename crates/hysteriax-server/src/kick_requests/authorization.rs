use super::*;
use std::collections::BTreeSet;

/// Queue exact user/node pairs while retaining one shared kick job per node.
/// Node locks coordinate with completion/scheduling just as enqueue_many does.
pub async fn enqueue_pairs(
    tx: &mut Transaction<'_, Postgres>,
    pairs: &[(String, String)],
    reason: &str,
) -> Result<Vec<String>, ApiError> {
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let pairs: BTreeSet<_> = pairs.iter().cloned().collect();
    let nodes: Vec<_> = pairs
        .iter()
        .map(|(_, n)| n.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let locked: Vec<String> =
        sqlx::query_scalar("SELECT id FROM nodes WHERE id=ANY($1) ORDER BY id FOR NO KEY UPDATE")
            .bind(&nodes)
            .fetch_all(&mut **tx)
            .await?;
    if locked.len() != nodes.len() {
        return Err(ApiError::not_found("node"));
    }
    let proposed: Vec<_> = nodes
        .iter()
        .map(|_| uuid::Uuid::new_v4().to_string())
        .collect();
    let jobs=sqlx::query("INSERT INTO jobs(id,kind,node_id,node_name,payload_json,status,stage,available_at,created_at,updated_at) SELECT p.id,'kick',n.id,n.name,'{\"scope\":\"node\"}'::jsonb,'queued','queued',now(),now(),now() FROM unnest($1::text[],$2::text[]) AS p(node_id,id) JOIN nodes n ON n.id=p.node_id ORDER BY n.id ON CONFLICT(node_id) WHERE kind='kick' AND status IN ('queued','running') DO UPDATE SET updated_at=jobs.updated_at RETURNING id,node_id,status,(xmax=0) AS inserted")
        .bind(&nodes).bind(&proposed).fetch_all(&mut **tx).await?;
    let job_nodes: Vec<String> = jobs.iter().map(|r| r.get("node_id")).collect();
    let job_ids: Vec<String> = jobs.iter().map(|r| r.get("id")).collect();
    sqlx::query("UPDATE kick_requests r SET latest_job_id=p.id,execution_generation=r.generation,updated_at=now() FROM unnest($1::text[],$2::text[]) AS p(node_id,id) WHERE r.node_id=p.node_id AND r.state='active' AND r.latest_job_id IS DISTINCT FROM p.id")
        .bind(&job_nodes).bind(&job_ids).execute(&mut **tx).await?;
    let (users, pair_nodes): (Vec<_>, Vec<_>) = pairs.into_iter().unzip();
    sqlx::query("INSERT INTO kick_requests(node_id,user_id,reasons,state) SELECT p.node_id,p.user_id,jsonb_build_object($3::text,true),'active' FROM unnest($1::text[],$2::text[]) AS p(user_id,node_id) ORDER BY p.node_id,p.user_id ON CONFLICT(node_id,user_id) DO UPDATE SET reasons=CASE WHEN kick_requests.state IN ('completed','cancelled') THEN EXCLUDED.reasons ELSE kick_requests.reasons||EXCLUDED.reasons END,generation=kick_requests.generation+1,state='active',updated_at=now()")
        .bind(&users).bind(&pair_nodes).bind(reason).execute(&mut **tx).await?;
    sqlx::query("UPDATE kick_requests r SET latest_job_id=j.id,execution_generation=CASE WHEN j.status='queued' THEN r.generation ELSE r.execution_generation END FROM unnest($1::text[],$2::text[]) AS p(user_id,node_id) JOIN jobs j ON j.node_id=p.node_id AND j.kind='kick' AND j.status IN ('queued','running') WHERE r.user_id=p.user_id AND r.node_id=p.node_id")
        .bind(&users).bind(&pair_nodes).execute(&mut **tx).await?;
    let events:Vec<_>=jobs.iter().filter(|r|r.get::<bool,_>("inserted")).map(|r|{
        let id:String=r.get("id");let node:String=r.get("node_id");
        json!({"job_id":id,"payload":json!({"id":id,"kind":"kick","node_id":node,"status":"queued","stage":"queued"})})
    }).collect();
    if !events.is_empty() {
        sqlx::query("INSERT INTO job_events(job_id,event_type,payload_json,created_at) SELECT job_id,'job.queued',payload,now() FROM jsonb_to_recordset($1) AS p(job_id text,payload jsonb) ORDER BY job_id").bind(json!(events)).execute(&mut **tx).await?;
    }
    Ok(job_ids
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

/// Empty tasks are derived bookkeeping. Keep their cleanup off permission
/// commits, bounded and restart-safe, with the same node -> job lock order as
/// admission. Busy workers are skipped and active obligations are checked again.
pub(super) async fn cleanup_queued(
    tx: &mut Transaction<'_, Postgres>,
    node: Option<&str>,
) -> Result<(), ApiError> {
    let nodes:Vec<String>=sqlx::query_scalar("SELECT n.id FROM nodes n JOIN jobs j ON j.node_id=n.id AND j.kind='kick' AND j.status='queued' WHERE ($1::text IS NULL OR n.id=$1) AND NOT EXISTS(SELECT 1 FROM kick_requests r WHERE r.latest_job_id=j.id AND r.node_id=n.id AND r.state='active') ORDER BY n.id LIMIT 100 FOR NO KEY UPDATE OF n SKIP LOCKED")
        .bind(node).fetch_all(&mut **tx).await?;
    if nodes.is_empty() {
        return Ok(());
    }
    let jobs:Vec<String>=sqlx::query_scalar("SELECT j.id FROM jobs j WHERE j.kind='kick' AND j.status='queued' AND j.node_id=ANY($1) AND NOT EXISTS(SELECT 1 FROM kick_requests r WHERE r.latest_job_id=j.id AND r.node_id=j.node_id AND r.state='active') ORDER BY j.node_id,j.id FOR UPDATE OF j SKIP LOCKED")
        .bind(&nodes).fetch_all(&mut **tx).await?;
    if jobs.is_empty() {
        return Ok(());
    }
    let cancelled:Vec<String>=sqlx::query_scalar("UPDATE jobs j SET status='cancelled',stage='restriction_cleared',finished_at=now(),updated_at=now() WHERE j.id=ANY($1) AND j.status='queued' AND NOT EXISTS(SELECT 1 FROM kick_requests r WHERE r.latest_job_id=j.id AND r.node_id=j.node_id AND r.state='active') RETURNING j.id")
        .bind(&jobs).fetch_all(&mut **tx).await?;
    if !cancelled.is_empty() {
        sqlx::query("INSERT INTO job_events(job_id,event_type,payload_json,created_at) SELECT id,'job.cancelled',jsonb_build_object('id',id,'status','cancelled','stage','restriction_cleared'),now() FROM unnest($1::text[]) AS p(id) ORDER BY id").bind(&cancelled).execute(&mut **tx).await?;
    }
    Ok(())
}
