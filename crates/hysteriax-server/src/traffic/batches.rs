//! Durable, bounded billing. A captured response survives process death; each
//! item's checkpoint, baseline, ledger row, usage and revocation commit together.
use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::db;

pub const CHUNK_SIZE: i64 = 500;

#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    Committed { items: usize, complete: bool },
    Finished,
    Waiting,
}

/// Reserve ordering before the remote read. Retrying with the same ID never
/// allocates a second sample or loses an already captured payload.
pub async fn reserve(pool: &PgPool, id: &str, node: &str, cycle: Option<Uuid>) -> Result<String> {
    let (id, node) = (id.to_owned(), node.to_owned());
    db::write(pool,move |tx| {
        let (id,node)=(id.clone(),node.clone());
        Box::pin(async move {
            let valid:Option<bool>=sqlx::query_scalar("SELECT true FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
                .bind(&node).fetch_optional(&mut **tx).await?;
            ensure!(valid==Some(true),"node is missing or being deleted");
            sqlx::query("INSERT INTO traffic_sample_batches(sample_id,node_id,cycle_id) VALUES($1,$2,$3) ON CONFLICT(sample_id) DO NOTHING")
                .bind(&id).bind(&node).bind(cycle).execute(&mut **tx).await?;
            let row=sqlx::query("SELECT node_id,cycle_id,state FROM traffic_sample_batches WHERE sample_id=$1")
                .bind(&id).fetch_one(&mut **tx).await?;
            ensure!(row.get::<String,_>("node_id")==node && row.get::<Option<Uuid>,_>("cycle_id")==cycle,"sample ID was reused for a different capture");
            Ok(row.get("state"))
        })
    }).await
}

/// Validate the entire response before persisting any item. A conflicting retry
/// cannot replace the original captured counters, even after billing completed.
pub async fn seal(
    pool: &PgPool,
    id: &str,
    instance: &str,
    counters: &Map<String, Value>,
) -> Result<()> {
    let parsed = super::parse_counters(counters)?;
    let canonical: Map<String, Value> = parsed
        .into_iter()
        .map(|(user, tx, rx)| (user, json!({"tx":tx,"rx":rx})))
        .collect();
    let payload = Value::Object(canonical);
    let digest = crate::security::token_digest(&format!("{instance}:{}", payload));
    let (id, instance) = (id.to_owned(), instance.to_owned());
    db::write(pool,move |tx| {
        let (id,instance,digest,payload)=(id.clone(),instance.clone(),digest.clone(),payload.clone());
        Box::pin(async move {
            let node:String=sqlx::query_scalar("SELECT node_id FROM traffic_sample_batches WHERE sample_id=$1")
                .bind(&id).fetch_one(&mut **tx).await?;
            let valid:Option<bool>=sqlx::query_scalar("SELECT true FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
                .bind(&node).fetch_optional(&mut **tx).await?;
            ensure!(valid==Some(true),"node is missing or being deleted");
            let row=sqlx::query("SELECT state,payload_digest,sampled_at,capture_deadline>clock_timestamp() AS timely FROM traffic_sample_batches WHERE sample_id=$1 FOR UPDATE")
                .bind(&id).fetch_one(&mut **tx).await?;
            if let Some(stored)=row.get::<Option<String>,_>("payload_digest") {
                ensure!(stored==digest,"sample counters changed; original capture retained");
                return Ok(());
            }
            ensure!(row.get::<String,_>("state")=="capturing" && row.get::<bool,_>("timely"),"capture expired or was discarded");
            let sampled_at:DateTime<Utc>=row.get("sampled_at");
            let count=sqlx::query("INSERT INTO traffic_sample_items(sample_id,node_id,user_id,tx_total,rx_total,sampled_at) SELECT $1,$2,key,(value->>'tx')::bigint,(value->>'rx')::bigint,$4 FROM jsonb_each($3::jsonb)")
                .bind(&id).bind(&node).bind(payload).bind(sampled_at).execute(&mut **tx).await?.rows_affected() as i64;
            sqlx::query("UPDATE traffic_sample_batches SET instance_id=$2,payload_digest=$3,item_count=$4,state='ready' WHERE sample_id=$1")
                .bind(&id).bind(&instance).bind(&digest).bind(count).execute(&mut **tx).await?;
            Ok(())
        })
    }).await
}

/// Only uncaptured work can expire. Captured counters remain available for
/// another process; a collection timeout must never discard ready billing work.
#[cfg(test)]
pub async fn captured(pool: &PgPool, id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM traffic_sample_batches WHERE sample_id=$1 AND payload_digest IS NOT NULL)")
        .bind(id).fetch_one(pool).await?)
}

pub async fn discard_capture(pool: &PgPool, id: &str) -> Result<()> {
    let id = id.to_owned();
    db::write(pool,move |tx| {
        let id=id.clone();
        Box::pin(async move {
            sqlx::query("UPDATE traffic_sample_batches SET state='discarded',completed_at=clock_timestamp() WHERE sample_id=$1 AND state='capturing'")
                .bind(id).execute(&mut **tx).await?;
            Ok(())
        })
    }).await
}

