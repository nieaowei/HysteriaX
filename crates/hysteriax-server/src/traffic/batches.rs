//! Durable, bounded billing. A captured response survives process death; each
//! item's checkpoint, baseline, ledger row, usage and revocation commit together.
use std::collections::HashMap;
use std::time::Instant;

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

#[derive(serde::Serialize)]
struct BilledCounter {
    record_id: String,
    user_id: String,
    tx_total: i64,
    rx_total: i64,
    delta_tx: i64,
    delta_rx: i64,
    delta_bytes: i64,
    gap_reason: Option<&'static str>,
}

/// Reserve ordering before the remote read. Retrying with the same ID never
/// allocates a second sample or loses an already captured payload.
#[cfg(test)]
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

/// Reserve an interval capture under the node lock, including captures that
/// fail before a remote request is sent.
pub async fn reserve_work(pool: &PgPool, work: &crate::background::Work) -> Result<String> {
    let work = work.clone();
    db::write(pool,move |tx| {
        let work=work.clone();
        Box::pin(async move {
            let valid:Option<bool>=sqlx::query_scalar("SELECT true FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
                .bind(&work.node).fetch_optional(&mut **tx).await?;
            ensure!(valid==Some(true),"node is missing or being deleted");
            sqlx::query("INSERT INTO traffic_sample_batches(sample_id,node_id,cycle_id) VALUES($1,$2,$3) ON CONFLICT(sample_id) DO NOTHING")
                .bind(&work.id).bind(&work.node).bind(work.cycle).execute(&mut **tx).await?;
            let row=sqlx::query("SELECT node_id,cycle_id,state FROM traffic_sample_batches WHERE sample_id=$1")
                .bind(&work.id).fetch_one(&mut **tx).await?;
            ensure!(row.get::<String,_>("node_id")==work.node && row.get::<Option<Uuid>,_>("cycle_id")==Some(work.cycle),"sample ID was reused for a different capture");
            Ok(row.get("state"))
        })
    }).await
}

/// Validate the entire response before persisting any item. A conflicting retry
/// cannot replace the original captured counters, even after billing completed.
#[cfg(test)]
pub async fn seal(
    pool: &PgPool,
    id: &str,
    instance: &str,
    counters: &Map<String, Value>,
) -> Result<()> {
    seal_inner(pool, id, instance, counters, None, None, false).await
}

#[cfg(test)]
pub async fn seal_response(
    pool: &PgPool,
    id: &str,
    instance: &str,
    counters: &Map<String, Value>,
    raw: &[u8],
) -> Result<()> {
    seal_inner(pool, id, instance, counters, None, Some(raw), false).await
}

/// Persist the already received response without reissuing the clear request.
/// Its original lease incarnation must still own both resources, but the
/// collection slot may have expired while the response was in flight.
/// Save with `instance_verified=false` before observing the service again;
/// only a matching follow-up instance permits upgrading it to true.
pub async fn seal_clear(
    pool: &PgPool,
    work: &crate::background::Work,
    instance: &str,
    counters: &Map<String, Value>,
    raw: &[u8],
    instance_verified: bool,
) -> Result<()> {
    for attempt in 0..=5 {
        match seal_inner(
            pool,
            &work.id,
            instance,
            counters,
            Some(work.clone()),
            Some(raw),
            instance_verified,
        )
        .await
        {
            Err(error) if attempt < 5 && retryable_capture_error(&error) => {
                tokio::time::sleep(std::time::Duration::from_millis(100 * (1 << attempt))).await;
            }
            result => return result,
        }
    }
    unreachable!("bounded persistence attempts always return")
}

fn retryable_capture_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<sqlx::Error>())
        .any(|error| match error {
            sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::WorkerCrashed
            | sqlx::Error::BeginFailed => true,
            sqlx::Error::Database(error) => error.code().is_some_and(|code| {
                matches!(
                    code.as_ref(),
                    "40001" | "40P01" | "55P03" | "57014" | "57P01" | "57P02" | "57P03"
                )
            }),
            _ => false,
        })
}

