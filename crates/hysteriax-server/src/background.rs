//! Durable work shared by every instance. Leases fence all result transactions;
//! sampling slots are admitted once and retain their original node denominator.
use crate::{
    db,
    execution::{self, Execution},
    state::AppState,
};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Work {
    pub id: String,
    pub kind: String,
    pub cycle: Uuid,
    pub node: String,
    pub period_start: DateTime<Utc>,
    pub expected: i64,
    pub payload: Value,
    pub leases: Vec<Execution>,
}

pub async fn ensure_cycle(pool: &PgPool, kind: &str) -> Result<Uuid> {
    let kind = kind.to_owned();
    let id = Uuid::new_v4();
    db::serializable(pool,move |tx| {
        let kind=kind.clone();
        Box::pin(async move {
            let seconds=match kind.as_str(){"traffic"|"network"=>10_i64,_=>anyhow::bail!("unsupported sampling kind")};
            let period:DateTime<Utc>=sqlx::query_scalar("SELECT to_timestamp(floor(extract(epoch FROM clock_timestamp())/$1)*$1)").bind(seconds as f64).fetch_one(&mut **tx).await?;
            let deadline=period+chrono::Duration::seconds(seconds);
            let created=sqlx::query("INSERT INTO sampling_cycles(id,kind,period_start,deadline_at,expected_nodes) VALUES($1,$2,$3,$4,0) ON CONFLICT(kind,period_start) DO NOTHING")
                .bind(id).bind(&kind).bind(period).bind(deadline).execute(&mut **tx).await?.rows_affected()==1;
            if !created {return Ok(sqlx::query_scalar("SELECT id FROM sampling_cycles WHERE kind=$1 AND period_start=$2").bind(&kind).bind(period).fetch_one(&mut **tx).await?);}
            let ids:Vec<String>=if kind=="traffic" {
                sqlx::query_scalar("SELECT id FROM nodes WHERE deployed_revision IS NOT NULL AND state NOT IN ('deleting','delete_failed') ORDER BY id FOR KEY SHARE").fetch_all(&mut **tx).await?
            } else {
                sqlx::query_scalar("SELECT n.id FROM nodes n JOIN node_packages p ON p.node_id=n.id WHERE (n.deployed_revision IS NOT NULL OR p.config->>'quota_bytes' IS NOT NULL) AND n.state NOT IN ('deleting','delete_failed') ORDER BY n.id FOR KEY SHARE OF n").fetch_all(&mut **tx).await?
            };
            let expected=ids.len() as i64;
            sqlx::query("UPDATE sampling_cycles SET expected_nodes=$2 WHERE id=$1").bind(id).bind(expected).execute(&mut **tx).await?;
            sqlx::query("INSERT INTO background_work(id,kind,cycle_id,node_id,available_at,deadline_at) SELECT $1::uuid::text||':'||node,$2,$1::uuid,node,$3,$4 FROM unnest($5::text[]) node")
                .bind(id).bind(&kind).bind(period).bind(deadline).bind(&ids).execute(&mut **tx).await?;
            if kind=="traffic" {
                sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,status,connections,cycle_started_at,expected_nodes,pending) SELECT $1,node,$2,'failed','{}',$2,$3,true FROM unnest($4::text[]) node")
                    .bind(id).bind(period).bind(expected).bind(&ids).execute(&mut **tx).await?;
            }
            Ok(id)
        })
    }).await
}

