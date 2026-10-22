//! Bounded shared-identity admission and individually fenced staging signatures.
use super::*;
use crate::background::maintenance::{self, Work};
use identities::PreparedIdentity;
const ADMIT: &str = "mtls-rotation-user-admission";
const ISSUE: &str = "mtls-rotation-user-issuance";
const SCAN: &str = "mtls-rotation-user-scan";
const SCAN_KEY: &str = "mtls:rotation-users";
const ISSUE_DUE: &str = "SELECT id FROM background_work WHERE kind=$1 AND payload->>'rotation_id'=$2 AND cycle_id IS NULL AND node_id IS NULL AND state IN ('queued','running') AND available_at<=$4 ORDER BY available_at,id LIMIT $3";
const MEMBERS: &str = "SELECT node_id FROM mtls_node_trust WHERE ca_id=$1 AND (desired OR deployed) UNION SELECT node_id FROM mtls_live_activation_trust WHERE ca_id=$1";
const ELIGIBLE: &str = "u.enabled AND (u.expires_at IS NULL OR u.expires_at>$3) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes)";
const LEAF: &str = "EXISTS(SELECT 1 FROM mtls_user_versions v WHERE v.identity_id=i.id AND v.ca_id=$1 AND v.ca_version=$2 AND NOT v.discarded AND v.not_before<=$3 AND v.expires_at>$3)";

pub(super) async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        if let Err(error) = scheduled_pass(&state).await {
            tracing::warn!(%error,"rotation identity scheduling deferred");
        }
    }
}
async fn scheduled_pass(state: &AppState) -> Result<()> {
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN, json!({})).await?;
    if let Some(work) = maintenance::claim(&state.pool, SCAN, &state.instance_id).await? {
        crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = admit_rotations(&state.pool, &work).await {
                tracing::warn!(%error,"rotation identity admission deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .transpose()?;
    }
    if let Some(work) = maintenance::claim(&state.pool, ADMIT, &state.instance_id).await? {
        execute_admission(&state.pool, &work).await;
    }
    if let Some(work) = maintenance::claim(&state.pool, ISSUE, &state.instance_id).await? {
        execute_issue(state, &work).await;
    }
    Ok(())
}
fn admission_payload(rotation: &str, ca: &str, version: i64) -> Value {
    json!({"rotation_id":rotation,"ca_id":ca,"ca_version":version,"lease_resources":[format!("ca:{ca}")]})
}
async fn admit_rotations(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::write(pool,move|tx| {let work=work.clone();Box::pin(async move {
        work.guard_on(tx).await?;
        let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
        let rows=sqlx::query("SELECT r.id,r.ca_id,r.ca_version FROM mtls_rotations r JOIN credentials c ON c.id=r.ca_id JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.phase IN ('identities','publish') AND r.next_action_at<=$1 AND NOT c.archived AND g.state='preparing' AND g.expires_at>$1 AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id='maintenance:rotation-users:'||r.id AND w.state IN ('queued','running')) ORDER BY r.next_action_at,r.id LIMIT $2 FOR UPDATE OF r SKIP LOCKED")
            .bind(now).bind(PAGE).fetch_all(&mut **tx).await?;
        for row in rows {
            let rotation:String=row.get("id");let ca:String=row.get("ca_id");let version:i64=row.get("ca_version");let id=format!("maintenance:rotation-users:{rotation}");
            let locked:Option<String>=sqlx::query_scalar("SELECT id FROM background_work WHERE id=$1 FOR UPDATE SKIP LOCKED").bind(&id).fetch_optional(&mut **tx).await?;
            if locked.is_none() && sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM background_work WHERE id=$1)").bind(&id).fetch_one(&mut **tx).await? {continue;}
            sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',finished_at=NULL,error_code=NULL,available_at=clock_timestamp() WHERE background_work.kind=excluded.kind AND background_work.state IN ('complete','failed')")
                .bind(id).bind(ADMIT).bind(admission_payload(&rotation,&ca,version)).execute(&mut **tx).await?;
        }
        work.finish_on(tx,false,1,None).await
    })}).await
}

