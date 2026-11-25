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

pub(crate) mod history_retention;
pub(crate) mod retention;

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

const CURRENT_CYCLE: &str = "SELECT id FROM sampling_cycles WHERE kind=$1 AND period_start=(SELECT to_timestamp(floor(extract(epoch FROM clock_timestamp())/$2)*$2))";

#[tracing::instrument(name="sampling_cycle", skip(pool,kind), fields(sampling_kind=kind))]
pub async fn ensure_cycle(pool: &PgPool, kind: &str) -> Result<Uuid> {
    let seconds = match kind {
        "traffic" | "network" => crate::runtime_config::sampling_interval_seconds() as i64,
        "proxy" => 60,
        _ => anyhow::bail!("unsupported sampling kind"),
    };
    let mut connection = pool.acquire().await?;
    if !execution::has_scope() {
        let existing: Option<Uuid> = sqlx::query_scalar(CURRENT_CYCLE)
            .bind(kind)
            .bind(seconds as f64)
            .fetch_optional(&mut *connection)
            .await?;
        if let Some(id) = existing {
            return Ok(id);
        }
    }
    let kind = kind.to_owned();
    let id = Uuid::new_v4();
    drop(connection);
    db::write(pool, move |tx| {
        let kind = kind.clone();
        Box::pin(async move {
            let seconds = match kind.as_str() {
                "traffic" | "network" => crate::runtime_config::sampling_interval_seconds() as i64,
                "proxy" => 60_i64,
                _ => anyhow::bail!("unsupported sampling kind"),
            };
            let period: DateTime<Utc> = sqlx::query_scalar(
                "SELECT to_timestamp(floor(extract(epoch FROM clock_timestamp())/$1)*$1)",
            )
            .bind(seconds as f64)
            .fetch_one(&mut **tx)
            .await?;
            let deadline = period + chrono::Duration::seconds(seconds);
            let created = sqlx::query(
                "INSERT INTO sampling_cycles(id,kind,period_start,deadline_at,expected_nodes) VALUES($1,$2,$3,$4,0) ON CONFLICT(kind,period_start) DO NOTHING",
            )
            .bind(id)
            .bind(&kind)
            .bind(period)
            .bind(deadline)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                == 1;
            if !created {
                return Ok(sqlx::query_scalar(
                    "SELECT id FROM sampling_cycles WHERE kind=$1 AND period_start=$2",
                )
                .bind(&kind)
                .bind(period)
                .fetch_one(&mut **tx)
                .await?);
            }
            let ids: Vec<String> = if kind == "traffic" {
                sqlx::query_scalar("SELECT id FROM nodes WHERE deployed_revision IS NOT NULL AND state NOT IN ('deleting','delete_failed') ORDER BY id")
                    .fetch_all(&mut **tx)
                    .await?
            } else if kind == "proxy" {
                sqlx::query_scalar("SELECT id FROM nodes WHERE deployed_revision IS NOT NULL AND state NOT IN ('deleting','delete_failed') AND mtls_state='ready' ORDER BY id")
                    .fetch_all(&mut **tx)
                    .await?
            } else {
                sqlx::query_scalar("SELECT n.id FROM nodes n JOIN node_packages p ON p.node_id=n.id WHERE (n.deployed_revision IS NOT NULL OR p.config->>'quota_bytes' IS NOT NULL) AND n.state NOT IN ('deleting','delete_failed') ORDER BY n.id")
                    .fetch_all(&mut **tx)
                    .await?
            };
            let expected = ids.len() as i64;
            sqlx::query("UPDATE sampling_cycles SET expected_nodes=$2 WHERE id=$1")
                .bind(id)
                .bind(expected)
                .execute(&mut **tx)
                .await?;
            for chunk in ids.chunks(1_000) {
                sqlx::query("INSERT INTO background_work(id,kind,cycle_id,node_id,available_at,deadline_at) SELECT $1::uuid::text||':'||node,$2,$1::uuid,node,$3,$4 FROM unnest($5::text[]) node")
                    .bind(id)
                    .bind(&kind)
                    .bind(period)
                    .bind(deadline)
                    .bind(chunk)
                    .execute(&mut **tx)
                    .await?;
                if kind == "traffic" {
                    sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,status,connections,cycle_started_at,expected_nodes,pending) SELECT $1,node,$2,'failed','{}',$2,$3,true FROM unnest($4::text[]) node")
                        .bind(id)
                        .bind(period)
                        .bind(expected)
                        .bind(chunk)
                        .execute(&mut **tx)
                        .await?;
                }
            }
            Ok(id)
        })
    })
    .await
}

