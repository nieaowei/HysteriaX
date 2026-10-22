//! Atomic CA publication: authoritative barriers and a fixed number of writes.
//! Node/identity fanout is admitted by outboxes committed with the publication.
use super::*;
use crate::background::maintenance::{self, Work};
const KIND: &str = "mtls-rotation-publication";
const SCAN: &str = "mtls-rotation-publication-admission";
const SCAN_KEY: &str = "mtls:rotation-publication-admission";
const ADMISSION: &str = "SELECT r.id,r.ca_id,r.ca_version FROM mtls_rotations r JOIN credentials c ON c.id=r.ca_id JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.phase IN ('identities','publish') AND r.outcome IS NULL AND NOT c.archived AND g.state='preparing' AND g.expires_at>$1 AND r.next_action_at<=$1 AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id='maintenance:rotation-publication:'||r.id AND (w.state IN ('queued','running') OR w.available_at>$1)) ORDER BY r.next_action_at,r.id LIMIT $2";
fn payload(rotation: &str, ca: &str, version: i64) -> Value {
    json!({"rotation_id":rotation,"ca_id":ca,"ca_version":version,"lease_resources":[format!("ca:{ca}")]})
}
pub(super) async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        if let Err(error) = scheduled_pass(&state).await {
            tracing::warn!(%error,"rotation publication deferred");
        }
    }
}
pub(super) async fn scheduled_pass(state: &AppState) -> Result<()> {
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN, json!({})).await?;
    if let Some(work) = maintenance::claim(&state.pool, SCAN, &state.instance_id).await? {
        crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = admit(&state.pool, &work).await {
                tracing::warn!(%error,"publication admission deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .transpose()?;
    }
    if let Some(work) = maintenance::claim(&state.pool, KIND, &state.instance_id).await? {
        execute(&state.pool, &work).await?;
    }
    Ok(())
}
async fn admit(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::write(pool,move|tx| {let work=work.clone();Box::pin(async move {
        work.guard_on(tx).await?;
        let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
        let rows=sqlx::query(ADMISSION).bind(now).bind(PAGE).fetch_all(&mut **tx).await?;
        for row in rows {
            let rotation:String=row.get("id");let ca:String=row.get("ca_id");let version:i64=row.get("ca_version");
            // A restored source may resume terminal work, but a healthy owner
            // or an unexpired retry deadline is never reset.
            sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',finished_at=NULL,error_code=NULL WHERE background_work.kind=excluded.kind AND background_work.state IN ('complete','failed') AND background_work.available_at<=clock_timestamp()")
                .bind(format!("maintenance:rotation-publication:{rotation}")).bind(KIND).bind(payload(&rotation,&ca,version)).execute(&mut **tx).await?;
        }
        work.finish_on(tx,false,1,None).await
    })}).await
}
#[cfg(test)]
pub(super) async fn pass(state: &AppState, rotation: &str, ca: &str, version: i64) -> Result<bool> {
    let key = format!("rotation-publication:{rotation}");
    maintenance::ensure(&state.pool, &key, KIND, payload(rotation, ca, version)).await?;
    let key = format!("maintenance:{key}");
    let Some(work) = maintenance::claim_id(&state.pool, KIND, &state.instance_id, &key).await?
    else {
        return Ok(false);
    };
    execute(&state.pool, &work).await
}
async fn execute(pool: &PgPool, work: &Work) -> Result<bool> {
    // Fixed before entering the retry boundary, including native SSI retries.
    let audit_id = Uuid::new_v4().to_string();
    match crate::execution::supervise(
        pool.clone(),
        work.leases.clone(),
        commit(pool, work, &audit_id),
    )
    .await
    {
        Some(Ok(published)) => Ok(published),
        Some(Err(error)) => {
            work.retry(pool).await?;
            Err(error)
        }
        None => Ok(false),
    }
}
fn target(work: &Work) -> Result<(String, String, i64)> {
    let rotation = work.payload["rotation_id"]
        .as_str()
        .context("missing publication rotation")?
        .to_owned();
    let ca = work.payload["ca_id"]
        .as_str()
        .context("missing publication CA")?
        .to_owned();
    let version = work.payload["ca_version"]
        .as_i64()
        .context("missing publication version")?;
    ensure!(
        work.id == format!("maintenance:rotation-publication:{rotation}")
            && work.leases.iter().any(|l| l.resource == format!("ca:{ca}")),
        "wrong publication work scope"
    );
    Ok((rotation, ca, version))
}
async fn commit(pool: &PgPool, work: &Work, audit_id: &str) -> Result<bool> {
    let (work, audit_id) = (work.clone(), audit_id.to_owned());
    db::serializable(pool, move |tx| {
        let (work, audit_id) = (work.clone(), audit_id.clone());
        Box::pin(async move { commit_on(tx, &work, &audit_id).await })
    })
    .await
}
async fn commit_on(
    tx: &mut Transaction<'static, Postgres>,
    work: &Work,
    audit_id: &str,
) -> Result<bool> {
    work.guard_on(tx).await?;
    let (rotation, ca, version) = target(work)?;
    sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR UPDATE")
        .bind(&ca)
        .fetch_optional(&mut **tx)
        .await?;
    sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE")
        .bind(&ca)
        .fetch_optional(&mut **tx)
        .await?;
    let source:Option<i64>=sqlx::query_scalar("SELECT a.published_version FROM mtls_rotations r JOIN credentials c ON c.id=r.ca_id JOIN mtls_authorities a ON a.credential_id=r.ca_id JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.id=$1 AND r.ca_id=$2 AND r.ca_version=$3 AND r.phase IN ('trust','identities','publish') AND r.outcome IS NULL AND NOT c.archived AND g.state='preparing' AND g.expires_at>clock_timestamp() AND a.published_version<>$3 FOR UPDATE OF r")
            .bind(&rotation).bind(&ca).bind(version).fetch_optional(&mut **tx).await?;
    let Some(old) = source else {
        work.finish_on(tx, false, 0, Some("rotation_publication_superseded"))
            .await?;
        return Ok(false);
    };
    // Lock exactly the two generations. This also serializes FK insertions
    // of old leaves and new membership against publication/retention proof.
    sqlx::query("SELECT ca_version FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=ANY($2::bigint[]) ORDER BY ca_version FOR UPDATE").bind(&ca).bind([old,version]).fetch_all(&mut **tx).await?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    if trust::missing_proof_on(tx, &ca, version, now).await? {
        sqlx::query("UPDATE mtls_rotations SET phase='trust',next_action_at=clock_timestamp(),error_message='Waiting for trust preparation on live members',updated_at=clock_timestamp() WHERE id=$1").bind(&rotation).execute(&mut **tx).await?;
        work.finish_on(tx, true, 1, Some("rotation_publication_waiting_trust"))
            .await?;
        return Ok(false);
    }
    if users::missing_identity_on(tx, &ca).await?
        || users::missing_leaf_on(tx, &ca, version).await?
    {
        sqlx::query("UPDATE mtls_rotations SET phase='identities',next_action_at=clock_timestamp(),error_message='Waiting for eligible identity material',updated_at=clock_timestamp() WHERE id=$1").bind(&rotation).execute(&mut **tx).await?;
        work.finish_on(tx, true, 1, Some("rotation_publication_waiting_identities"))
            .await?;
        return Ok(false);
    }
    sqlx::query("UPDATE mtls_ca_generations SET state='retired' WHERE ca_id=$1 AND ca_version=$2 AND state='published'").bind(&ca).bind(old).execute(&mut **tx).await?;
    let changed=sqlx::query("UPDATE mtls_ca_generations SET state='published',published_at=$3 WHERE ca_id=$1 AND ca_version=$2 AND state='preparing' AND expires_at>$3").bind(&ca).bind(version).bind(now).execute(&mut **tx).await?;
    ensure!(
        changed.rows_affected() == 1,
        "target CA no longer preparing"
    );
    sqlx::query("UPDATE mtls_authorities SET published_version=$2,maintenance_error=NULL,next_action_at=CASE WHEN source='generated' THEN (SELECT expires_at-interval '365 days' FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=$2) ELSE NULL END WHERE credential_id=$1").bind(&ca).bind(version).execute(&mut **tx).await?;
    let deadline = retention_deadline(tx, &ca, old).await?;
    sqlx::query("INSERT INTO mtls_trust_cleanup(ca_id,ca_version,next_action_at) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(&ca).bind(old).bind(deadline).execute(&mut **tx).await?;
    publication::enqueue_on(tx, &rotation, &ca, version).await?;
    metadata::enqueue_on(tx, &rotation, &ca, version).await?;
    probes::enqueue_on(tx, &rotation, &ca, version).await?;
    sqlx::query("UPDATE mtls_rotations SET phase='probes',published_at=$2,error_message=NULL,next_action_at=$2,updated_at=$2 WHERE id=$1").bind(&rotation).bind(now).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) SELECT $1,actor_id,'ca.published','ca_rotation',id,jsonb_build_object('ca_credential_id',ca_id,'ca_version',ca_version,'version',ca_version,'retired_version',$2::bigint,'cleanup_after',$3::timestamptz),$4 FROM mtls_rotations WHERE id=$5")
            .bind(audit_id).bind(old).bind(deadline).bind(now).bind(&rotation).execute(&mut **tx).await?;
    work.finish_on(tx, false, 0, None).await?;
    Ok(true)
}

#[cfg(test)]
mod tests;
