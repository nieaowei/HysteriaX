//! Bounded, fenced trust preparation. Only database jobs are produced here;
//! remote dispatch is performed by the ordinary node execution resources.
use super::*;
use crate::background::maintenance::{self, Work};
const KIND: &str = "mtls-rotation-trust";
const MEMBERS: &str = "SELECT node_id FROM mtls_node_trust WHERE ca_id=$1 AND (desired OR deployed) UNION SELECT node_id FROM mtls_live_activation_trust WHERE ca_id=$1";
// The publication barrier always uses authoritative membership and fresh proof,
// never the progress cursor or the cached rotation member flags.
const PROOF: &str = "EXISTS(SELECT 1 FROM mtls_activations a JOIN mtls_probe_versions p ON p.node_id=n.id AND p.credential_id=a.probe_credential_id AND p.credential_version=a.probe_credential_version JOIN mtls_node_trust t ON t.node_id=n.id AND t.ca_id=p.ca_id AND t.ca_version=p.ca_version WHERE a.id=n.mtls_activation_id AND n.mtls_state='ready' AND a.phase='committed' AND n.deployed_content_sha256=a.target_digest AND p.ca_id=$1 AND p.ca_version=$2 AND NOT p.discarded AND p.expires_at>$3 AND t.deployed)";

const ADMISSION: &str = "SELECT r.id,r.ca_id,r.ca_version FROM mtls_rotations r JOIN credentials c ON c.id=r.ca_id JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.phase='trust' AND NOT c.archived AND g.state='preparing' AND g.expires_at>$1 AND r.next_action_at<=$1 AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id='maintenance:rotation-trust:'||r.id AND w.state IN ('queued','running')) ORDER BY r.next_action_at,r.id LIMIT $2 FOR UPDATE OF r SKIP LOCKED";

const SCAN_KIND: &str = "mtls-rotation-trust-admission";
const SCAN_KEY: &str = "mtls:rotation-trust-admission";

pub(super) async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        if let Err(error) = scheduled_pass(&state).await {
            tracing::warn!(%error,"rotation trust scheduling deferred");
        }
    }
}
async fn scheduled_pass(state: &AppState) -> Result<()> {
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN_KIND, json!({})).await?;
    if let Some(work) = maintenance::claim(&state.pool, SCAN_KIND, &state.instance_id).await? {
        crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = admit(&state.pool, &work).await {
                tracing::warn!(%error,"rotation trust admission deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .transpose()?;
    }
    if let Some(work) = maintenance::claim(&state.pool, KIND, &state.instance_id).await? {
        execute(&state.pool, &work).await;
    }
    Ok(())
}
async fn admit(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::write(pool,move|tx| {
        let work=work.clone();Box::pin(async move {
            work.guard_on(tx).await?;
            let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
            let rows=sqlx::query(ADMISSION)
                .bind(now).bind(PAGE).fetch_all(&mut **tx).await?;
            for row in rows {
                let rotation:String=row.get("id");let ca:String=row.get("ca_id");let version:i64=row.get("ca_version");
                let id=format!("maintenance:rotation-trust:{rotation}");
                // A concurrent direct advance may have claimed an existing work
                // after candidate selection. Never wait for it while holding r.
                let locked:Option<String>=sqlx::query_scalar("SELECT id FROM background_work WHERE id=$1 FOR UPDATE SKIP LOCKED").bind(&id).fetch_optional(&mut **tx).await?;
                if locked.is_none() {
                    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_work WHERE id=$1)").bind(&id).fetch_one(&mut **tx).await?;
                    if exists {continue;}
                }
                sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',finished_at=NULL,error_code=NULL,available_at=clock_timestamp() WHERE background_work.kind=excluded.kind AND background_work.state IN ('complete','failed')")
                    .bind(id).bind(KIND).bind(json!({"rotation_id":rotation,"ca_id":ca,"ca_version":version,"cursor":"","lease_resources":[format!("ca:{ca}")]})).execute(&mut **tx).await?;
            }
            work.finish_on(tx,false,1,None).await
        })
    }).await
}
async fn execute(pool: &PgPool, work: &Work) -> Option<bool> {
    let result =
        crate::execution::supervise(pool.clone(), work.leases.clone(), page(pool, work)).await;
    match result {
        Some(Ok(ready)) => Some(ready),
        Some(Err(error)) => {
            tracing::warn!(%error,work_id=%work.id,"trust preparation deferred");
            let _ = work.retry(pool).await;
            None
        }
        None => None,
    }
}

