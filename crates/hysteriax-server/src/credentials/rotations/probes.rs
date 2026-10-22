//! Bounded probe switching and an authoritative, fenced completion barrier.
use super::*;
use crate::background::maintenance::{self, Work};
const ADMIT: &str = "mtls-rotation-probe-admission";
const CHILD: &str = "mtls-rotation-probe-switch";
const SCAN: &str = "mtls-rotation-probe-scan";
const SCAN_KEY: &str = "mtls:rotation-probes";
const CHILD_DUE: &str = "SELECT id FROM background_work WHERE kind=$1 AND payload->>'rotation_id'=$2 AND cycle_id IS NULL AND node_id IS NULL AND state IN ('queued','running') AND available_at<=$3 ORDER BY available_at,id LIMIT $4";
const MEMBERS: &str = "SELECT node_id FROM mtls_node_trust WHERE ca_id=$1 AND (desired OR deployed) UNION SELECT node_id FROM mtls_live_activation_trust WHERE ca_id=$1";
const ACTIVE: &str = "EXISTS(SELECT 1 FROM mtls_active_probes ap JOIN mtls_probe_versions p ON p.node_id=ap.node_id AND p.credential_id=ap.credential_id AND p.credential_version=ap.credential_version WHERE ap.node_id=n.id AND ap.ca_id=$1 AND ap.ca_version=$2 AND p.ca_id=$1 AND p.ca_version=$2 AND NOT p.discarded AND p.expires_at>clock_timestamp())";
const ADMISSION: &str = "SELECT r.id,r.ca_id,r.ca_version FROM mtls_rotations r JOIN credentials c ON c.id=r.ca_id JOIN mtls_authorities a ON a.credential_id=r.ca_id JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.phase='probes' AND r.outcome IS NULL AND r.published_at IS NOT NULL AND a.published_version=r.ca_version AND g.state='published' AND NOT c.archived AND g.expires_at>$1 AND r.next_action_at<=$1 AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id='rotation-probes:'||r.id AND (w.state IN ('queued','running') OR w.available_at>$1)) ORDER BY r.next_action_at,r.id LIMIT $2";
fn payload(rotation: &str, ca: &str, version: i64) -> Value {
    json!({"rotation_id":rotation,"ca_id":ca,"ca_version":version,"cursor":"","lease_resources":[format!("ca:{ca}")]})
}
pub(super) async fn enqueue_on(
    tx: &mut Transaction<'_, Postgres>,
    rotation: &str,
    ca: &str,
    version: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING",
    )
    .bind(format!("rotation-probes:{rotation}"))
    .bind(ADMIT)
    .bind(payload(rotation, ca, version))
    .execute(&mut **tx)
    .await?;
    Ok(())
}
pub(super) async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        if let Err(error) = scheduled_pass(&state).await {
            tracing::warn!(%error,"rotation probe work deferred");
        }
    }
}
async fn scheduled_pass(state: &AppState) -> Result<()> {
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN, json!({})).await?;
    if let Some(w) = maintenance::claim(&state.pool, SCAN, &state.instance_id).await? {
        execute(&state.pool, &w, Action::Scan).await?;
    }
    if let Some(w) = maintenance::claim(&state.pool, ADMIT, &state.instance_id).await? {
        execute(&state.pool, &w, Action::Page).await?;
    }
    if let Some(w) = maintenance::claim(&state.pool, CHILD, &state.instance_id).await? {
        execute(&state.pool, &w, Action::Switch).await?;
    }
    Ok(())
}
enum Action {
    Scan,
    Page,
    Switch,
}
async fn execute(pool: &PgPool, work: &Work, operation: Action) -> Result<()> {
    let audit = Uuid::new_v4().to_string();
    let result = crate::execution::supervise(pool.clone(), work.leases.clone(), async {
        match operation {
            Action::Scan => scan(pool, work).await,
            Action::Page => page(pool, work, &audit).await,
            Action::Switch => switch(pool, work).await,
        }
    })
    .await;
    if let Some(Err(error)) = result {
        work.retry(pool).await?;
        return Err(error);
    }
    Ok(())
}
pub(super) async fn pass(state: &AppState, rotation: &str, ca: &str, version: i64) -> Result<()> {
    let (r, c) = (rotation.to_owned(), ca.to_owned());
    db::write::<_, anyhow::Error, _>(&state.pool, move |tx| {
        let (r, c) = (r.clone(), c.clone());
        Box::pin(async move { enqueue_on(tx, &r, &c, version).await })
    })
    .await?;
    let id = format!("rotation-probes:{rotation}");
    if let Some(w) = maintenance::claim_id(&state.pool, ADMIT, &state.instance_id, &id).await? {
        execute(&state.pool, &w, Action::Page).await?;
    }
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await?;
    let ids: Vec<String> = sqlx::query_scalar(CHILD_DUE)
        .bind(CHILD)
        .bind(rotation)
        .bind(now)
        .bind(PAGE)
        .fetch_all(&state.pool)
        .await?;
    for id in ids {
        if let Some(w) = maintenance::claim_id(&state.pool, CHILD, &state.instance_id, &id).await? {
            execute(&state.pool, &w, Action::Switch).await?;
        }
    }
    // A second bounded page can confirm switches just completed by this pass.
    if let Some(w) = maintenance::claim_id(&state.pool, ADMIT, &state.instance_id, &id).await? {
        execute(&state.pool, &w, Action::Page).await?;
    }
    Ok(())
}
async fn scan(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::write(pool,move|tx| {let work=work.clone();Box::pin(async move {
        work.guard_on(tx).await?;
        let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
        let rows=sqlx::query(ADMISSION).bind(now).bind(PAGE).fetch_all(&mut **tx).await?;
        for row in rows {
            let r:String=row.get("id");let ca:String=row.get("ca_id");let version:i64=row.get("ca_version");
            sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',finished_at=NULL,error_code=NULL WHERE background_work.kind=excluded.kind AND background_work.state IN ('complete','failed') AND background_work.available_at<=clock_timestamp()")
                .bind(format!("rotation-probes:{r}")).bind(ADMIT).bind(payload(&r,&ca,version)).execute(&mut **tx).await?;
        }
        work.finish_on(tx,false,1,None).await
    })}).await
}
fn target(work: &Work) -> Result<(String, String, i64)> {
    let r = work.payload["rotation_id"]
        .as_str()
        .context("missing probe rotation")?
        .to_owned();
    let ca = work.payload["ca_id"]
        .as_str()
        .context("missing probe CA")?
        .to_owned();
    let v = work.payload["ca_version"]
        .as_i64()
        .context("missing probe generation")?;
    ensure!(
        work.leases.iter().any(|l| l.resource == format!("ca:{ca}")),
        "missing probe CA resource"
    );
    Ok((r, ca, v))
}
async fn current(tx: &mut Transaction<'_, Postgres>, r: &str, ca: &str, v: i64) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_rotations r JOIN credentials c ON c.id=r.ca_id JOIN mtls_authorities a ON a.credential_id=r.ca_id JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.id=$1 AND r.ca_id=$2 AND r.ca_version=$3 AND r.phase='probes' AND r.outcome IS NULL AND r.published_at IS NOT NULL AND a.published_version=$3 AND g.state='published' AND g.expires_at>clock_timestamp() AND NOT c.archived)")
        .bind(r).bind(ca).bind(v).fetch_one(&mut **tx).await?)
}
async fn lock_ca(tx: &mut Transaction<'_, Postgres>, ca: &str, r: &str) -> Result<()> {
    sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR KEY SHARE")
        .bind(ca)
        .fetch_optional(&mut **tx)
        .await?;
    sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE")
        .bind(ca)
        .fetch_optional(&mut **tx)
        .await?;
    sqlx::query("SELECT id FROM mtls_rotations WHERE id=$1 FOR UPDATE")
        .bind(r)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(())
}
async fn pending_children(tx: &mut Transaction<'_, Postgres>, r: &str) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_work WHERE kind=$1 AND payload->>'rotation_id'=$2 AND cycle_id IS NULL AND node_id IS NULL AND state<>'complete')").bind(CHILD).bind(r).fetch_one(&mut **tx).await?)
}
async fn page(pool: &PgPool, work: &Work, audit: &str) -> Result<()> {
    let (work, audit) = (work.clone(), audit.to_owned());
    db::serializable(pool, move |tx| {
        let (work, audit) = (work.clone(), audit.clone());
        Box::pin(async move { page_on(tx, &work, &audit).await })
    })
    .await
}
async fn page_on(tx: &mut Transaction<'static, Postgres>, work: &Work, audit: &str) -> Result<()> {
    work.guard_on(tx).await?;
    let (r, ca, v) = target(&work)?;
    ensure!(
        work.id == format!("rotation-probes:{r}"),
        "wrong probe admission scope"
    );
    lock_ca(tx, &ca, &r).await?;
    if !current(tx, &r, &ca, v).await? {
        return work
            .finish_on(tx, false, 0, Some("rotation_probes_superseded"))
            .await;
    }
    if metadata::pending_on(&mut **tx, &r).await? {
        return work
            .finish_on(tx, true, 1, Some("rotation_probes_waiting_metadata"))
            .await;
    }
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    // Cached membership is presentation data. Retire it in bounded pages too,
    // revisiting locked rows before declaring the rotation fully complete.
    sqlx::query(&format!("DELETE FROM mtls_rotation_nodes WHERE (rotation_id,node_id) IN (SELECT rotation_id,node_id FROM mtls_rotation_nodes WHERE rotation_id=$2 AND node_id NOT IN ({MEMBERS}) ORDER BY node_id LIMIT $3 FOR UPDATE SKIP LOCKED)"))
        .bind(&ca).bind(&r).bind(PAGE).execute(&mut **tx).await?;
    let obsolete:bool=sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM mtls_rotation_nodes WHERE rotation_id=$2 AND node_id NOT IN ({MEMBERS}))"))
        .bind(&ca).bind(&r).fetch_one(&mut **tx).await?;
    let cursor = work.payload["cursor"]
        .as_str()
        .context("missing probe cursor")?;
    // Missing actual proof is also an obligation, even if the active pointer
    // or a cached member claims that switching already finished.
    let cache = "EXISTS(SELECT 1 FROM mtls_rotation_nodes rn WHERE rn.rotation_id=$6 AND rn.node_id=n.id AND rn.expected_revision=n.desired_revision AND rn.trust_prepared AND rn.probe_switched)";
    let missing = format!("(NOT {ACTIVE} OR NOT {} OR NOT {cache})", trust::PROOF);
    let nodes:Vec<String>=sqlx::query_scalar(&format!("SELECT n.id FROM nodes n WHERE n.id IN ({MEMBERS}) AND n.id>$4 AND {missing} ORDER BY n.id LIMIT $5"))
            .bind(&ca).bind(v).bind(now).bind(cursor).bind(PAGE).bind(&r).fetch_all(&mut **tx).await?;
    for node in &nodes {
        sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',finished_at=NULL,error_code=NULL WHERE background_work.kind=excluded.kind AND background_work.state IN ('complete','failed') AND background_work.available_at<=clock_timestamp()")
                .bind(format!("rotation-probe:{r}:{node}")).bind(CHILD).bind(json!({"rotation_id":r,"ca_id":ca,"ca_version":v,"node_id":node,"lease_resources":[format!("ca:{ca}"),format!("node:{node}")]})).execute(&mut **tx).await?;
    }
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    let missing_proof = trust::missing_proof_on(tx, &ca, v, now).await?;
    let missing_active: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM nodes n WHERE n.id IN ({MEMBERS}) AND NOT {ACTIVE})"
    ))
    .bind(&ca)
    .bind(v)
    .fetch_one(&mut **tx)
    .await?;
    let missing_cache:bool=sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM nodes n WHERE n.id IN ({MEMBERS}) AND NOT EXISTS(SELECT 1 FROM mtls_rotation_nodes rn WHERE rn.rotation_id=$2 AND rn.node_id=n.id AND rn.expected_revision=n.desired_revision AND rn.trust_prepared AND rn.probe_switched))"))
        .bind(&ca).bind(&r).fetch_one(&mut **tx).await?;
    let ready = !obsolete
        && !missing_proof
        && !missing_active
        && !missing_cache
        && !pending_children(tx, &r).await?;
    if ready {
        sqlx::query("UPDATE mtls_rotations SET phase='complete',error_message=NULL,updated_at=clock_timestamp() WHERE id=$1").bind(&r).execute(&mut **tx).await?;
        sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) SELECT $1,actor_id,'ca.rotation_completed','ca_rotation',id,jsonb_build_object('ca_credential_id',ca_id,'ca_version',ca_version,'version',ca_version),clock_timestamp() FROM mtls_rotations WHERE id=$2").bind(&audit).bind(&r).execute(&mut **tx).await?;
    } else {
        sqlx::query("UPDATE mtls_rotations SET error_message='Waiting for probe activation on live members',next_action_at=clock_timestamp()+interval '1 second',updated_at=clock_timestamp() WHERE id=$1").bind(&r).execute(&mut **tx).await?;
    }
    let cursor = nodes.last().map(String::as_str).unwrap_or("");
    sqlx::query("UPDATE background_work SET payload=jsonb_set(payload,'{cursor}',to_jsonb($2::text)) WHERE id=$1").bind(&work.id).bind(cursor).execute(&mut **tx).await?;
    work.finish_on(tx, !ready, 0, None).await
}

