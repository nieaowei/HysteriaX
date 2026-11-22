//! One durable renewable work item per shared user/CA identity. Signing happens
//! outside transactions; publication and work acknowledgement commit together.
use super::identities::{self, PreparedIdentity};
use crate::{
    background::maintenance::{self, Work},
    db,
    state::AppState,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgPool, Row};

const PAGE: i64 = 100;
const KIND: &str = "mtls-identity-renewal";
const SCAN_KIND: &str = "mtls-identity-admission";
const SCAN_KEY: &str = "mtls:identities";

const DEMAND: maintenance::Demand = maintenance::Demand {
    kind: SCAN_KIND,
    key: SCAN_KEY,
    predicate: concat!(
        // Queued children already keep the worker busy. Let ordinary bounded
        // admission continue, and avoid rescanning an already admitted backlog
        // on every shared hint refresh. An empty queue enables urgent discovery.
        "CASE WHEN EXISTS(SELECT 1 FROM background_work WHERE kind='mtls-identity-renewal' AND state='queued') THEN false ELSE (",
        "EXISTS(WITH due AS MATERIALIZED (",
        include_str!("identity_due.sql"),
        " ORDER BY i.next_action_at,i.id LIMIT 1) SELECT 1 FROM due) OR EXISTS(WITH pending AS MATERIALIZED (",
        include_str!("identity_pending.sql"),
        " ORDER BY i.id LIMIT 1) SELECT 1 FROM pending)",
        ") END"
    ),
};

pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        if let Err(error) = state
            .maintenance
            .run_urgent(&[SCAN_KIND, KIND], &[SCAN_KEY], DEMAND, pass(&state))
            .await
        {
            tracing::warn!(%error,"identity maintenance deferred");
        }
    }
}
pub async fn pass(state: &AppState) -> Result<()> {
    maintenance::ensure_urgent(&state.pool, DEMAND, json!({})).await?;
    if let Some(work) = maintenance::claim(&state.pool, SCAN_KIND, &state.instance_id).await? {
        crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = admit_work(&state.pool, &work).await {
                tracing::warn!(%error,"identity admission deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .into_result()?
        .transpose()?;
    }
    state.maintenance.refresh_hint().await?;
    if let Some(work) = maintenance::claim(&state.pool, KIND, &state.instance_id).await? {
        crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
            if let Err(error) = renew_work(state, &work).await {
                tracing::warn!(%error,"identity renewal deferred");
                work.retry(&state.pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .into_result()?
        .transpose()?;
    }
    Ok(())
}

async fn admit_work(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::write(pool,move|tx| {
        let work=work.clone();
        Box::pin(async move {
            work.guard_on(tx).await?;
            let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
            // Excluding active work, rather than repeatedly reading the first
            // due page, lets admission progress while earlier signers are busy.
            let due=sqlx::query(sqlx::AssertSqlSafe(concat!(include_str!("identity_due.sql")," ORDER BY i.next_action_at,i.id LIMIT $2 FOR NO KEY UPDATE OF i SKIP LOCKED").replace("(SELECT now FROM cutoff)", "$1")))
                .bind(now).bind(PAGE).fetch_all(&mut **tx).await?;
            let pending=sqlx::query(sqlx::AssertSqlSafe(concat!(include_str!("identity_pending.sql")," ORDER BY i.id LIMIT $2 FOR NO KEY UPDATE OF i SKIP LOCKED").replace("(SELECT now FROM cutoff)", "$1")))
                .bind(now).bind(PAGE).fetch_all(&mut **tx).await?;
            for row in due.iter().chain(pending.iter()) {
                let id:String=row.get("id");
                sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES('mtls-identity:'||$1,$2,$3) ON CONFLICT(id) DO UPDATE SET state='queued',owner=NULL,started_at=NULL,finished_at=NULL,error_code=NULL,available_at=clock_timestamp() WHERE background_work.kind=excluded.kind AND background_work.state IN ('complete','failed')")
                    .bind(&id).bind(KIND).bind(json!({"identity_id":id,"user_id":row.get::<String,_>("user_id"),"ca_id":row.get::<String,_>("ca_id"),"lease_resources":[format!("identity:{id}")]})).execute(&mut **tx).await?;
            }
            work.finish_on(tx,false,if due.len()==PAGE as usize || pending.len()==PAGE as usize {1}else{30},None).await
        })
    }).await
}

#[derive(Clone, sqlx::FromRow)]
struct Source {
    revision: i64,
    published_version: i64,
    credential_id: Option<String>,
    archived: bool,
    due: bool,
    eligible: bool,
}
const SOURCE: &str = "SELECT i.revision,a.published_version,i.credential_id,i.archived,(i.next_action_at IS NULL OR i.next_action_at<=$4) due,coalesce(u.enabled AND (u.expires_at IS NULL OR u.expires_at>$4) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes),false) eligible FROM mtls_user_identities i JOIN mtls_authorities a ON a.credential_id=i.ca_id LEFT JOIN users u ON u.id=i.user_id WHERE i.id=$1 AND i.user_id=$2 AND i.ca_id=$3";
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
enum Preparation {
    Skipped,
    Signed(PreparedIdentity),
    Failed {
        revision: i64,
        version: i64,
        code: String,
    },
}

async fn renew_work(state: &AppState, work: &Work) -> Result<()> {
    let id = work.payload["identity_id"]
        .as_str()
        .context("missing identity")?;
    let user = work.payload["user_id"]
        .as_str()
        .context("missing identity owner")?;
    let ca = work.payload["ca_id"]
        .as_str()
        .context("missing identity authority")?;
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await?;
    let source: Option<Source> = sqlx::query_as(SOURCE)
        .bind(id)
        .bind(user)
        .bind(ca)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
    tx.commit().await?;
    let prepared = if let Some(source) = source.filter(|s| !s.archived && s.due && s.eligible) {
        match identities::prepare_user(
            &state.pool,
            &state.secrets,
            user,
            ca,
            source.published_version,
            false,
        )
        .await
        {
            Ok(p) => Preparation::Signed(p),
            Err(error) => Preparation::Failed {
                revision: source.revision,
                version: source.published_version,
                code: if error.to_string().starts_with("CA update required:") {
                    "CA update required: cannot extend the existing certificate".into()
                } else {
                    "Managed identity renewal failed; retry scheduled".into()
                },
            },
        }
    } else {
        Preparation::Skipped
    };
    commit_work(state, work, prepared).await
}

async fn commit_work(state: &AppState, work: &Work, prepared: Preparation) -> Result<()> {
    let (work, secrets) = (work.clone(), state.secrets.clone());
    db::serializable::<_,anyhow::Error,_>(&state.pool,move|tx| {
        let (work,secrets,prepared)=(work.clone(),secrets.clone(),prepared.clone());
        Box::pin(async move {
            work.guard_on(tx).await?;
            let id=work.payload["identity_id"].as_str().context("missing identity")?;
            let user=work.payload["user_id"].as_str().context("missing identity owner")?;
            let ca=work.payload["ca_id"].as_str().context("missing identity authority")?;
            sqlx::query("SELECT id FROM users WHERE id=$1 FOR NO KEY UPDATE").bind(user).fetch_optional(&mut **tx).await?;
            let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
            let before:Option<Source>=sqlx::query_as(SOURCE).bind(id).bind(user).bind(ca).bind(now).fetch_optional(&mut **tx).await?;
            let mut parents=vec![ca.to_owned()];
            if let Some(credential)=before.as_ref().and_then(|s|s.credential_id.clone()) {parents.push(credential);}
            parents.sort();parents.dedup();
            sqlx::query("SELECT id FROM credentials WHERE id=ANY($1::text[]) ORDER BY id FOR UPDATE").bind(parents).fetch_all(&mut **tx).await?;
            sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE").bind(ca).fetch_optional(&mut **tx).await?;
            sqlx::query("SELECT id FROM mtls_user_identities WHERE id=$1 FOR UPDATE").bind(id).fetch_optional(&mut **tx).await?;
            let source:Option<Source>=sqlx::query_as(SOURCE).bind(id).bind(user).bind(ca).bind(now).fetch_optional(&mut **tx).await?;
            let Some(source)=source.filter(|s|!s.archived) else {return work.finish_on(tx,false,0,Some("identity_removed")).await;};
            if !source.eligible {
                sqlx::query("UPDATE mtls_user_identities SET next_action_at=NULL WHERE id=$1").bind(id).execute(&mut **tx).await?;
                return work.finish_on(tx,false,0,None).await;
            }
            if !source.due {return work.finish_on(tx,false,0,None).await;}
            match prepared {
                Preparation::Signed(candidate)=>{identities::commit_user(tx,&secrets,candidate).await?;}
                Preparation::Failed{revision,version,code}=>{
                    if source.revision!=revision || source.published_version!=version {
                        return work.finish_on(tx,true,1,Some("identity_preparation_required")).await;
                    }
                    sqlx::query("UPDATE mtls_user_identities SET maintenance_error=$2,next_action_at=clock_timestamp()+interval '1 hour' WHERE id=$1")
                        .bind(id).bind(code).execute(&mut **tx).await?;
                }
                Preparation::Skipped=>return work.finish_on(tx,true,1,Some("identity_preparation_required")).await,
            }
            work.finish_on(tx,false,0,None).await
        })
    }).await
}

#[cfg(test)]
#[path = "identity_maintenance_tests.rs"]
mod tests;
