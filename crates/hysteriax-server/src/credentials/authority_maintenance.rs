//! Generated CA publication under a CA/work lease. Rotation/trust work remains
//! a separate workflow; ordinary publication does not fan out to its members.
use crate::{
    api::{
        AdminActor,
        managed_mtls::{self, PreparedAuthorityPublication, PublishAuthority},
    },
    background::maintenance::{self, Work},
    db,
    error::ApiError,
    state::AppState,
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgPool, Postgres, Row, Transaction};
const PAGE: i64 = 100;
const KIND: &str = "mtls-ca-publication";
const SCAN_KIND: &str = "mtls-ca-admission";
const SCAN_KEY: &str = "mtls:authorities";
const DUE: &str = "SELECT a.credential_id,a.published_version,c.revision FROM mtls_authorities a JOIN credentials c ON c.id=a.credential_id WHERE a.source='generated' AND NOT c.archived AND a.next_action_at<=$1 AND NOT EXISTS(SELECT 1 FROM mtls_rotations r WHERE r.ca_id=a.credential_id AND r.phase<>'complete') AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id='mtls-ca-publication:'||a.credential_id||':'||a.published_version::text AND w.state IN ('queued','running')) ORDER BY a.next_action_at,a.credential_id LIMIT $2 FOR NO KEY UPDATE OF a SKIP LOCKED";
pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        if let Err(error) = pass(&state).await {
            tracing::warn!(%error,"CA maintenance deferred");
        }
    }
}
pub async fn pass(state: &AppState) -> Result<()> {
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN_KIND, json!({})).await?;
    if let Some(work) = maintenance::claim(&state.pool, SCAN_KIND, &state.instance_id).await? {
        crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = admit_work(&state.pool, &work).await {
                tracing::warn!(%error,"CA admission deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .transpose()?;
    }
    if let Some(work) = maintenance::claim(&state.pool, KIND, &state.instance_id).await? {
        crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = publish_work(state, &work).await {
                tracing::warn!(%error,"CA publication deferred");
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
                let ca:String=row.get("credential_id");let version:i64=row.get("published_version");
                sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',owner=NULL,started_at=NULL,finished_at=NULL,error_code=NULL,available_at=clock_timestamp(),payload=excluded.payload WHERE background_work.kind=excluded.kind AND background_work.state IN ('complete','failed')")
                    .bind(format!("mtls-ca-publication:{ca}:{version}")).bind(KIND).bind(json!({"ca_id":ca,"published_version":version,"expected_revision":row.get::<i64,_>("revision"),"lease_resources":[format!("ca:{ca}")]})).execute(&mut **tx).await?;
            }
            work.finish_on(tx,false,if rows.len()==PAGE as usize {1}else{10},None).await
        })
    }).await
}
#[derive(Clone)]
struct Target {
    ca: String,
    version: i64,
    revision: i64,
}
impl Target {
    fn from_work(work: &Work) -> Result<Self> {
        let target = Self {
            ca: work.payload["ca_id"].as_str().context("missing CA")?.into(),
            version: work.payload["published_version"]
                .as_i64()
                .context("missing publication version")?,
            revision: work.payload["expected_revision"]
                .as_i64()
                .context("missing CA revision")?,
        };
        ensure!(
            work.leases
                .iter()
                .any(|l| l.resource == format!("ca:{}", target.ca)),
            "CA resource scope is incomplete"
        );
        Ok(target)
    }
    fn key(&self) -> String {
        format!("auto-ca:{}:{}", self.ca, self.version)
    }
    fn matches(&self, source: &Source) -> bool {
        source.eligible
            && source.revision == self.revision
            && source.published_version == self.version
    }
}
#[derive(sqlx::FromRow)]
struct Source {
    revision: i64,
    published_version: i64,
    eligible: bool,
}
const SOURCE: &str = "SELECT c.revision,a.published_version,coalesce(a.source='generated' AND NOT c.archived AND a.next_action_at<=clock_timestamp() AND NOT EXISTS(SELECT 1 FROM mtls_rotations r WHERE r.ca_id=a.credential_id AND r.phase<>'complete'),false) eligible FROM mtls_authorities a JOIN credentials c ON c.id=a.credential_id WHERE a.credential_id=$1";
async fn source_on(tx: &mut Transaction<'_, Postgres>, target: &Target) -> Result<Option<Source>> {
    Ok(sqlx::query_as(SOURCE)
        .bind(&target.ca)
        .fetch_optional(&mut **tx)
        .await?)
}
async fn lock_source(tx: &mut Transaction<'_, Postgres>, target: &Target) -> Result<()> {
    sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR UPDATE")
        .bind(&target.ca)
        .fetch_optional(&mut **tx)
        .await?;
    sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE")
        .bind(&target.ca)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(())
}