async fn switch(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::serializable(pool,move|tx| {let work=work.clone();Box::pin(async move {
        work.guard_on(tx).await?;let (r,ca,v)=target(&work)?;
        let node=work.payload["node_id"].as_str().context("missing probe node")?;
        ensure!(work.id==format!("rotation-probe:{r}:{node}") && work.leases.iter().any(|l|l.resource==format!("node:{node}")),"wrong probe node scope");
        let source:Option<(i64,String)>=sqlx::query_as("SELECT desired_revision,state FROM nodes WHERE id=$1 FOR NO KEY UPDATE").bind(node).fetch_optional(&mut **tx).await?;
        let Some((revision,state))=source else {return work.finish_on(tx,false,0,Some("rotation_probe_removed")).await;};
        lock_ca(tx,&ca,&r).await?;
        if !current(tx,&r,&ca,v).await? {return work.finish_on(tx,false,0,Some("rotation_probes_superseded")).await;}
        let member:bool=sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM ({MEMBERS}) m WHERE node_id=$2)")).bind(&ca).bind(node).fetch_one(&mut **tx).await?;
        if !member {return work.finish_on(tx,false,0,Some("rotation_probe_removed")).await;}
        if matches!(state.as_str(),"deleting"|"delete_failed") || !proof_on(tx,node,&ca,v).await? {
            return work.finish_on(tx,true,60,Some("rotation_probe_waiting_proof")).await;
        }
        let active:bool=sqlx::query_scalar(&format!("SELECT {ACTIVE} FROM nodes n WHERE n.id=$3")).bind(&ca).bind(v).bind(node).fetch_one(&mut **tx).await?;
        if !active && !switch_probe_on(tx,node,&ca,v).await? {return work.finish_on(tx,true,60,Some("rotation_probe_waiting_proof")).await;}
        sqlx::query("INSERT INTO mtls_rotation_nodes(rotation_id,node_id,expected_revision,trust_prepared,probe_switched) VALUES($1,$2,$3,true,true) ON CONFLICT(rotation_id,node_id) DO UPDATE SET expected_revision=$3,trust_prepared=true,probe_switched=true").bind(&r).bind(node).bind(revision).execute(&mut **tx).await?;
        work.finish_on(tx,false,0,None).await
    })}).await
}

#[cfg(test)]
mod tests;