pub(super) async fn pass(state: &AppState, rotation: &str, ca: &str, version: i64) -> Result<bool> {
    let key = format!("rotation-users:{rotation}");
    maintenance::ensure(
        &state.pool,
        &key,
        ADMIT,
        admission_payload(rotation, ca, version),
    )
    .await?;
    let Some(work) = maintenance::claim_id(
        &state.pool,
        ADMIT,
        &state.instance_id,
        &format!("maintenance:{key}"),
    )
    .await?
    else {
        return Ok(false);
    };
    if !execute_admission(&state.pool, &work).await {
        return Ok(false);
    }
    // Direct advancement remains bounded; the asynchronous worker resumes any
    // queued remainder or abandoned owner using exactly these same work rows.
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await?;
    let ids: Vec<String> = sqlx::query_scalar(ISSUE_DUE)
        .bind(ISSUE)
        .bind(rotation)
        .bind(PAGE)
        .bind(now)
        .fetch_all(&state.pool)
        .await?;
    for id in ids {
        if let Some(work) =
            maintenance::claim_id(&state.pool, ISSUE, &state.instance_id, &id).await?
        {
            execute_issue(state, &work).await;
        }
    }
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    let ready = valid_rotation_on(&mut tx, rotation, ca, version).await?
        && !missing_identity_on(&mut tx, ca).await?
        && !missing_leaf_on(&mut tx, ca, version).await?;
    tx.commit().await?;
    Ok(ready)
}
async fn execute_admission(pool: &PgPool, work: &Work) -> bool {
    match crate::execution::supervise(pool.clone(), work.leases.clone(), admit_users(pool, work))
        .await
    {
        Some(Ok(())) => true,
        Some(Err(error)) => {
            tracing::warn!(%error,work_id=%work.id,"rotation users deferred");
            let _ = work.retry(pool).await;
            false
        }
        None => false,
    }
}
async fn execute_issue(state: &AppState, work: &Work) {
    let _ = crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
        if let Err(error) = issue_work(state, work).await {
            tracing::warn!(%error,work_id=%work.id,"rotation signature deferred");
            work.retry(&state.pool).await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
}
async fn valid_rotation_on(
    tx: &mut Transaction<'_, Postgres>,
    rotation: &str,
    ca: &str,
    version: i64,
) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_rotations r JOIN credentials c ON c.id=r.ca_id JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.id=$1 AND r.ca_id=$2 AND r.ca_version=$3 AND r.phase IN ('identities','publish') AND NOT c.archived AND g.state='preparing' AND g.expires_at>clock_timestamp())")
        .bind(rotation).bind(ca).bind(version).fetch_one(&mut **tx).await?)
}
pub(super) async fn missing_identity_on(
    tx: &mut Transaction<'_, Postgres>,
    ca: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM users u WHERE EXISTS(SELECT 1 FROM node_assignments a WHERE a.user_id=u.id AND a.node_id IN ({MEMBERS})) AND NOT EXISTS(SELECT 1 FROM mtls_user_identities i WHERE i.user_id=u.id AND i.ca_id=$1))"))
        .bind(ca).fetch_one(&mut **tx).await?)
}
async fn missing_leaf_on(
    tx: &mut Transaction<'_, Postgres>,
    ca: &str,
    version: i64,
) -> Result<bool> {
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    Ok(sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM mtls_user_identities i JOIN users u ON u.id=i.user_id WHERE i.ca_id=$1 AND NOT i.archived AND {ELIGIBLE} AND NOT {LEAF})"))
        .bind(ca).bind(version).bind(now).fetch_one(&mut **tx).await?)
}
async fn lock_rotation(tx: &mut Transaction<'_, Postgres>, rotation: &str, ca: &str) -> Result<()> {
    sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR UPDATE")
        .bind(ca)
        .fetch_optional(&mut **tx)
        .await?;
    sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE")
        .bind(ca)
        .fetch_optional(&mut **tx)
        .await?;
    sqlx::query("SELECT id FROM mtls_rotations WHERE id=$1 FOR UPDATE")
        .bind(rotation)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(())
}
fn rotation_target(work: &Work) -> Result<(String, String, i64)> {
    let rotation = work.payload["rotation_id"]
        .as_str()
        .context("missing rotation")?
        .to_owned();
    let ca = work.payload["ca_id"]
        .as_str()
        .context("missing CA")?
        .to_owned();
    let version = work.payload["ca_version"]
        .as_i64()
        .context("missing generation")?;
    ensure!(
        work.leases.iter().any(|l| l.resource == format!("ca:{ca}")),
        "missing rotation CA resource"
    );
    Ok((rotation, ca, version))
}
async fn admit_users(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::serializable(pool,move|tx| {let work=work.clone();Box::pin(async move {
        work.guard_on(tx).await?;
        let (rotation,ca,version)=rotation_target(&work)?;
        ensure!(work.id==format!("maintenance:rotation-users:{rotation}"),"wrong admission scope");
        let users:Vec<String>=sqlx::query_scalar(&format!("SELECT u.id FROM users u WHERE EXISTS(SELECT 1 FROM node_assignments a WHERE a.user_id=u.id AND a.node_id IN ({MEMBERS})) AND NOT EXISTS(SELECT 1 FROM mtls_user_identities i WHERE i.user_id=u.id AND i.ca_id=$1) ORDER BY u.id LIMIT $2 FOR NO KEY UPDATE OF u SKIP LOCKED"))
            .bind(&ca).bind(PAGE).fetch_all(&mut **tx).await?;
        lock_rotation(tx,&rotation,&ca).await?;
        if !valid_rotation_on(tx,&rotation,&ca,version).await? {return work.finish_on(tx,false,0,Some("rotation_users_superseded")).await;}
        for user in users {
            sqlx::query("INSERT INTO mtls_user_identities(id,user_id,ca_id,created_at,updated_at) VALUES(gen_random_uuid()::text,$1,$2,clock_timestamp(),clock_timestamp()) ON CONFLICT(user_id,ca_id) DO NOTHING")
                .bind(user).bind(&ca).execute(&mut **tx).await?;
        }
        let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
        let candidates=sqlx::query(&format!("SELECT i.id,i.user_id,i.revision,i.authorization_revision FROM mtls_user_identities i JOIN users u ON u.id=i.user_id WHERE i.ca_id=$1 AND NOT i.archived AND {ELIGIBLE} AND NOT {LEAF} AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id='rotation-user:'||$4||':'||i.id AND (w.state IN ('queued','running') OR w.available_at>$3)) ORDER BY i.user_id,i.id LIMIT $5 FOR UPDATE OF i SKIP LOCKED"))
            .bind(&ca).bind(version).bind(now).bind(&rotation).bind(PAGE).fetch_all(&mut **tx).await?;
        for row in candidates {
            let identity:String=row.get("id");
            sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',owner=NULL,started_at=NULL,finished_at=NULL,error_code=NULL,available_at=clock_timestamp(),payload=excluded.payload WHERE background_work.kind=excluded.kind AND background_work.state IN ('complete','failed') AND background_work.available_at<=clock_timestamp()")
                .bind(format!("rotation-user:{rotation}:{identity}")).bind(ISSUE).bind(json!({"rotation_id":rotation,"ca_id":ca,"ca_version":version,"identity_id":identity,"user_id":row.get::<String,_>("user_id"),"identity_revision":row.get::<i64,_>("revision"),"authorization_revision":row.get::<i64,_>("authorization_revision"),"lease_resources":[format!("ca:{ca}"),format!("identity:{identity}")]})).execute(&mut **tx).await?;
        }

        sqlx::query("UPDATE mtls_rotations SET next_action_at=clock_timestamp()+interval '1 second',updated_at=clock_timestamp() WHERE id=$1").bind(&rotation).execute(&mut **tx).await?;
        work.finish_on(tx,false,0,None).await
    })}).await
}