async fn publish_work(state: &AppState, work: &Work) -> Result<()> {
    let target = Target::from_work(work)?;
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    let eligible = source_on(&mut tx, &target)
        .await?
        .is_some_and(|s| target.matches(&s));
    tx.commit().await?;
    if !eligible {
        return commit_work(&state.pool, work, None).await;
    }
    let input = PublishAuthority {
        expected_revision: target.revision,
        certificate: None,
        private_key: None,
        replace_rotation_id: None,
        idempotency_key: target.key(),
    };
    match managed_mtls::prepare_authority_publication(
        state,
        AdminActor {
            id: "system:mtls-maintenance".into(),
        },
        target.ca,
        input,
    )
    .await
    {
        Ok(p) => commit_work(&state.pool, work, Some(p)).await,
        Err(_) => record_error(&state.pool, work).await,
    }
}
async fn commit_work(
    pool: &PgPool,
    work: &Work,
    prepared: Option<PreparedAuthorityPublication>,
) -> Result<()> {
    let work = work.clone();
    db::serializable::<_, ApiError, _>(pool, move |tx| {
        let (work, prepared) = (work.clone(), prepared.clone());
        Box::pin(async move {
            work.guard_on(tx).await?;
            let target = Target::from_work(&work)?;
            lock_source(tx, &target).await?;
            if !source_on(tx, &target)
                .await?
                .is_some_and(|s| target.matches(&s))
            {
                work.finish_on(tx, false, 0, Some("ca_publication_superseded"))
                    .await?;
                return Ok(());
            }
            let Some(prepared) = prepared else {
                work.finish_on(tx, true, 1, Some("ca_preparation_required"))
                    .await?;
                return Ok(());
            };
            if !prepared.matches_automatic_scope(&target.ca, target.revision, &target.key()) {
                return Err(ApiError::conflict(
                    "prepared CA does not match its work scope",
                ));
            }
            managed_mtls::publish_authority_on(tx, prepared).await?;
            work.finish_on(tx, false, 0, None).await?;
            Ok(())
        })
    })
    .await
    .map_err(|e| anyhow::anyhow!(e.message))
}
async fn record_error(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::write(pool,move|tx| {
        let work=work.clone();Box::pin(async move {
            work.guard_on(tx).await?;
            let target=Target::from_work(&work)?;lock_source(tx,&target).await?;
            if !source_on(tx,&target).await?.is_some_and(|s|target.matches(&s)) {return work.finish_on(tx,false,0,Some("ca_publication_superseded")).await;}
            sqlx::query("UPDATE mtls_authorities SET maintenance_error='CA publication failed; retry scheduled',next_action_at=clock_timestamp()+interval '1 hour' WHERE credential_id=$1")
                .bind(&target.ca).execute(&mut **tx).await?;
            sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES(gen_random_uuid()::text,'system:mtls-maintenance','ca.publish_request_failed','credential',$1,$2,clock_timestamp())")
                .bind(&target.ca).bind(json!({"reason":"CA publication failed; retry scheduled","expected_revision":target.revision,"request_key_hash":crate::security::token_digest(&target.key())})).execute(&mut **tx).await?;
            work.finish_on(tx,false,0,None).await
        })
    }).await
}

#[cfg(test)]
mod tests;