async fn seal_inner(
    pool: &PgPool,
    id: &str,
    instance: &str,
    counters: &Map<String, Value>,
    work: Option<crate::background::Work>,
    raw: Option<&[u8]>,
    instance_verified: bool,
) -> Result<()> {
    let parsed = super::parse_counters(counters)?;
    let canonical: Map<String, Value> = parsed
        .into_iter()
        .map(|(user, tx, rx)| (user, json!({"tx":tx,"rx":rx})))
        .collect();
    let payload = Value::Object(canonical);
    let digest = if let Some(raw) = raw {
        ensure!(
            &super::parse_response(raw)? == counters,
            "traffic response and parsed counters differ"
        );
        let text = std::str::from_utf8(raw)?;
        crate::security::token_digest(&format!("{instance}:{text}"))
    } else {
        crate::security::token_digest(&format!("{instance}:{}", payload))
    };
    // Unleased capture is a test fixture only. Runtime clear responses always
    // carry their original work and start unverified.
    let instance_verified = work.is_none() || instance_verified;
    let (id, instance) = (id.to_owned(), instance.to_owned());
    db::write(pool,move |tx| {
        let (id,instance,digest,payload,work)=(id.clone(),instance.clone(),digest.clone(),payload.clone(),work.clone());
        Box::pin(async move {
            let lease_identity=if let Some(work)=&work {
                let node_lease=work.leases.iter().find(|l|l.resource==format!("node:{}",work.node)).ok_or_else(||anyhow::anyhow!("missing node lease"))?;
                let work_lease=work.leases.iter().find(|l|l.resource==format!("work:{}",work.id)).ok_or_else(||anyhow::anyhow!("missing work lease"))?;
                ensure!(work.id==id && node_lease.work==id && work_lease.work==id && node_lease.owner==work_lease.owner && node_lease.epoch==work_lease.epoch,"mismatched traffic execution leases");
                let mut leases=[node_lease,work_lease];leases.sort_by(|a,b|a.resource.cmp(&b.resource));
                for lease in leases {lease.guard(tx).await?;}
                let valid:Option<bool>=sqlx::query_scalar("SELECT true FROM background_work WHERE id=$1 AND node_id=$2 AND kind='traffic' AND owner=$3 AND generation=$4 AND state='running' FOR UPDATE")
                    .bind(&id).bind(&work.node).bind(&work_lease.owner).bind(work_lease.generation).fetch_optional(&mut **tx).await?;
                ensure!(valid==Some(true),"traffic work ownership was lost");
                Some((work.node.clone(),work_lease.owner.clone(),work_lease.generation,node_lease.generation,work_lease.epoch.clone()))
            } else {None};
            let node:String=sqlx::query_scalar("SELECT node_id FROM traffic_sample_batches WHERE sample_id=$1")
                .bind(&id).fetch_one(&mut **tx).await?;
            if let Some((expected,_,_,_,_))=&lease_identity {ensure!(&node==expected,"traffic batch belongs to another node");}
            let valid:Option<bool>=sqlx::query_scalar("SELECT true FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
                .bind(&node).fetch_optional(&mut **tx).await?;
            ensure!(valid==Some(true),"node is missing or being deleted");
            let row=sqlx::query("SELECT state,payload_digest,sampled_at,capture_deadline>clock_timestamp() AS timely,clear_owner,clear_work_generation,clear_node_generation,clear_epoch FROM traffic_sample_batches b WHERE sample_id=$1 FOR UPDATE")
                .bind(&id).fetch_one(&mut **tx).await?;
            if let Some((_,owner,work_generation,node_generation,epoch))=&lease_identity {
                ensure!(row.get::<Option<String>,_>("clear_owner").as_ref()==Some(owner) && row.get::<Option<i64>,_>("clear_work_generation")==Some(*work_generation) && row.get::<Option<i64>,_>("clear_node_generation")==Some(*node_generation) && row.get::<Option<String>,_>("clear_epoch").as_ref()==Some(epoch),"clear request belongs to another execution");
            } else {
                ensure!(row.get::<Option<String>,_>("clear_owner").is_none(),"clear capture requires its original execution");
            }
            if let Some(stored)=row.get::<Option<String>,_>("payload_digest") {
                ensure!(stored==digest,"sample counters changed; original capture retained");
                if work.is_some() && instance_verified {
                    // Verification is monotonic and cannot rewrite a batch that
                    // recovery has already started billing conservatively.
                    sqlx::query("UPDATE traffic_sample_batches SET clear_instance_verified=true WHERE sample_id=$1 AND state='ready' AND processed_count=0 AND clear_instance_verified=false")
                        .bind(&id).execute(&mut **tx).await?;
                }
                return Ok(());
            }
            ensure!(row.get::<String,_>("state")=="capturing" && (lease_identity.is_some() || row.get::<bool,_>("timely")),"capture expired or was discarded");
            let sampled_at:DateTime<Utc>=row.get("sampled_at");
            // Validate and digest the complete response above, but persist only
            // identities that can be billed on this node. Existing captured
            // batches keep their original items and digest during recovery.
            let count=sqlx::query("INSERT INTO traffic_sample_items(sample_id,node_id,user_id,tx_total,rx_total,sampled_at) SELECT $1,$2,e.key,(e.value->>'tx')::bigint,(e.value->>'rx')::bigint,$4 FROM jsonb_each($3::jsonb) e JOIN users u ON u.id=e.key JOIN node_assignments a ON a.user_id=u.id AND a.node_id=$2 WHERE NOT (left(e.key,length('monitor-'||$2||'-'))='monitor-'||$2||'-' AND substring(e.key from length('monitor-'||$2||'-')+1) ~ '^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$') AND NOT (left(e.key,length('probe-'||$2||'-'))='probe-'||$2||'-' AND substring(e.key from length('probe-'||$2||'-')+1) ~ '^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$')")
                .bind(&id).bind(&node).bind(payload).bind(sampled_at).execute(&mut **tx).await?.rows_affected() as i64;
            sqlx::query("UPDATE traffic_sample_batches SET instance_id=$2,payload_digest=$3,item_count=$4,state='ready',clear_instance_verified=$5 WHERE sample_id=$1")
                .bind(&id).bind(&instance).bind(&digest).bind(count).bind(instance_verified).execute(&mut **tx).await?;
            if let Some(work)=&work {
                let mut leases=work.leases.iter().filter(|l|l.resource==format!("node:{}",work.node) || l.resource==format!("work:{}",work.id)).collect::<Vec<_>>();
                leases.sort_by(|a,b|a.resource.cmp(&b.resource));
                for lease in leases {lease.guard(tx).await?;}
            }
            Ok(())
        })
    }).await
}

/// Only uncaptured work can expire. Captured counters remain available for
/// another process; a collection timeout must never discard ready billing work.
pub async fn captured(pool: &PgPool, id: &str) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM traffic_sample_batches WHERE sample_id=$1 AND payload_digest IS NOT NULL)")
        .bind(id).fetch_one(pool).await?)
}