/// Recover read-only work only while its slot is current. Already captured
/// traffic is always eligible: recovery must finish billing rather than read SSH.
pub async fn claim(pool: &PgPool, kind: &str, owner: &str) -> Result<Option<Work>> {
    let (kind, owner) = (kind.to_owned(), owner.to_owned());
    db::write(pool,move |tx| {
        let (kind,owner)=(kind.clone(),owner.clone());
        Box::pin(async move {
            let rows=sqlx::query("SELECT w.*,c.period_start,c.expected_nodes FROM background_work w JOIN sampling_cycles c ON c.id=w.cycle_id JOIN nodes n ON n.id=w.node_id WHERE w.kind=$1 AND w.state IN ('queued','running') AND n.state NOT IN ('deleting','delete_failed') AND w.available_at<=clock_timestamp() AND (w.deadline_at>clock_timestamp() OR (w.kind='traffic' AND EXISTS(SELECT 1 FROM traffic_sample_batches b WHERE b.sample_id=w.id AND b.payload_digest IS NOT NULL))) AND NOT EXISTS(SELECT 1 FROM resource_leases l WHERE l.resource_key=ANY(ARRAY['work:'||w.id,'node:'||w.node_id]) AND (l.blocked OR (l.owner IS NOT NULL AND l.expires_at>clock_timestamp()) OR EXISTS(SELECT 1 FROM execution_operations o WHERE o.resource_key=l.resource_key AND o.state IN ('dispatching','reconciling','needs_attention')))) ORDER BY w.available_at,w.id LIMIT 32 FOR UPDATE OF w SKIP LOCKED")
                .bind(&kind).fetch_all(&mut **tx).await?;
            for row in rows {
                let id:String=row.get("id");let node:String=row.get("node_id");
                sqlx::query("SAVEPOINT sampling_candidate").execute(&mut **tx).await?;
                let mut leases=Vec::new();let mut resources=vec![format!("node:{node}"),format!("work:{id}")];resources.sort();
                for resource in resources {
                    if let Some(lease)=execution::acquire(tx,&resource,&owner,&id).await? {leases.push(lease);} else {break;}
                }
                if leases.len()!=2 {
                    sqlx::query("ROLLBACK TO SAVEPOINT sampling_candidate").execute(&mut **tx).await?;
                    sqlx::query("RELEASE SAVEPOINT sampling_candidate").execute(&mut **tx).await?;
                    continue;
                }
                let generation=leases.iter().find(|lease|lease.resource.starts_with("work:")).unwrap().generation;
                let mut payload:Value=row.get("payload");
                if payload.get("captured_at").is_none() {
                    let captured:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
                    payload["captured_at"]=json!(captured);
                    if kind=="network" {
                        payload["capture_generation"]=json!(sqlx::query_scalar::<_,i64>("SELECT generation FROM node_packages WHERE node_id=$1").bind(&node).fetch_one(&mut **tx).await?);
                    }
                }
                sqlx::query("UPDATE background_work SET state='running',owner=$2,generation=$3,payload=$4,started_at=coalesce(started_at,clock_timestamp()) WHERE id=$1")
                    .bind(&id).bind(&owner).bind(generation).bind(&payload).execute(&mut **tx).await?;
                sqlx::query("RELEASE SAVEPOINT sampling_candidate").execute(&mut **tx).await?;
                return Ok(Some(Work{id,kind,cycle:row.get("cycle_id"),node,period_start:row.get("period_start"),expected:row.get("expected_nodes"),payload,leases}));
            }
            Ok(None)
        })
    }).await
}

pub async fn finish(pool: &PgPool, work: &Work, success: bool) -> Result<()> {
    let work = work.clone();
    db::write(pool,move |tx| {
        let work=work.clone();
        Box::pin(async move {
            for lease in &work.leases {lease.guard(tx).await?;}
            let own=work.leases.iter().find(|l|l.resource.starts_with("work:")).unwrap();
            let batch:Option<String>=if work.kind=="traffic" {sqlx::query_scalar("SELECT state FROM traffic_sample_batches WHERE sample_id=$1 AND payload_digest IS NOT NULL").bind(&work.id).fetch_optional(&mut **tx).await?} else {None};
            let state=match batch.as_deref() {Some("complete")=>"complete",Some("ready")=>"queued",_ if success=>"complete",_=>"failed"};
            let changed=sqlx::query("UPDATE background_work SET state=$4,owner=NULL,finished_at=CASE WHEN $4='queued' THEN NULL ELSE clock_timestamp() END,available_at=CASE WHEN $4='queued' THEN clock_timestamp()+interval '1 second' ELSE available_at END,error_code=$5 WHERE id=$1 AND owner=$2 AND generation=$3 AND state='running'")
                .bind(&work.id).bind(&own.owner).bind(own.generation).bind(state).bind(if state=="queued"{Some("billing_deferred")}else if state=="failed"{Some("sampling_failed")}else{None}).execute(&mut **tx).await?.rows_affected();
            anyhow::ensure!(changed==1,"sampling ownership was lost");
            if work.kind=="traffic" && state!="queued" {
                sqlx::query("UPDATE online_samples SET pending=false WHERE cycle_id=$1 AND node_id=$2 AND pending").bind(work.cycle).bind(&work.node).execute(&mut **tx).await?;
            }
            for lease in &work.leases {lease.release(tx).await?;}
            Ok(())
        })
    }).await
}