/// Recover read-only work only while its slot is current. Already captured
/// traffic is always eligible: recovery must finish billing rather than read SSH.
const SAMPLING_CANDIDATES: &str = "FROM background_work w JOIN sampling_cycles c ON c.id=w.cycle_id JOIN nodes n ON n.id=w.node_id WHERE w.kind=$1 AND w.cycle_id IS NOT NULL AND w.state IN ('queued','running') AND n.state NOT IN ('deleting','delete_failed') AND w.available_at<=clock_timestamp() AND (w.deadline_at>clock_timestamp() OR (w.kind='traffic' AND EXISTS(SELECT 1 FROM traffic_sample_batches b WHERE b.sample_id=w.id AND b.payload_digest IS NOT NULL))) AND NOT EXISTS(SELECT 1 FROM resource_leases l WHERE l.resource_key=ANY(ARRAY['work:'||w.id,'node:'||w.node_id]) AND (l.blocked OR (l.owner IS NOT NULL AND l.expires_at>clock_timestamp()) OR EXISTS(SELECT 1 FROM execution_operations o WHERE o.resource_key=l.resource_key AND o.state IN ('dispatching','reconciling','needs_attention'))))";

pub async fn claim(pool: &PgPool, kind: &str, owner: &str) -> Result<Option<Work>> {
    let mut poll = maintenance::Poll::new(&format!("sampling:{kind}"), "claim");
    let result = claim_sampling(pool, kind, owner, &poll, true).await;
    poll.finish(
        result.is_ok(),
        result.as_ref().is_ok_and(|work| work.is_some()),
    );
    result
}

async fn claim_sampling(
    pool: &PgPool,
    kind: &str,
    owner: &str,
    poll: &maintenance::Poll,
    preflight: bool,
) -> Result<Option<Work>> {
    let mut connection = pool.acquire().await?;
    if preflight && !execution::has_scope() {
        let present: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT EXISTS(SELECT 1 {SAMPLING_CANDIDATES})"
        )))
        .bind(kind)
        .fetch_one(&mut *connection)
        .await?;
        if !present {
            poll.preflight_empty();
            return Ok(None);
        }
    }
    #[cfg(test)]
    maintenance::sampling_preflight().await;
    poll.transaction();
    let (kind, owner) = (kind.to_owned(), owner.to_owned());
    let result = db::write_after_probe(pool,connection,move |tx| {
        let (kind,owner)=(kind.clone(),owner.clone());
        Box::pin(async move {
            // Keep due-time order, but give each kind a stable, different node
            // order within a slot to reduce collisions on shared node leases.
            let rows=sqlx::query(sqlx::AssertSqlSafe(format!("SELECT w.*,c.period_start,c.expected_nodes, (w.kind='traffic' AND EXISTS(SELECT 1 FROM traffic_sample_batches b WHERE b.sample_id=w.id AND b.payload_digest IS NOT NULL)) has_capture {SAMPLING_CANDIDATES} ORDER BY w.available_at,hashtext(w.kind||':'||w.node_id),w.id LIMIT 32 FOR UPDATE OF w SKIP LOCKED")))
                .bind(&kind).fetch_all(&mut **tx).await?;
            if !rows.is_empty() {
                // ROLLBACK TO keeps the savepoint. The enclosing transaction
                // releases it when the winning claim commits.
                sqlx::query("SAVEPOINT sampling_candidate").execute(&mut **tx).await?;
            }
            let mut rejected = 0_u64;
            for row in rows {
                let id:String=row.get("id");let node:String=row.get("node_id");
                let resources=vec![format!("node:{node}"),format!("work:{id}")];
                let Some(leases)=execution::acquire_all(tx,&resources,&owner,&id).await? else {
                    sqlx::query("ROLLBACK TO SAVEPOINT sampling_candidate").execute(&mut **tx).await?;
                    rejected += 1;
                    continue;
                };
                let generation=leases.iter().find(|lease|lease.resource.starts_with("work:")).unwrap().generation;
                let mut payload:Value=row.get("payload");
                if payload.get("captured_at").is_none() {
                    let snapshot=sqlx::query("SELECT clock_timestamp() captured_at,n.deployed_revision,p.generation FROM nodes n LEFT JOIN node_packages p ON p.node_id=n.id WHERE n.id=$1")
                        .bind(&node).fetch_one(&mut **tx).await?;
                    payload["captured_at"]=json!(snapshot.get::<DateTime<Utc>,_>("captured_at"));
                    if kind=="proxy" { payload["deployed_revision"]=json!(snapshot.get::<Option<i64>,_>("deployed_revision")); }
                    if kind=="network" { payload["capture_generation"]=json!(snapshot.try_get::<i64,_>("generation")?); }
                }
                // Acquiring resources or loading metadata may have consumed the
                // slot. Captured billing remains recoverable after its deadline.
                let changed=sqlx::query("UPDATE background_work w SET state='running',owner=$2,generation=$3,payload=$4,started_at=coalesce(started_at,clock_timestamp()) WHERE id=$1 AND (deadline_at>clock_timestamp() OR (kind='traffic' AND EXISTS(SELECT 1 FROM traffic_sample_batches b WHERE b.sample_id=w.id AND b.payload_digest IS NOT NULL)))")
                    .bind(&id).bind(&owner).bind(generation).bind(&payload).execute(&mut **tx).await?.rows_affected();
                if changed!=1 {
                    sqlx::query("ROLLBACK TO SAVEPOINT sampling_candidate").execute(&mut **tx).await?;
                    rejected += 1;
                    continue;
                }
                return Ok((Some(Work{id,kind,cycle:row.get("cycle_id"),node,period_start:row.get("period_start"),expected:row.get("expected_nodes"),payload,leases}),rejected,row.get::<bool,_>("has_capture")));
            }
            Ok((None,rejected,false))
        })
    }).await;
    if let Ok((work, rejected, captured)) = &result {
        poll.claim_details(work.is_none(), *rejected, *captured);
    }
    result.map(|(work, _, _)| work)
}