pub async fn expire_captures(pool: &PgPool) -> Result<()> {
    sqlx::query("UPDATE traffic_sample_batches SET state='discarded',completed_at=clock_timestamp() WHERE state='capturing' AND capture_deadline<=clock_timestamp()")
        .execute(pool).await?;
    Ok(())
}

/// All resources are locked node -> batch -> sorted users -> item/baseline.
/// A quota reset locks the same user row and therefore divides billing by
/// database admission order without clearing per-node counter baselines.
pub async fn process(pool: &PgPool, id: &str) -> Result<Progress> {
    let id = id.to_owned();
    db::write(pool,move |tx| {
        let id=id.clone();
        Box::pin(async move {
            let Some(node)=sqlx::query_scalar::<_,String>("SELECT node_id FROM traffic_sample_batches WHERE sample_id=$1")
                .bind(&id).fetch_optional(&mut **tx).await? else {return Ok(Progress::Finished);};
            let node_state:Option<String>=sqlx::query_scalar("SELECT state FROM nodes WHERE id=$1 FOR NO KEY UPDATE SKIP LOCKED")
                .bind(&node).fetch_optional(&mut **tx).await?;
            let Some(_node_state)=node_state else {return Ok(Progress::Waiting);};
            let Some(batch)=sqlx::query("SELECT * FROM traffic_sample_batches WHERE sample_id=$1 FOR NO KEY UPDATE SKIP LOCKED")
                .bind(&id).fetch_optional(&mut **tx).await? else {return Ok(Progress::Waiting);};
            let status:String=batch.get("state");
            if matches!(status.as_str(),"complete"|"discarded") {return Ok(Progress::Finished);}
            // New captures stop at deletion, but durable captured counters
            // finish billing before node history is removed.
            if status!="ready" {return Ok(Progress::Waiting);}
            let order:i64=batch.get("sample_order");
            let prior:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM traffic_sample_batches WHERE node_id=$1 AND sample_order<$2 AND state IN ('capturing','ready'))")
                .bind(&node).bind(order).fetch_one(&mut **tx).await?;
            if prior {return Ok(Progress::Waiting);}
            let instance:String=batch.get("instance_id");
            let sampled_at:DateTime<Utc>=batch.get("sampled_at");
            let items=sqlx::query("SELECT user_id,tx_total,rx_total FROM traffic_sample_items WHERE sample_id=$1 AND processed_at IS NULL ORDER BY user_id LIMIT $2")
                .bind(&id).bind(CHUNK_SIZE).fetch_all(&mut **tx).await?;
            let ids:Vec<String>=items.iter().map(|item|item.get("user_id")).collect();
            // Explicitly lock users in ID order before touching any counter.
            sqlx::query("SELECT id FROM users WHERE id=ANY($1::text[]) ORDER BY id FOR NO KEY UPDATE")
                .bind(&ids).fetch_all(&mut **tx).await?;
            for item in &items {
                let user:String=item.get("user_id");
                let assigned:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM node_assignments WHERE node_id=$1 AND user_id=$2)")
                    .bind(&node).bind(&user).fetch_one(&mut **tx).await?;
                if !assigned {continue;}
                let current_tx:i64=item.get("tx_total");
                let current_rx:i64=item.get("rx_total");
                let baseline=sqlx::query("SELECT instance_id,tx_total,rx_total,sample_order FROM traffic_baselines WHERE node_id=$1 AND user_id=$2 FOR UPDATE")
                    .bind(&node).bind(&user).fetch_optional(&mut **tx).await?;
                if baseline.as_ref().is_some_and(|baseline|baseline.get::<i64,_>("sample_order")>=order) {continue;}
                let previous=baseline.as_ref().map(|row|(row.get::<String,_>("instance_id"),row.get::<i64,_>("tx_total") as u64,row.get::<i64,_>("rx_total") as u64));
                let delta=super::counter_delta(previous.as_ref().map(|(instance,tx,rx)|(instance.as_str(),*tx,*rx)),&instance,current_tx as u64,current_rx as u64);
                let delta_tx=delta.tx as i64;
                let delta_rx=delta.rx as i64;
                let delta_bytes=delta_tx.saturating_add(delta_rx);
                sqlx::query("INSERT INTO traffic_baselines(node_id,user_id,instance_id,tx_total,rx_total,sampled_at,sample_order) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(node_id,user_id) DO UPDATE SET instance_id=EXCLUDED.instance_id,tx_total=EXCLUDED.tx_total,rx_total=EXCLUDED.rx_total,sampled_at=EXCLUDED.sampled_at,sample_order=EXCLUDED.sample_order")
                    .bind(&node).bind(&user).bind(&instance).bind(current_tx).bind(current_rx).bind(sampled_at).bind(order).execute(&mut **tx).await?;
                sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,gap_reason,sampled_at,baseline_only,sample_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
                    .bind(Uuid::new_v4().to_string()).bind(&node).bind(&user).bind(&instance).bind(current_tx).bind(current_rx).bind(delta_tx).bind(delta_rx).bind(delta.gap_reason).bind(sampled_at).bind(previous.is_none()).bind(&id).execute(&mut **tx).await?;
                sqlx::query("UPDATE users SET usage_bytes=usage_bytes+least($1,9223372036854775807-usage_bytes),updated_at=clock_timestamp() WHERE id=$2")
                    .bind(delta_bytes).bind(&user).execute(&mut **tx).await?;
                enforce_user(tx,&user).await?;
                if let Some(reason)=delta.gap_reason {super::record_gap_in_tx(tx,&node,reason,&sampled_at).await?;}
            }
            sqlx::query("UPDATE traffic_sample_items SET processed_at=clock_timestamp() WHERE sample_id=$1 AND user_id=ANY($2::text[]) AND processed_at IS NULL")
                .bind(&id).bind(&ids).execute(&mut **tx).await?;
            let processed:i64=batch.get::<i64,_>("processed_count")+items.len() as i64;
            let complete=processed==batch.get::<i64,_>("item_count");
            sqlx::query("UPDATE traffic_sample_batches SET processed_count=$2,state=CASE WHEN $3 THEN 'complete' ELSE 'ready' END,completed_at=CASE WHEN $3 THEN clock_timestamp() ELSE NULL END WHERE sample_id=$1")
                .bind(&id).bind(processed).bind(complete).execute(&mut **tx).await?;
            if complete {
                sqlx::query("UPDATE nodes SET state=CASE WHEN state='unreachable' THEN 'deployed' ELSE state END,last_seen_at=greatest(last_seen_at,$2),last_sample_at=greatest(last_sample_at,$2),updated_at=clock_timestamp() WHERE id=$1 AND NOT EXISTS(SELECT 1 FROM traffic_sample_batches newer WHERE newer.node_id=$1 AND newer.sample_order>$3 AND newer.state='complete')")
                    .bind(&node).bind(sampled_at).bind(order).execute(&mut **tx).await?;
                sqlx::query("UPDATE online_samples SET traffic_status='ok' WHERE node_id=$1 AND cycle_id=$2")
                    .bind(&node).bind(batch.get::<Option<Uuid>,_>("cycle_id")).execute(&mut **tx).await?;
                sqlx::query("UPDATE data_gaps SET resolved_at=clock_timestamp() WHERE node_id=$1 AND resolved_at IS NULL AND opened_at<=$2")
                    .bind(&node).bind(sampled_at).execute(&mut **tx).await?;
            }
            Ok(Progress::Committed{items:items.len(),complete})
        })
    }).await
}

