//! Bounded probe renewal under node, CA and work leases. No remote requests are
//! made here; immutable signing material and work confirmation commit together.
use super::probe::{self, PreparedProbe};
use crate::{
    background::maintenance::{self, Work},
    db,
    state::AppState,
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgPool, Postgres, Row, Transaction};

const PAGE: i64 = 100;
const KIND: &str = "mtls-probe-renewal";
const SCAN_KIND: &str = "mtls-probe-admission";
const SCAN_KEY: &str = "mtls:probes";
const DUE: &str = "SELECT p.node_id,p.ca_id,p.ca_version,p.credential_id,p.credential_version FROM mtls_probe_versions p JOIN mtls_ca_generations g ON g.ca_id=p.ca_id AND g.ca_version=p.ca_version JOIN nodes n ON n.id=p.node_id JOIN credentials c ON c.id=g.ca_id WHERE g.state='published' AND NOT c.archived AND n.state NOT IN ('deleting','delete_failed') AND NOT p.discarded AND p.next_action_at<=$1 AND NOT EXISTS(SELECT 1 FROM mtls_probe_versions newer WHERE newer.node_id=p.node_id AND newer.ca_id=p.ca_id AND newer.ca_version=p.ca_version AND newer.credential_version>p.credential_version AND NOT newer.discarded) AND EXISTS(SELECT 1 FROM mtls_node_trust t WHERE t.node_id=p.node_id AND t.ca_id=p.ca_id AND t.ca_version=p.ca_version AND (t.desired OR t.deployed)) AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id='mtls-probe:'||p.node_id||':'||p.ca_id||':'||p.ca_version::text AND w.state IN ('queued','running')) ORDER BY p.next_action_at,p.node_id,p.ca_id,p.ca_version LIMIT $2 FOR NO KEY UPDATE OF p SKIP LOCKED";

pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        if let Err(error) = pass(&state).await {
            tracing::warn!(%error,"probe renewal deferred");
        }
    }
}
pub async fn pass(state: &AppState) -> Result<()> {
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN_KIND, json!({})).await?;
    if let Some(work) = maintenance::claim(&state.pool, SCAN_KIND, &state.instance_id).await? {
        crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = admit_work(&state.pool, &work).await {
                tracing::warn!(%error,"probe admission deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .transpose()?;
    }
    if let Some(work) = maintenance::claim(&state.pool, KIND, &state.instance_id).await? {
        crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = renew_work(state, &work).await {
                tracing::warn!(%error,"probe renewal transaction deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .transpose()?;
    }
    Ok(())
}
async fn admit_work(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::write(pool,move|tx| {
        let work=work.clone();Box::pin(async move {
            work.guard_on(tx).await?;
            let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
            let rows=sqlx::query(DUE).bind(now).bind(PAGE).fetch_all(&mut **tx).await?;
            for row in &rows {
                let node:String=row.get("node_id");let ca:String=row.get("ca_id");let version:i64=row.get("ca_version");
                let id=format!("mtls-probe:{node}:{ca}:{version}");
                sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',owner=NULL,started_at=NULL,finished_at=NULL,error_code=NULL,available_at=clock_timestamp(),payload=excluded.payload WHERE background_work.kind=excluded.kind AND background_work.state IN ('complete','failed')")
                    .bind(id).bind(KIND).bind(json!({"node_id":node,"ca_id":ca,"ca_version":version,"credential_id":row.get::<String,_>("credential_id"),"credential_version":row.get::<i64,_>("credential_version"),"lease_resources":[format!("ca:{ca}"),format!("node:{node}")]})).execute(&mut **tx).await?;
            }
            work.finish_on(tx,false,if rows.len()==PAGE as usize {1}else{10},None).await
        })
    }).await
}

#[derive(Clone)]
struct Target {
    node: String,
    ca: String,
    version: i64,
    credential: String,
    credential_version: i64,
}
impl Target {
    fn from_work(work: &Work) -> Result<Self> {
        let target = Self {
            node: work.payload["node_id"]
                .as_str()
                .context("missing probe node")?
                .into(),
            ca: work.payload["ca_id"]
                .as_str()
                .context("missing probe authority")?
                .into(),
            version: work.payload["ca_version"]
                .as_i64()
                .context("missing probe authority version")?,
            credential: work.payload["credential_id"]
                .as_str()
                .context("missing probe credential")?
                .into(),
            credential_version: work.payload["credential_version"]
                .as_i64()
                .context("missing probe credential version")?,
        };
        ensure!(
            [format!("node:{}", target.node), format!("ca:{}", target.ca)]
                .iter()
                .all(|key| work.leases.iter().any(|lease| &lease.resource == key)),
            "probe resource scope is incomplete"
        );
        Ok(target)
    }
}
#[derive(sqlx::FromRow)]
struct Source {
    desired_revision: i64,
    due: bool,
}
const SOURCE: &str = "SELECT n.desired_revision,p.next_action_at<=clock_timestamp() due FROM nodes n JOIN mtls_probe_versions p ON p.node_id=n.id JOIN mtls_ca_generations g ON g.ca_id=p.ca_id AND g.ca_version=p.ca_version JOIN credentials c ON c.id=g.ca_id WHERE n.id=$1 AND p.ca_id=$2 AND p.ca_version=$3 AND p.credential_id=$4 AND p.credential_version=$5 AND n.state NOT IN ('deleting','delete_failed') AND g.state='published' AND NOT c.archived AND NOT p.discarded AND NOT EXISTS(SELECT 1 FROM mtls_probe_versions newer WHERE newer.node_id=p.node_id AND newer.ca_id=p.ca_id AND newer.ca_version=p.ca_version AND newer.credential_version>p.credential_version AND NOT newer.discarded) AND EXISTS(SELECT 1 FROM mtls_node_trust t WHERE t.node_id=p.node_id AND t.ca_id=p.ca_id AND t.ca_version=p.ca_version AND (t.desired OR t.deployed))";
async fn source_on(tx: &mut Transaction<'_, Postgres>, target: &Target) -> Result<Option<Source>> {
    Ok(sqlx::query_as(SOURCE)
        .bind(&target.node)
        .bind(&target.ca)
        .bind(target.version)
        .bind(&target.credential)
        .bind(target.credential_version)
        .fetch_optional(&mut **tx)
        .await?)
}
async fn lock_source(tx: &mut Transaction<'_, Postgres>, target: &Target) -> Result<()> {
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(&target.node)
        .fetch_optional(&mut **tx)
        .await?;
    let mut parents = vec![target.ca.clone(), target.credential.clone()];
    parents.sort();
    parents.dedup();
    sqlx::query(
        "SELECT id FROM credentials WHERE id=ANY($1::text[]) ORDER BY id FOR NO KEY UPDATE",
    )
    .bind(parents)
    .fetch_all(&mut **tx)
    .await?;
    sqlx::query("SELECT ca_id FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=$2 FOR SHARE")
        .bind(&target.ca)
        .bind(target.version)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(())
}
async fn renew_work(state: &AppState, work: &Work) -> Result<()> {
    let target = Target::from_work(work)?;
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    let source = source_on(&mut tx, &target).await?;
    tx.commit().await?;
    let Some(source) = source.filter(|s| s.due) else {
        return commit_work(&state.pool, work, None).await;
    };
    match probe::prepare(
        &state.pool,
        &state.secrets,
        &target.node,
        &target.ca,
        target.version,
    )
    .await
    {
        Ok(prepared) => commit_work(&state.pool, work, Some(prepared)).await,
        Err(error) => {
            record_error(
                &state.pool,
                work,
                source.desired_revision,
                if error.to_string().starts_with("CA update required:") {
                    "Probe certificate renewal failed: CA update required"
                } else {
                    "Probe certificate renewal failed: renewal deferred"
                },
            )
            .await
        }
    }
}
async fn commit_work(pool: &PgPool, work: &Work, prepared: Option<PreparedProbe>) -> Result<()> {
    let work = work.clone();
    db::serializable::<_, anyhow::Error, _>(pool, move |tx| {
        let (work, prepared) = (work.clone(), prepared.clone());
        Box::pin(async move {
            work.guard_on(tx).await?;
            let target = Target::from_work(&work)?;
            lock_source(tx, &target).await?;
            let Some(source) = source_on(tx, &target).await?.filter(|s| s.due) else {
                return work.finish_on(tx, false, 0, Some("probe_superseded")).await;
            };
            let _ = source;
            let Some(prepared) = prepared else {
                return work
                    .finish_on(tx, true, 1, Some("probe_preparation_required"))
                    .await;
            };
            ensure!(
                prepared.matches_scope(
                    &target.node,
                    &target.ca,
                    target.version,
                    &target.credential,
                    target.credential_version
                ),
                "prepared probe does not match its work scope"
            );
            probe::commit_on(tx, prepared).await?;
            work.finish_on(tx, false, 0, None).await
        })
    })
    .await
}
async fn record_error(pool: &PgPool, work: &Work, revision: i64, message: &str) -> Result<()> {
    let (work, message) = (work.clone(), message.to_owned());
    db::serializable::<_,anyhow::Error,_>(pool,move|tx| {
        let (work,message)=(work.clone(),message.clone());Box::pin(async move {
            work.guard_on(tx).await?;
            let target=Target::from_work(&work)?;
            lock_source(tx,&target).await?;
            let Some(source)=source_on(tx,&target).await?.filter(|s|s.due) else {return work.finish_on(tx,false,0,Some("probe_superseded")).await;};
            if source.desired_revision!=revision {return work.finish_on(tx,true,1,Some("probe_preparation_required")).await;}
            sqlx::query("UPDATE nodes SET mtls_error=$2 WHERE id=$1 AND mtls_state='ready'").bind(&target.node).bind(message).execute(&mut **tx).await?;
            sqlx::query("UPDATE mtls_probe_versions SET next_action_at=clock_timestamp()+interval '1 hour' WHERE node_id=$1 AND credential_id=$2 AND credential_version=$3")
                .bind(&target.node).bind(&target.credential).bind(target.credential_version).execute(&mut **tx).await?;
            work.finish_on(tx,false,0,None).await
        })
    }).await
}

#[cfg(test)]
mod tests;