#[derive(Debug, thiserror::Error)]
#[error("sampling slot expired before remote collection")]
pub struct SlotExpired;

/// Admission is checked immediately before collection, so a locally paused
/// claimant cannot start an uncaptured remote read after its slot has closed.
/// Once collection has started, its bounded result may arrive after the slot.
pub async fn admit_read(pool: &PgPool, work: &Work) -> Result<()> {
    execution::admit_sampling_read(pool, work).await
}

pub async fn finish(pool: &PgPool, work: &Work, success: bool) -> Result<()> {
    finish_or_defer(pool, work, Some(success)).await
}

#[derive(Debug, thiserror::Error)]
#[error("sampling admission stopped for shutdown")]
pub struct Draining;

/// Yield an unstarted capture without inventing a failed observation. A clear
/// marker must never be released this way: it needs capture or uncertainty.
async fn finish_or_defer(pool: &PgPool, work: &Work, success: Option<bool>) -> Result<()> {
    let work = work.clone();
    db::write(pool,move |tx| {
        let work=work.clone();
        Box::pin(async move {
            execution::guard_all(tx,&work.leases).await?;
            let own=work.leases.iter().find(|l|l.resource.starts_with("work:")).unwrap();
            let batch:Option<String>=if work.kind=="traffic" {sqlx::query_scalar("SELECT state FROM traffic_sample_batches WHERE sample_id=$1 AND payload_digest IS NOT NULL").bind(&work.id).fetch_optional(&mut **tx).await?} else {None};
            if success.is_none() {
                let requested:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM traffic_sample_batches WHERE sample_id=$1 AND clear_requested_at IS NOT NULL AND payload_digest IS NULL)")
                    .bind(&work.id).fetch_one(&mut **tx).await?;
                anyhow::ensure!(!requested,"cannot defer an unresolved traffic clear");
            }
            let state=match batch.as_deref() {Some("complete")=>"complete",Some("ready")=>"queued",_ if success.is_none()=>"queued",_ if success==Some(true)=>"complete",_=>"failed"};
            let changed=sqlx::query("UPDATE background_work SET state=$4,owner=NULL,finished_at=CASE WHEN $4='queued' THEN NULL ELSE clock_timestamp() END,available_at=CASE WHEN $4='queued' THEN clock_timestamp()+interval '1 second' ELSE available_at END,error_code=$5 WHERE id=$1 AND owner=$2 AND generation=$3 AND state='running'")
                .bind(&work.id).bind(&own.owner).bind(own.generation).bind(state).bind(if state=="queued"{Some(if success.is_none(){"shutdown_deferred"}else{"billing_deferred"})}else if state=="failed"{Some("sampling_failed")}else{None}).execute(&mut **tx).await?.rows_affected();
            anyhow::ensure!(changed==1,"sampling ownership was lost");
            if work.kind=="traffic" && state!="queued" {
                sqlx::query("UPDATE online_samples SET pending=false WHERE cycle_id=$1 AND node_id=$2 AND pending").bind(work.cycle).bind(&work.node).execute(&mut **tx).await?;
            }
            for lease in &work.leases {anyhow::ensure!(lease.release(tx).await?,"sampling resource has an unverified remote operation");}
            execution::finish_current_on(tx,&format!("work:{}",work.id),match state {"queued"=>execution::Disposition::Yielded,"failed"=>execution::Disposition::Failed,_=>execution::Disposition::Complete})?;
            Ok(())
        })
    }).await
}