pub(super) async fn pass(state: &AppState, rotation: &str, ca: &str, version: i64) -> Result<bool> {
    let key = format!("rotation-trust:{rotation}");
    maintenance::ensure(&state.pool, &key, KIND, json!({"rotation_id":rotation,"ca_id":ca,"ca_version":version,"cursor":"","lease_resources":[format!("ca:{ca}")]})).await?;
    let Some(work) = maintenance::claim_id(
        &state.pool,
        KIND,
        &state.instance_id,
        &format!("maintenance:{key}"),
    )
    .await?
    else {
        return Ok(false);
    };
    Ok(execute(&state.pool, &work).await.unwrap_or(false))
}

#[derive(Clone)]
struct Target {
    rotation: String,
    ca: String,
    version: i64,
    cursor: String,
}
impl Target {
    fn from_work(work: &Work) -> Result<Self> {
        let target = Self {
            rotation: work.payload["rotation_id"]
                .as_str()
                .context("missing rotation")?
                .into(),
            ca: work.payload["ca_id"].as_str().context("missing CA")?.into(),
            version: work.payload["ca_version"]
                .as_i64()
                .context("missing CA version")?,
            cursor: work.payload["cursor"]
                .as_str()
                .context("missing trust cursor")?
                .into(),
        };
        ensure!(
            work.leases
                .iter()
                .any(|l| l.resource == format!("ca:{}", target.ca)),
            "missing CA trust resource"
        );
        ensure!(
            work.id == format!("maintenance:rotation-trust:{}", target.rotation),
            "trust work scope changed"
        );
        Ok(target)
    }
}