pub async fn enforce_user(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    user: &str,
) -> Result<()> {
    let marked:Option<String>=sqlx::query_scalar("UPDATE users SET access_kick_enqueued_at=clock_timestamp(),updated_at=clock_timestamp() WHERE id=$1 AND access_kick_enqueued_at IS NULL AND (NOT enabled OR expires_at<=clock_timestamp() OR usage_bytes>=quota_bytes) RETURNING id")
        .bind(user).fetch_optional(&mut **tx).await?;
    if marked.is_some() {
        let nodes:Vec<(String,String)>=sqlx::query_as("SELECT n.id,n.state FROM node_assignments a JOIN nodes n ON n.id=a.node_id WHERE a.user_id=$1 ORDER BY n.id")
            .bind(user).fetch_all(&mut **tx).await?;
        for (node, state) in nodes {
            let payload = json!({"user_id":user,"kick_reason":"access_restricted"});
            if state == "deleting" {
                // Keep the obligation if removal later fails, but do not add
                // new remote work behind the running removal operation.
                crate::kick_requests::prepare(tx, &node, &payload).await?;
            } else {
                crate::api::enqueue_job_with_payload_in_tx(tx, "kick", Some(&node), None, payload)
                    .await?;
            }
        }
    }
    Ok(())
}

pub async fn drain(pool: &PgPool, id: &str) -> Result<bool> {
    loop {
        match process(pool, id).await? {
            Progress::Committed { complete: true, .. } | Progress::Finished => return Ok(true),
            Progress::Waiting => return Ok(false),
            Progress::Committed { .. } => tokio::task::yield_now().await,
        }
    }
}

pub async fn process_next(pool: &PgPool) -> Result<bool> {
    let ids:Vec<String>=sqlx::query_scalar("SELECT b.sample_id FROM traffic_sample_batches b WHERE b.state='ready' AND NOT EXISTS(SELECT 1 FROM traffic_sample_batches prior WHERE prior.node_id=b.node_id AND prior.sample_order<b.sample_order AND prior.state IN ('capturing','ready')) ORDER BY b.sample_order LIMIT 16")
        .fetch_all(pool).await?;
    for id in ids {
        if let Progress::Committed { .. } = process(pool, &id).await? {
            return Ok(true);
        }
    }
    Ok(false)
}