const EXPIRY_FILTER: &str = r#"w.cycle_id IS NOT NULL
                      AND w.state IN ('queued','running')
                      AND (w.deadline_at <= clock_timestamp() OR NOT EXISTS (
                          SELECT 1 FROM nodes n
                          WHERE n.id=w.node_id AND n.state NOT IN ('deleting','delete_failed')
                      ))
                      AND NOT EXISTS (
                          SELECT 1 FROM resource_leases l
                          WHERE l.resource_key='work:'||w.id
                            AND l.owner IS NOT NULL AND l.expires_at>clock_timestamp()
                      )
                      AND NOT (w.kind='traffic' AND EXISTS (
                          SELECT 1 FROM traffic_sample_batches b
                          WHERE b.sample_id=w.id AND b.payload_digest IS NOT NULL
                      ))"#;

pub async fn expire(pool: &PgPool) -> Result<usize> {
    let mut connection = pool.acquire().await?;
    if !execution::has_scope() {
        let sql = format!("SELECT EXISTS(SELECT 1 FROM background_work w WHERE {EXPIRY_FILTER})");
        let present: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(&mut *connection)
            .await?;
        if !present {
            return Ok(0);
        }
    }
    db::write_after_probe(pool, connection, |tx| {
        Box::pin(async move {
            // Work rows remain locked until both task and online status changes
            // commit. Competing expiry workers skip this bounded cohort; live
            // owners and captured billing retain their original exclusions.
            let expired: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                r#"
                WITH candidates AS MATERIALIZED (
                    SELECT w.id
                    FROM background_work w
                    WHERE {EXPIRY_FILTER}
                    ORDER BY w.deadline_at,w.id
                    LIMIT 256
                    FOR UPDATE OF w SKIP LOCKED
                ), expired AS (
                    UPDATE background_work w
                    SET state='expired',owner=NULL,finished_at=clock_timestamp(),
                        error_code='sampling_slot_expired'
                    FROM candidates c
                    WHERE w.id=c.id
                    RETURNING w.cycle_id,w.node_id,w.kind
                ), online AS (
                    UPDATE online_samples s
                    SET pending=false,traffic_status='failed'
                    FROM expired e
                    WHERE e.kind='traffic' AND s.cycle_id=e.cycle_id
                      AND s.node_id=e.node_id AND s.pending
                    RETURNING s.id
                )
                SELECT count(*) FROM expired
                "#
            )))
            .fetch_one(&mut **tx)
            .await?;
            Ok(expired as usize)
        })
    })
    .await
}

