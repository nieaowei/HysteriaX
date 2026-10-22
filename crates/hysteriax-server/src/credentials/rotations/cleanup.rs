//! Bounded retired-anchor cleanup. Actual trust and retained leaves remain the
//! authority; member caches and queued jobs never prove remote cleanup.
use super::*;
use crate::background::maintenance::{self, Work};
const SCAN: &str = "mtls-trust-cleanup-admission";
const SCAN_KEY: &str = "mtls:trust-cleanup";
const KIND: &str = "mtls-trust-cleanup-page";
const MEMBERS: &str = "SELECT node_id FROM mtls_node_trust WHERE ca_id=$1 AND ca_version=$2 AND deployed UNION SELECT node_id FROM mtls_live_activation_trust WHERE ca_id=$1 AND ca_version=$2";
const ADMISSION: &str = "SELECT c.ca_id,c.ca_version FROM mtls_trust_cleanup c JOIN mtls_ca_generations g ON g.ca_id=c.ca_id AND g.ca_version=c.ca_version JOIN mtls_authorities a ON a.credential_id=c.ca_id WHERE c.completed_at IS NULL AND c.next_action_at<=$1 AND g.state='retired' AND a.published_version<>c.ca_version AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id='trust-cleanup:'||c.ca_id||':'||c.ca_version AND (w.state IN ('queued','running') OR w.available_at>$1)) ORDER BY c.next_action_at,c.ca_id,c.ca_version LIMIT $2";
pub(super) async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        if let Err(error) = scheduled_pass(&state).await {
            tracing::warn!(%error,"retired trust cleanup deferred");
        }
    }
}
pub(super) async fn scheduled_pass(state: &AppState) -> Result<()> {
    scan_pass(state).await?;
    if let Some(work) = maintenance::claim(&state.pool, KIND, &state.instance_id).await? {
        execute(&state.pool, &work, false).await?;
    }
    Ok(())
}
async fn scan_pass(state: &AppState) -> Result<()> {
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN, json!({})).await?;
    if let Some(work) = maintenance::claim(&state.pool, SCAN, &state.instance_id).await? {
        execute(&state.pool, &work, true).await?;
    }
    Ok(())
}
#[cfg(test)]
pub(super) async fn pass(state: &AppState) -> Result<()> {
    scan_pass(state).await?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await?;
    let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM background_work WHERE kind=$1 AND cycle_id IS NULL AND node_id IS NULL AND state IN ('queued','running') AND available_at<=$2 ORDER BY available_at,id LIMIT $3").bind(KIND).bind(now).bind(PAGE).fetch_all(&state.pool).await?;
    for id in ids {
        if let Some(work) =
            maintenance::claim_id(&state.pool, KIND, &state.instance_id, &id).await?
        {
            execute(&state.pool, &work, false).await?;
        }
    }
    Ok(())
}
async fn execute(pool: &PgPool, work: &Work, admission: bool) -> Result<()> {
    match crate::execution::supervise(pool.clone(), work.leases.clone(), async {
        if admission {
            scan(pool, work).await
        } else {
            page(pool, work).await
        }
    })
    .await
    {
        Some(Err(error)) => {
            work.retry(pool).await?;
            Err(error)
        }
        _ => Ok(()),
    }
}
async fn scan(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::write(pool,move|tx|{let work=work.clone();Box::pin(async move {
        work.guard_on(tx).await?;
        let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
        let rows=sqlx::query(ADMISSION).bind(now).bind(PAGE).fetch_all(&mut **tx).await?;
        for row in rows {
            let ca:String=row.get("ca_id");let version:i64=row.get("ca_version");
            sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',finished_at=NULL,error_code=NULL WHERE background_work.kind=excluded.kind AND background_work.state IN ('complete','failed') AND background_work.available_at<=clock_timestamp()")
                .bind(format!("trust-cleanup:{ca}:{version}")).bind(KIND).bind(json!({"ca_id":ca,"ca_version":version,"cursor":"","lease_resources":[format!("ca:{ca}")]})).execute(&mut **tx).await?;
        }
        work.finish_on(tx,false,1,None).await
    })}).await
}
fn target(work: &Work) -> Result<(String, i64, String)> {
    let ca = work.payload["ca_id"]
        .as_str()
        .context("missing cleanup CA")?
        .to_owned();
    let v = work.payload["ca_version"]
        .as_i64()
        .context("missing cleanup generation")?;
    let cursor = work.payload["cursor"]
        .as_str()
        .context("missing cleanup cursor")?
        .to_owned();
    ensure!(
        work.id == format!("trust-cleanup:{ca}:{v}")
            && work.leases.iter().any(|l| l.resource == format!("ca:{ca}")),
        "wrong trust cleanup work scope"
    );
    Ok((ca, v, cursor))
}
pub(super) async fn member_on(
    connection: &mut sqlx::PgConnection,
    node: &str,
    ca: &str,
    v: i64,
) -> Result<bool> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM ({MEMBERS}) m WHERE node_id=$3)"
    ))
    .bind(ca)
    .bind(v)
    .bind(node)
    .fetch_one(connection)
    .await?)
}
pub(super) async fn retention_due_on(
    connection: &mut sqlx::PgConnection,
    ca: &str,
    v: i64,
) -> Result<(bool, DateTime<Utc>)> {
    let deadline = retention_deadline(connection, ca, v).await?;
    // The no-leaf deadline itself uses clock_timestamp(). Compare against a
    // later database clock reading, never an earlier process or transaction time.
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(connection)
        .await?;
    Ok((deadline <= now, deadline))
}
async fn page(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::serializable(pool,move|tx|{let work=work.clone();Box::pin(async move {
        work.guard_on(tx).await?;
        let (ca,v,cursor)=target(&work)?;
        // Node parent order precedes CA/authority and the cleanup ledger. Busy
        // nodes remain authoritative members and are revisited after wrapping.
        let locked:Vec<String>=sqlx::query_scalar(&format!("WITH live AS (SELECT node_id FROM ({MEMBERS}) m WHERE node_id>$3 ORDER BY node_id LIMIT $4), stale AS (SELECT node_id FROM mtls_cleanup_nodes WHERE ca_id=$1 AND ca_version=$2 AND NOT completed AND node_id NOT IN ({MEMBERS}) ORDER BY node_id LIMIT $4) SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM live UNION SELECT node_id FROM stale) ORDER BY n.id FOR NO KEY UPDATE OF n SKIP LOCKED"))
            .bind(&ca).bind(v).bind(&cursor).bind(PAGE).fetch_all(&mut **tx).await?;
        let nodes:Vec<String>=sqlx::query_scalar(&format!("SELECT node_id FROM ({MEMBERS}) m WHERE node_id=ANY($3::text[]) AND node_id>$4 ORDER BY node_id LIMIT $5"))
            .bind(&ca).bind(v).bind(&locked).bind(&cursor).bind(PAGE).fetch_all(&mut **tx).await?;
        sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR KEY SHARE").bind(&ca).fetch_optional(&mut **tx).await?;
        sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE").bind(&ca).fetch_optional(&mut **tx).await?;
        sqlx::query("SELECT ca_version FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=$2 FOR UPDATE").bind(&ca).bind(v).fetch_optional(&mut **tx).await?;
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_trust_cleanup c JOIN mtls_ca_generations g ON g.ca_id=c.ca_id AND g.ca_version=c.ca_version JOIN mtls_authorities a ON a.credential_id=c.ca_id WHERE c.ca_id=$1 AND c.ca_version=$2 AND c.completed_at IS NULL AND c.next_action_at<=clock_timestamp() AND g.state='retired' AND a.published_version<>c.ca_version)").bind(&ca).bind(v).fetch_one(&mut **tx).await?;
        if !valid {return work.finish_on(tx,false,0,Some("trust_cleanup_superseded_or_not_due")).await;}
        sqlx::query("SELECT ca_id FROM mtls_trust_cleanup WHERE ca_id=$1 AND ca_version=$2 FOR UPDATE").bind(&ca).bind(v).fetch_one(&mut **tx).await?;
        let (due,deadline)=retention_due_on(tx,&ca,v).await?;
        if !due {
            sqlx::query("UPDATE mtls_trust_cleanup SET next_action_at=$3 WHERE ca_id=$1 AND ca_version=$2").bind(&ca).bind(v).bind(deadline).execute(&mut **tx).await?;
            return work.finish_on(tx,false,0,Some("trust_cleanup_waiting_retention")).await;
        }
        for node in &nodes {
            sqlx::query("INSERT INTO mtls_cleanup_nodes(ca_id,ca_version,node_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(&ca).bind(v).bind(node).execute(&mut **tx).await?;
            let previous:Option<(String,String)>=sqlx::query_as("SELECT j.id,j.status FROM mtls_cleanup_nodes c JOIN jobs j ON j.id=c.job_id WHERE c.ca_id=$1 AND c.ca_version=$2 AND c.node_id=$3").bind(&ca).bind(v).bind(node).fetch_optional(&mut **tx).await?;
            if let Some((job,status))=previous && (matches!(status.as_str(),"queued"|"running"|"waiting_confirmation") || !operation_stale_on(tx,&job).await?) {continue;}
            let ready:bool=sqlx::query_scalar("SELECT mtls_state='ready' AND state NOT IN ('deleting','delete_failed') FROM nodes WHERE id=$1").bind(node).fetch_one(&mut **tx).await?;
            if !ready {continue;}
            let job=enqueue_node(tx,node,&ca,v,None,"cleanup").await?;
            sqlx::query("UPDATE mtls_cleanup_nodes SET job_id=$4,completed=false,error_message=NULL WHERE ca_id=$1 AND ca_version=$2 AND node_id=$3").bind(&ca).bind(v).bind(node).bind(job).execute(&mut **tx).await?;
        }
        // A later ordinary deployment or a node removal can also prove that an
        // anchor is absent. Materialize only that fact, at most one page at a time.
        sqlx::query(&format!("UPDATE mtls_cleanup_nodes SET completed=true,error_message=NULL WHERE (ca_id,ca_version,node_id) IN (SELECT ca_id,ca_version,node_id FROM mtls_cleanup_nodes WHERE ca_id=$1 AND ca_version=$2 AND NOT completed AND node_id=ANY($3::text[]) AND node_id NOT IN ({MEMBERS}) ORDER BY node_id LIMIT $4 FOR UPDATE SKIP LOCKED)"))
            .bind(&ca).bind(v).bind(&locked).bind(PAGE).execute(&mut **tx).await?;
        let remains:bool=sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM ({MEMBERS}) m)")).bind(&ca).bind(v).fetch_one(&mut **tx).await?;
        let cached:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_cleanup_nodes WHERE ca_id=$1 AND ca_version=$2 AND NOT completed)").bind(&ca).bind(v).fetch_one(&mut **tx).await?;
        let done=!remains && !cached;
        let last=nodes.last().map(String::as_str).unwrap_or(&cursor);
        let more:bool=sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM ({MEMBERS}) m WHERE node_id>$3)")).bind(&ca).bind(v).bind(last).fetch_one(&mut **tx).await?;
        let next=if more && !nodes.is_empty() {last}else{""};
        sqlx::query("UPDATE background_work SET payload=jsonb_set(payload,'{cursor}',to_jsonb($2::text)) WHERE id=$1").bind(&work.id).bind(next).execute(&mut **tx).await?;
        let delay=if more {1}else{60};
        sqlx::query("UPDATE mtls_trust_cleanup SET completed_at=CASE WHEN $3 THEN clock_timestamp() ELSE NULL END,next_action_at=clock_timestamp()+make_interval(secs=>$4::double precision) WHERE ca_id=$1 AND ca_version=$2").bind(&ca).bind(v).bind(done).bind(delay as f64).execute(&mut **tx).await?;
        work.finish_on(tx,!done,delay,None).await
    })}).await
}

#[cfg(test)]
mod tests;