#[derive(Clone)]
struct Target {
    rotation: String,
    ca: String,
    version: i64,
    identity: String,
    user: String,
    revision: i64,
    authorization: i64,
}
impl Target {
    fn from_work(work: &Work) -> Result<Self> {
        let (rotation, ca, version) = rotation_target(work)?;
        let target = Self {
            rotation,
            ca,
            version,
            identity: work.payload["identity_id"]
                .as_str()
                .context("missing identity")?
                .into(),
            user: work.payload["user_id"]
                .as_str()
                .context("missing user")?
                .into(),
            revision: work.payload["identity_revision"]
                .as_i64()
                .context("missing identity revision")?,
            authorization: work.payload["authorization_revision"]
                .as_i64()
                .context("missing authorization revision")?,
        };
        ensure!(
            work.id == format!("rotation-user:{}:{}", target.rotation, target.identity)
                && work
                    .leases
                    .iter()
                    .any(|l| l.resource == format!("identity:{}", target.identity)),
            "wrong rotation identity scope"
        );
        Ok(target)
    }
    fn matches(&self, source: &Source) -> bool {
        source.eligible
            && source.revision == self.revision
            && source.authorization == self.authorization
    }
}
#[derive(sqlx::FromRow)]
struct Source {
    revision: i64,
    authorization: i64,
    user_revision: i64,
    policy_revision: i64,
    credential_id: Option<String>,
    eligible: bool,
    has_leaf: bool,
}
async fn source_on(tx: &mut Transaction<'_, Postgres>, t: &Target) -> Result<Option<Source>> {
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    Ok(sqlx::query_as(&format!("SELECT i.revision,i.authorization_revision authorization,u.revision user_revision,a.policy_revision,i.credential_id,({ELIGIBLE} AND NOT i.archived AND NOT c.archived AND g.state='preparing' AND g.expires_at>$3 AND r.phase IN ('identities','publish')) eligible,{LEAF} has_leaf FROM mtls_user_identities i JOIN users u ON u.id=i.user_id JOIN mtls_authorities a ON a.credential_id=i.ca_id JOIN credentials c ON c.id=i.ca_id JOIN mtls_ca_generations g ON g.ca_id=i.ca_id AND g.ca_version=$2 JOIN mtls_rotations r ON r.id=$4 AND r.ca_id=i.ca_id AND r.ca_version=$2 WHERE i.ca_id=$1 AND i.id=$5 AND i.user_id=$6"))
        .bind(&t.ca).bind(t.version).bind(now).bind(&t.rotation).bind(&t.identity).bind(&t.user).fetch_optional(&mut **tx).await?)
}
#[derive(Clone)]
enum Preparation {
    Skipped,
    Signed(PreparedIdentity),
    Failed {
        user_revision: i64,
        policy_revision: i64,
    },
}
async fn issue_work(state: &AppState, work: &Work) -> Result<()> {
    let t = Target::from_work(work)?;
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    let source = source_on(&mut tx, &t).await?;
    tx.commit().await?;
    let prepared = if let Some(source) = source.filter(|s| t.matches(s) && !s.has_leaf) {
        match identities::prepare_user(
            &state.pool,
            &state.secrets,
            &t.user,
            &t.ca,
            t.version,
            false,
        )
        .await
        {
            Ok(p) => Preparation::Signed(p),
            Err(_) => Preparation::Failed {
                user_revision: source.user_revision,
                policy_revision: source.policy_revision,
            },
        }
    } else {
        Preparation::Skipped
    };
    commit_work(state, work, prepared).await
}
async fn commit_work(state: &AppState, work: &Work, prepared: Preparation) -> Result<()> {
    let (work, secrets) = (work.clone(), state.secrets.clone());
    db::serializable(&state.pool,move|tx| {let (work,secrets,prepared)=(work.clone(),secrets.clone(),prepared.clone());Box::pin(async move {
        work.guard_on(tx).await?;let t=Target::from_work(&work)?;
        sqlx::query("SELECT id FROM users WHERE id=$1 FOR NO KEY UPDATE").bind(&t.user).fetch_optional(&mut **tx).await?;
        let before=source_on(tx,&t).await?;let mut parents=vec![t.ca.clone()];
        if let Some(credential)=before.as_ref().and_then(|s|s.credential_id.clone()) {parents.push(credential);}
        parents.sort();parents.dedup();
        sqlx::query("SELECT id FROM credentials WHERE id=ANY($1::text[]) ORDER BY id FOR UPDATE").bind(parents).fetch_all(&mut **tx).await?;
        sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE").bind(&t.ca).fetch_optional(&mut **tx).await?;
        sqlx::query("SELECT id FROM mtls_user_identities WHERE id=$1 FOR UPDATE").bind(&t.identity).fetch_optional(&mut **tx).await?;
        sqlx::query("SELECT id FROM mtls_rotations WHERE id=$1 FOR UPDATE").bind(&t.rotation).fetch_optional(&mut **tx).await?;
        let Some(source)=source_on(tx,&t).await?.filter(|s|t.matches(s)) else {return work.finish_on(tx,false,0,Some("rotation_identity_superseded")).await;};
        if source.has_leaf {return work.finish_on(tx,false,0,None).await;}
        match prepared {
            Preparation::Signed(candidate)=>{
                ensure!(candidate.matches_rotation(&t.identity,&t.user,&t.ca,t.version),"prepared identity does not match rotation work");
                let issued=candidate.will_issue();
                identities::commit_user(tx,&secrets,candidate).await?;
                if issued {sqlx::query("UPDATE mtls_rotations SET prepared_count=prepared_count+1,updated_at=clock_timestamp() WHERE id=$1").bind(&t.rotation).execute(&mut **tx).await?;}
                work.finish_on(tx,false,0,None).await
            }
            Preparation::Failed{user_revision,policy_revision}=>{
                if source.user_revision!=user_revision || source.policy_revision!=policy_revision {return work.finish_on(tx,true,1,Some("rotation_identity_preparation_required")).await;}
                sqlx::query("UPDATE mtls_rotations SET error_message='Shared identity preparation failed; retry scheduled',updated_at=clock_timestamp() WHERE id=$1").bind(&t.rotation).execute(&mut **tx).await?;
                audit(tx,&t.rotation,"ca.rotation_failed",json!({"reason":"Shared identity preparation failed; retry scheduled","identity_id":t.identity,"work_id":work.id})).await?;
                work.finish_on(tx,false,3600,Some("rotation_identity_preparation_failed")).await
            }
            Preparation::Skipped=>work.finish_on(tx,true,1,Some("rotation_identity_preparation_required")).await,
        }
    })}).await
}

#[cfg(test)]
mod tests;