pub async fn expire_captures_on(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
) -> Result<usize> {
    let cutoff: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    let ids:Vec<String>=sqlx::query_scalar("SELECT b.sample_id FROM traffic_sample_batches b WHERE b.state='capturing' AND b.capture_deadline<=$1 AND NOT EXISTS(SELECT 1 FROM resource_leases l WHERE l.resource_key='work:'||b.sample_id AND l.owner IS NOT NULL AND l.expires_at>$1) ORDER BY b.capture_deadline,b.sample_id LIMIT 256 FOR UPDATE OF b SKIP LOCKED")
        .bind(cutoff).fetch_all(&mut **tx).await?;
    for id in &ids {
        super::clear::uncertain_on(tx, id).await?;
    }
    sqlx::query("UPDATE traffic_sample_batches SET state='discarded',completed_at=clock_timestamp() WHERE sample_id=ANY($1::text[])")
        .bind(&ids).execute(&mut **tx).await?;
    Ok(ids.len())
}
#[cfg(test)]
pub async fn expire_captures(pool: &PgPool) -> Result<()> {
    db::write(pool, |tx| {
        Box::pin(async move { expire_captures_on(tx).await.map(|_| ()) })
    })
    .await
}

/// All resources are locked node -> batch -> sorted users -> item/baseline.
/// A quota reset locks the same user row and therefore divides billing by
/// database admission order without clearing per-node counter baselines.
pub async fn process(pool: &PgPool, id: &str) -> Result<Progress> {
    let sample_id = id.to_owned();
    let started = Instant::now();
    let id = sample_id.clone();
    let result = db::write(pool,move |tx| {
        let id=id.clone();
        Box::pin(async move {
            let attempt_started=Instant::now();
            let Some((node,locked_node))=sqlx::query_as::<_,(String,Option<String>)>("SELECT b.node_id,n.id FROM traffic_sample_batches b LEFT JOIN LATERAL (SELECT id FROM nodes WHERE id=b.node_id FOR NO KEY UPDATE SKIP LOCKED) n ON true WHERE b.sample_id=$1")
                .bind(&id).fetch_optional(&mut **tx).await? else {return Ok(Progress::Finished);};
            if locked_node.is_none() {return Ok(Progress::Waiting);}
            let Some(batch)=sqlx::query("SELECT b.*, EXISTS(SELECT 1 FROM traffic_sample_batches old WHERE old.node_id=b.node_id AND old.sample_order<b.sample_order AND old.state IN ('capturing','ready')) AS has_prior, CASE WHEN b.interval_checked THEN b.interval_restart ELSE coalesce((SELECT old.instance_id<>b.instance_id FROM traffic_sample_batches old WHERE old.node_id=b.node_id AND old.sample_order<b.sample_order AND old.state='complete' AND old.instance_id IS NOT NULL ORDER BY old.sample_order DESC LIMIT 1),false) END AS detected_restart FROM traffic_sample_batches b WHERE sample_id=$1 FOR NO KEY UPDATE OF b SKIP LOCKED")
                .bind(&id).fetch_optional(&mut **tx).await? else {return Ok(Progress::Waiting);};
            let status:String=batch.get("state");
            if matches!(status.as_str(),"complete"|"discarded") {return Ok(Progress::Finished);}
            // New captures stop at deletion, but durable captured counters
            // finish billing before node history is removed.
            if status!="ready" {return Ok(Progress::Waiting);}
            let order:i64=batch.get("sample_order");
            if batch.get::<bool,_>("has_prior") {return Ok(Progress::Waiting);}
            let instance:String=batch.get("instance_id");
            let sampled_at:DateTime<Utc>=batch.get("sampled_at");
            let instance_verified:bool=batch.get("clear_instance_verified");
            let interval_unverified=!instance_verified;
            let interval_restart:bool=batch.get("detected_restart");
            if !batch.get::<bool,_>("interval_checked") {
                sqlx::query("WITH checked AS (UPDATE traffic_sample_batches SET interval_checked=true,interval_restart=$2 WHERE sample_id=$1 RETURNING node_id,sampled_at) INSERT INTO data_gaps(id,node_id,opened_at,reason) SELECT gen_random_uuid()::text,node_id,sampled_at,reason FROM checked CROSS JOIN (VALUES ('traffic_clear_instance_unverified', $3::boolean),('node_restarted_between_samples',$2::boolean)) AS gaps(reason,needed) WHERE needed")
                    .bind(&id).bind(interval_restart).bind(interval_unverified).execute(&mut **tx).await?;
            }
            let items=sqlx::query("SELECT user_id,tx_total,rx_total FROM traffic_sample_items WHERE sample_id=$1 AND processed_at IS NULL ORDER BY user_id LIMIT $2")
                .bind(&id).bind(CHUNK_SIZE).fetch_all(&mut **tx).await?;
            let ids:Vec<String>=items.iter().map(|item|item.get("user_id")).collect();
            let user_lock_started=Instant::now();
            let mut billed=Vec::new();
            let mut user_lock_ms=0;
            if !ids.is_empty() {
                sqlx::query("SELECT id FROM users WHERE id=ANY($1::text[]) ORDER BY id FOR NO KEY UPDATE")
                    .bind(&ids).fetch_all(&mut **tx).await?;
                user_lock_ms=user_lock_started.elapsed().as_millis() as u64;
                // Membership is read after the user locks; baselines are locked
                // in the same sorted order without an extra database round trip.
                let assignments:HashMap<String,Option<i64>>=sqlx::query_as::<_,(String,Option<i64>)>("SELECT a.user_id,b.sample_order FROM node_assignments a LEFT JOIN LATERAL (SELECT sample_order FROM traffic_baselines WHERE node_id=a.node_id AND user_id=a.user_id FOR UPDATE) b ON true WHERE a.node_id=$1 AND a.user_id=ANY($2::text[]) ORDER BY a.user_id")
                    .bind(&node).bind(&ids).fetch_all(&mut **tx).await?.into_iter().collect();
                for item in &items {
                    let user:String=item.get("user_id");
                    let Some(baseline)=assignments.get(&user) else {continue;};
                    if baseline.is_some_and(|previous|previous>=order) {continue;}
                    let current_tx:i64=item.get("tx_total");
                    let current_rx:i64=item.get("rx_total");
                    billed.push(BilledCounter {
                        record_id:Uuid::new_v4().to_string(),user_id:user,tx_total:current_tx,rx_total:current_rx,
                        delta_tx:current_tx,delta_rx:current_rx,delta_bytes:current_tx.saturating_add(current_rx),
                        gap_reason:if interval_unverified {Some("traffic_clear_instance_unverified")} else if interval_restart {Some("node_restarted_between_samples")} else {None},
                    });
                }
            }
            if !billed.is_empty() {
                let payload=serde_json::to_value(&billed)?;
                sqlx::query(include_str!("billing_page.sql"))
                    .bind(&node).bind(&instance).bind(sampled_at).bind(order).bind(&payload).bind(&id)
                    .execute(&mut **tx).await?;
                let billed_ids:Vec<String>=billed.iter().map(|counter|counter.user_id.clone()).collect();
                super::access::admit_many_on(tx,&billed_ids).await?;

            }
            let processed:i64=batch.get::<i64,_>("processed_count")+items.len() as i64;
            let complete=processed==batch.get::<i64,_>("item_count");
            sqlx::query(include_str!("billing_checkpoint.sql"))
                .bind(&id).bind(&ids).bind(processed).bind(complete)
                .bind(if interval_restart || interval_unverified {"failed"} else {"ok"})
                .execute(&mut **tx).await?;
            tracing::debug!(sample_id=%id,node_id=%node,items=items.len(),billed_items=billed.len(),user_lock_ms,attempt_ms=attempt_started.elapsed().as_millis() as u64,"traffic billing page prepared");
            Ok(Progress::Committed{items:items.len(),complete})
        })
    }).await;
    if let Ok(Progress::Committed { items, complete }) = &result {
        tracing::info!(%sample_id,items,complete,elapsed_ms=started.elapsed().as_millis() as u64,"traffic billing page committed");
    }
    result
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
