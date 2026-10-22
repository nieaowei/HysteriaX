//! Identity scheduling is derived from an already committed CA publication.
//! Material visibility itself is atomic through mtls_user_version_publication.
use super::*;
use crate::background::maintenance::{self, Work};
const KIND: &str = "mtls-identity-publication-schedule";

pub(super) async fn enqueue_on(
    tx: &mut Transaction<'_, Postgres>,
    rotation: &str,
    ca: &str,
    version: i64,
) -> Result<()> {
    sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING")
        .bind(format!("identity-publication:{rotation}")).bind(KIND).bind(json!({"rotation_id":rotation,"ca_id":ca,"ca_version":version,"cursor":"","after_marker":-1,"lease_resources":[format!("ca:{ca}")]})).execute(&mut **tx).await?;
    Ok(())
}
pub(super) async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        match maintenance::claim(&state.pool, KIND, &state.instance_id).await {
            Ok(Some(work)) => execute(&state.pool, &work).await,
            Ok(None) => {}
            Err(error) => tracing::warn!(%error,"identity publication scheduling deferred"),
        }
    }
}
pub(super) async fn pass(state: &AppState, rotation: &str) -> Result<()> {
    if let Some(work) = maintenance::claim_id(
        &state.pool,
        KIND,
        &state.instance_id,
        &format!("identity-publication:{rotation}"),
    )
    .await?
    {
        execute(&state.pool, &work).await;
    }
    Ok(())
}
async fn execute(pool: &PgPool, work: &Work) {
    let _ = crate::execution::supervise(pool.clone(), work.leases.clone(), async {
        if let Err(error) = page(pool, work).await {
            tracing::warn!(%error,work_id=%work.id,"identity schedule deferred");
            work.retry(pool).await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
}
#[derive(Clone)]
struct Target {
    rotation: String,
    ca: String,
    version: i64,
    cursor: String,
    after_marker: i64,
}
impl Target {
    fn from_work(work: &Work) -> Result<Self> {
        let t = Self {
            rotation: work.payload["rotation_id"]
                .as_str()
                .context("missing rotation")?
                .into(),
            ca: work.payload["ca_id"].as_str().context("missing CA")?.into(),
            version: work.payload["ca_version"]
                .as_i64()
                .context("missing generation")?,
            after_marker: work.payload["after_marker"].as_i64().unwrap_or(-1),
            cursor: work.payload["cursor"]
                .as_str()
                .context("missing publication cursor")?
                .into(),
        };
        ensure!(
            work.id == format!("identity-publication:{}", t.rotation)
                && work
                    .leases
                    .iter()
                    .any(|l| l.resource == format!("ca:{}", t.ca)),
            "wrong identity publication scope"
        );
        Ok(t)
    }
}
const PENDING: &str = "SELECT * FROM ((SELECT i.id,i.user_id,0::bigint AS marker FROM mtls_user_identities i WHERE i.ca_id=$1 AND NOT i.archived AND i.scheduled_ca_version IS NULL AND (0::bigint,i.user_id)>($3,$4) AND EXISTS(SELECT 1 FROM users u WHERE u.id=i.user_id) ORDER BY i.user_id,i.id LIMIT $5) UNION ALL (SELECT i.id,i.user_id,i.scheduled_ca_version AS marker FROM mtls_user_identities i WHERE i.ca_id=$1 AND NOT i.archived AND i.scheduled_ca_version<$2 AND (i.scheduled_ca_version,i.user_id)>($3,$4) AND EXISTS(SELECT 1 FROM users u WHERE u.id=i.user_id) ORDER BY i.scheduled_ca_version,i.user_id,i.id LIMIT $5)) pending ORDER BY marker,user_id,id LIMIT $5";

async fn page(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::serializable(pool,move|tx| {let work=work.clone();Box::pin(async move {
        work.guard_on(tx).await?;let t=Target::from_work(&work)?;
        let rows=sqlx::query(PENDING).bind(&t.ca).bind(t.version).bind(t.after_marker).bind(&t.cursor).bind(PAGE).fetch_all(&mut **tx).await?;
        // Lock user parents in fixed order before CA parents and identities.
        // Busy users and identities remain unfinished and are revisited when
        // the cursor wraps; no missing candidate is mistaken for completion.
        let candidates:Vec<String>=rows.iter().map(|row|row.get("user_id")).collect();
        let users:Vec<String>=sqlx::query_scalar("SELECT id FROM users WHERE id=ANY($1::text[]) ORDER BY id FOR NO KEY UPDATE SKIP LOCKED")
            .bind(candidates).fetch_all(&mut **tx).await?;
        sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR KEY SHARE").bind(&t.ca).fetch_optional(&mut **tx).await?;
        sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE").bind(&t.ca).fetch_optional(&mut **tx).await?;
        let current:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_rotations r JOIN mtls_authorities a ON a.credential_id=r.ca_id JOIN credentials c ON c.id=a.credential_id JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.id=$1 AND r.ca_id=$2 AND r.ca_version=$3 AND r.outcome IS NULL AND r.published_at IS NOT NULL AND g.published_at IS NOT NULL AND a.published_version=$3 AND NOT c.archived)")
            .bind(&t.rotation).bind(&t.ca).bind(t.version).fetch_one(&mut **tx).await?;
        let published:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=$2 AND published_at IS NOT NULL)").bind(&t.ca).bind(t.version).fetch_one(&mut **tx).await?;
        if !published {return work.finish_on(tx,false,0,Some("identity_publication_superseded")).await;}
        let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM mtls_user_identities WHERE ca_id=$1 AND user_id=ANY($2::text[]) AND NOT archived AND coalesce(scheduled_ca_version,0)<$3 ORDER BY user_id,id FOR UPDATE SKIP LOCKED")
            .bind(&t.ca).bind(users).bind(t.version).fetch_all(&mut **tx).await?;
        let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
        if current {sqlx::query("UPDATE mtls_user_identities i SET next_action_at=CASE WHEN EXISTS(SELECT 1 FROM users u WHERE u.id=i.user_id AND u.enabled AND (u.expires_at IS NULL OR u.expires_at>$3) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes)) THEN (SELECT CASE WHEN v.next_action_at<=$3 AND v.expires_at>=(SELECT expires_at-interval '5 minutes' FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=$2) THEN $3+interval '1 hour' ELSE greatest($3,v.next_action_at) END FROM mtls_user_version_publication v WHERE v.identity_id=i.id AND v.ca_version=$2 AND v.released AND NOT v.discarded ORDER BY v.credential_version DESC LIMIT 1) ELSE NULL END,maintenance_error=CASE WHEN (SELECT v.next_action_at<=$3 AND v.expires_at>=(SELECT expires_at-interval '5 minutes' FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=$2) FROM mtls_user_version_publication v WHERE v.identity_id=i.id AND v.ca_version=$2 AND v.released AND NOT v.discarded ORDER BY v.credential_version DESC LIMIT 1) THEN 'CA update required: cannot extend the existing certificate' ELSE NULL END,scheduled_ca_version=$2,revision=revision+1,updated_at=$3 WHERE i.ca_id=$1 AND i.id=ANY($4::text[]) AND NOT i.archived AND coalesce(i.scheduled_ca_version,0)<$2")
            .bind(&t.ca).bind(t.version).bind(now).bind(ids).execute(&mut **tx).await?;}
        // Readers already observe release atomically through the generation.
        // Materialize its legacy flag in separately bounded rows so a completed
        // outbox also provides a verifiable old-image rollback compatibility gate.
        sqlx::query("UPDATE mtls_user_versions SET released=true WHERE (identity_id,credential_version) IN (SELECT identity_id,credential_version FROM mtls_user_versions WHERE ca_id=$1 AND ca_version=$2 AND NOT released AND NOT discarded ORDER BY identity_id,credential_version LIMIT $3 FOR UPDATE SKIP LOCKED)")
            .bind(&t.ca).bind(t.version).bind(PAGE).execute(&mut **tx).await?;
        let pending_flags:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_user_versions WHERE ca_id=$1 AND ca_version=$2 AND NOT released AND NOT discarded)").bind(&t.ca).bind(t.version).fetch_one(&mut **tx).await?;
        let remaining:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_user_identities i JOIN users u ON u.id=i.user_id WHERE i.ca_id=$1 AND NOT i.archived AND (i.scheduled_ca_version IS NULL OR i.scheduled_ca_version<$2))")
            .bind(&t.ca).bind(t.version).fetch_one(&mut **tx).await?;
        let cursor=rows.last().map(|r|r.get::<String,_>("user_id")).unwrap_or_default();
        let marker=rows.last().map(|r|r.get::<i64,_>("marker")).unwrap_or(-1);
        sqlx::query("UPDATE background_work SET payload=jsonb_set(jsonb_set(payload,'{cursor}',to_jsonb($2::text)),'{after_marker}',to_jsonb($3::bigint)) WHERE id=$1").bind(&work.id).bind(cursor).bind(marker).execute(&mut **tx).await?;
        let more=pending_flags || current && remaining;
        work.finish_on(tx,more,if more {1}else{0},if current {None}else{Some("identity_publication_superseded")}).await
    })}).await
}

#[cfg(test)]
mod tests;