/// Expiry never sits on the sampling admission path. One loop per instance
/// handles all queues; row locks and SKIP LOCKED coordinate multiple instances.
pub async fn run_expiry(state: AppState) {
    tracing::info!(batch_limit = 256, "sampling expiry worker started");
    let mut shutdown = state.shutdown.subscribe();
    loop {
        if *shutdown.borrow() {
            return;
        }
        let started = std::time::Instant::now();
        let pause = match expire(&state.pool).await {
            Ok(expired) if expired > 0 => {
                tracing::info!(
                    expired,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "sampling expiry batch completed"
                );
                false
            }
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(%error, "sampling expiry deferred");
                true
            }
        };
        if pause {
            tokio::select! {
                _ = shutdown.wait_for(|stop| *stop) => return,
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {},
            }
        } else {
            tokio::task::yield_now().await;
        }
    }
}

pub async fn run_sampling(state: AppState, kind: &'static str) {
    let max_parallel = crate::runtime_config::background_worker_parallelism();
    let interval_seconds = match kind {
        "traffic" | "network" => crate::runtime_config::sampling_interval_seconds(),
        _ => 60,
    };
    let operation_timeout_seconds = crate::runtime_config::operation_timeout_seconds(match kind {
        "traffic" => 45,
        "network" => 30,
        _ => 15,
    });
    tracing::info!(
        kind,
        max_parallel,
        interval_seconds,
        operation_timeout_seconds,
        "sampling worker started"
    );
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tasks = tokio::task::JoinSet::new();
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {
            biased;
            _=async { let _ = shutdown.wait_for(|stop|*stop).await; }=>break,
            // Reap finished tasks before another due tick. Slow database
            // passes can leave the timer continuously ready; they must not
            // starve the JoinSet and permanently consume every worker slot.
            Some(result)=tasks.join_next(),if !tasks.is_empty()=>{
                if let Err(error)=result {tracing::error!(kind,%error,"sampling task failed");}
            }
            _=timer.tick()=>{
                if let Err(error)=ensure_cycle(&state.pool,kind).await {tracing::warn!(kind,%error,"sampling cycle deferred");continue;}
                while tasks.len()<max_parallel && !*state.shutdown.borrow() {
                    let claiming=std::time::Instant::now();
                    let work=match claim(&state.pool,kind,&state.instance_id).await {Ok(Some(work))=>work,Ok(None)=>break,Err(error)=>{tracing::warn!(kind,%error,"sampling claim deferred");break;}};
                    let claim_ms=claiming.elapsed().as_millis() as u64;
                    let app=state.clone();
                    tasks.spawn(async move {
                        execution::supervise(app.pool.clone(),work.leases.clone(),async {
                            let collecting=std::time::Instant::now();
                            let result=match kind {
                                "traffic"=>crate::traffic::sample_work(&app,&work).await,
                                "network"=>crate::node_limits::sample_work(&app,&work).await,
                                "proxy"=>crate::proxy_monitor::sample_work(&app,&work).await,
                                _=>unreachable!(),
                            };
                            let collection_ms=collecting.elapsed().as_millis() as u64;
                            let draining=result.as_ref().is_err_and(|error|error.is::<Draining>());
                            if let Err(error)=&result && !draining {tracing::warn!(kind,node_id=%work.node,work_id=%work.id,claim_ms,collection_ms,slot_expired=error.is::<SlotExpired>(),%error,"sampling work failed");}
                            let confirming=std::time::Instant::now();
                            let confirmation=if draining{finish_or_defer(&app.pool,&work,None).await}else{finish(&app.pool,&work,result.is_ok()).await};
                            if let Err(error)=confirmation {tracing::warn!(work_id=%work.id,claim_ms,collection_ms,confirmation_ms=confirming.elapsed().as_millis() as u64,%error,"sampling completion deferred");}
                            else {tracing::debug!(kind,node_id=%work.node,work_id=%work.id,claim_ms,collection_ms,confirmation_ms=confirming.elapsed().as_millis() as u64,success=result.is_ok(),"sampling work completed");}
                        }).await;
                    });
                }
            }
        }
    }
    tracing::info!(
        kind,
        in_flight = tasks.len(),
        "sampling admission stopped; draining claimed work"
    );
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            tracing::error!(kind,%error,"sampling task failed during drain");
        }
    }
    tracing::info!(kind, "sampling queue drained");
}
#[cfg(test)]
mod sampling_benchmark;
#[cfg(test)]
mod tests;

pub mod maintenance;