pub async fn expire(pool: &PgPool) -> Result<usize> {
    db::write(pool,|tx|Box::pin(async move {
        let rows=sqlx::query("SELECT w.id,w.cycle_id,w.node_id,w.kind FROM background_work w WHERE w.state IN ('queued','running') AND (w.deadline_at<=clock_timestamp() OR NOT EXISTS(SELECT 1 FROM nodes n WHERE n.id=w.node_id AND n.state NOT IN ('deleting','delete_failed'))) AND NOT EXISTS(SELECT 1 FROM resource_leases l WHERE l.resource_key='work:'||w.id AND l.owner IS NOT NULL AND l.expires_at>clock_timestamp()) AND NOT (w.kind='traffic' AND EXISTS(SELECT 1 FROM traffic_sample_batches b WHERE b.sample_id=w.id AND b.payload_digest IS NOT NULL)) ORDER BY w.deadline_at,w.id LIMIT 256 FOR UPDATE OF w SKIP LOCKED").fetch_all(&mut **tx).await?;
        for row in &rows {
            sqlx::query("UPDATE background_work SET state='expired',owner=NULL,finished_at=clock_timestamp(),error_code='sampling_slot_expired' WHERE id=$1").bind(row.get::<String,_>("id")).execute(&mut **tx).await?;
            if row.get::<String,_>("kind")=="traffic" {
                sqlx::query("UPDATE online_samples SET pending=false,traffic_status='failed' WHERE cycle_id=$1 AND node_id=$2 AND pending").bind(row.get::<Uuid,_>("cycle_id")).bind(row.get::<String,_>("node_id")).execute(&mut **tx).await?;
            }
        }
        Ok(rows.len())
    })).await
}

pub async fn run_sampling(state: AppState, kind: &'static str) {
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _=timer.tick()=>{
                if let Err(error)=ensure_cycle(&state.pool,kind).await {tracing::warn!(kind,%error,"sampling cycle deferred");continue;}
                if let Err(error)=expire(&state.pool).await {tracing::warn!(%error,"sampling expiry deferred");}
                while tasks.len()<4 {
                    let work=match claim(&state.pool,kind,&state.instance_id).await {Ok(Some(work))=>work,Ok(None)=>break,Err(error)=>{tracing::warn!(kind,%error,"sampling claim deferred");break;}};
                    let app=state.clone();
                    tasks.spawn(async move {
                        execution::supervise(app.pool.clone(),work.leases.clone(),async {
                            let result=match kind {
                                "traffic"=>crate::traffic::sample_work(&app,&work).await,
                                "network"=>crate::node_limits::sample_work(&app,&work).await,
                                _=>unreachable!(),
                            };
                            if let Err(error)=&result {tracing::warn!(kind,node_id=%work.node,%error,"sampling work failed");}
                            if let Err(error)=finish(&app.pool,&work,result.is_ok()).await {tracing::warn!(work_id=%work.id,%error,"sampling completion deferred");}
                        }).await;
                    });
                }
            }
            Some(_)=tasks.join_next(),if !tasks.is_empty()=>{}
        }
    }
}
#[cfg(test)]
mod tests;