async fn page(pool: &PgPool, work: &Work) -> Result<bool> {
    let work = work.clone();
    db::serializable(pool, move |tx| {
        let work = work.clone();
        Box::pin(async move {
            work.guard_on(tx).await?;
            let target = Target::from_work(&work)?;
            let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
            // Parent order matches ordinary node mutations. Skipped nodes are
            // revisited when the persistent cursor wraps; no proof is assumed.
            let members: Vec<String> = sqlx::query_scalar(&format!("SELECT n.id FROM nodes n WHERE n.id IN ({MEMBERS}) AND n.id>$2 ORDER BY n.id LIMIT $3 FOR NO KEY UPDATE OF n SKIP LOCKED"))
                .bind(&target.ca).bind(&target.cursor).bind(PAGE).fetch_all(&mut **tx).await?;
            sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR UPDATE").bind(&target.ca).fetch_optional(&mut **tx).await?;
            sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE").bind(&target.ca).fetch_optional(&mut **tx).await?;
            let source = sqlx::query("SELECT r.retry_requested FROM mtls_rotations r JOIN credentials c ON c.id=r.ca_id JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.id=$1 AND r.ca_id=$2 AND r.ca_version=$3 AND r.phase IN ('trust','identities','publish') AND NOT c.archived AND g.state='preparing' AND g.expires_at>clock_timestamp() FOR UPDATE OF r")
                .bind(&target.rotation).bind(&target.ca).bind(target.version).fetch_optional(&mut **tx).await?;
            let Some(source) = source else {
                work.finish_on(tx,false,0,Some("rotation_trust_superseded")).await?;
                return Ok(false);
            };
            let retry: bool = source.get("retry_requested");
            // Garbage membership is also deleted in bounded pages. Final
            // readiness does not depend on these cached membership rows.
            sqlx::query(&format!("DELETE FROM mtls_rotation_nodes WHERE (rotation_id,node_id) IN (SELECT rotation_id,node_id FROM mtls_rotation_nodes WHERE rotation_id=$2 AND node_id NOT IN ({MEMBERS}) ORDER BY node_id LIMIT $3 FOR UPDATE SKIP LOCKED)"))
                .bind(&target.ca).bind(&target.rotation).bind(PAGE).execute(&mut **tx).await?;
            for node in &members {
                let proof: bool = sqlx::query_scalar(&format!("SELECT {PROOF} FROM nodes n WHERE n.id=$4"))
                    .bind(&target.ca).bind(target.version).bind(now).bind(node).fetch_one(&mut **tx).await?;
                sqlx::query("INSERT INTO mtls_rotation_nodes(rotation_id,node_id,expected_revision,trust_prepared) SELECT $1,id,desired_revision,$3 FROM nodes WHERE id=$2 ON CONFLICT(rotation_id,node_id) DO UPDATE SET expected_revision=excluded.expected_revision,trust_prepared=excluded.trust_prepared")
                    .bind(&target.rotation).bind(node).bind(proof).execute(&mut **tx).await?;
                if proof { continue; }
                let item = sqlx::query("SELECT p.job_id,j.status FROM mtls_rotation_nodes p LEFT JOIN jobs j ON j.id=p.job_id WHERE p.rotation_id=$1 AND p.node_id=$2")
                    .bind(&target.rotation).bind(node).fetch_one(&mut **tx).await?;
                let status: Option<String> = item.get("status");
                if status.as_deref().is_some_and(|s|matches!(s,"queued"|"running"|"waiting_confirmation")) { continue; }
                if let Some(previous) = item.get::<Option<String>,_>("job_id")
                    && status.as_deref()!=Some("succeeded") && !retry && !operation_stale_on(tx,&previous).await? { continue; }
                let ready: bool = sqlx::query_scalar("SELECT mtls_state='ready' AND state NOT IN ('deleting','delete_failed') FROM nodes WHERE id=$1")
                    .bind(node).fetch_one(&mut **tx).await?;
                if !ready { continue; }
                let job = enqueue_node(tx,node,&target.ca,target.version,Some(&target.rotation),"trust").await?;
                sqlx::query("UPDATE mtls_rotation_nodes SET job_id=$3,error_message=NULL WHERE rotation_id=$1 AND node_id=$2")
                    .bind(&target.rotation).bind(node).bind(job).execute(&mut **tx).await?;
            }
            let last = members.last().map(String::as_str).unwrap_or(&target.cursor);
            let more: bool = sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM ({MEMBERS}) m WHERE node_id>$2)"))
                .bind(&target.ca).bind(last).fetch_one(&mut **tx).await?;
            let missing: bool = sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM nodes n WHERE n.id IN ({MEMBERS}) AND NOT {PROOF})"))
                .bind(&target.ca).bind(target.version).bind(now).fetch_one(&mut **tx).await?;
            let ready = !missing;
            let next = if ready {0} else if more {1} else {60};
            sqlx::query("UPDATE mtls_rotations SET phase=$2,error_message=$3,next_action_at=clock_timestamp()+make_interval(secs=>$4::double precision),retry_requested=CASE WHEN $5 THEN retry_requested ELSE false END,updated_at=clock_timestamp() WHERE id=$1")
                .bind(&target.rotation).bind(if ready {"identities"}else{"trust"}).bind((!ready).then_some("Waiting for trust preparation on live members")).bind(next as f64).bind(more).execute(&mut **tx).await?;
            // If every remaining member was locked, wrap rather than starving
            // earlier skipped nodes forever. The authoritative barrier stays shut.
            let cursor = if more && !members.is_empty() {last}else{""};
            sqlx::query("UPDATE background_work SET payload=jsonb_set(payload,'{cursor}',to_jsonb($2::text)) WHERE id=$1")
                .bind(&work.id).bind(cursor).execute(&mut **tx).await?;
            work.finish_on(tx,false,0,None).await?;
            Ok(ready)
        })
    }).await
}

#[cfg(test)]
mod tests;
