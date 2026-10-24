use super::super::identities::{self, PreparedIdentity};
use crate::{
    db,
    deployment::JobInput,
    execution::{self, Execution},
    security::SecretBox,
};
use anyhow::{Context, Result};
use sqlx::{PgPool, Postgres, Row, Transaction};

#[derive(Clone)]
pub(super) struct Source {
    job: JobInput,
    revision: i64,
    authorization_generation: i64,
    pub(super) users: Vec<String>,
    leases: Vec<Execution>,
    target: Option<(String, i64)>,
    identity_ids: Vec<String>,
}
async fn job_on(tx: &mut Transaction<'_, Postgres>, job: &JobInput) -> Result<()> {
    execution::require_job_scope(&job.id, job.node_id.as_deref())?;
    anyhow::ensure!(
        matches!(job.kind.as_str(), "deploy" | "sync" | "rollback"),
        "identity preparation requires a deployment job"
    );
    let row=sqlx::query("SELECT kind,node_id,target_revision,payload_json,attempts FROM jobs WHERE id=$1 AND status='running' FOR UPDATE")
        .bind(&job.id).fetch_optional(&mut **tx).await?.context("deployment job is no longer running")?;
    anyhow::ensure!(
        row.get::<String, _>("kind") == job.kind
            && row.get::<Option<String>, _>("node_id") == job.node_id
            && row.get::<Option<i64>, _>("target_revision") == job.target_revision
            && row.get::<serde_json::Value, _>("payload_json") == job.payload
            && row.get::<i64, _>("attempts") == job.attempts,
        "deployment job source changed"
    );
    let keys = vec![
        format!("job:{}", job.id),
        format!(
            "node:{}",
            job.node_id.as_deref().context("deployment node missing")?
        ),
    ];
    let uncertain:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM execution_operations WHERE (work_id=$1 OR resource_key=ANY($2)) AND state IN ('dispatching','reconciling','needs_attention'))")
        .bind(&job.id).bind(keys).fetch_one(&mut **tx).await?;
    anyhow::ensure!(!uncertain, "deployment has an unverified remote step");
    Ok(())
}
async fn node_on(tx: &mut Transaction<'_, Postgres>, job: &JobInput) -> Result<(i64, i64)> {
    let node = job.node_id.as_deref().context("deployment node missing")?;
    let row = sqlx::query("SELECT n.desired_revision,v.generation FROM nodes n JOIN node_authorization_versions v ON v.node_id=n.id WHERE n.id=$1 AND n.state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE OF n FOR SHARE OF v")
        .bind(node).fetch_optional(&mut **tx).await?.context("deployment node or authorization source is missing")?;
    Ok((row.get("desired_revision"), row.get("generation")))
}

pub(super) async fn capture(pool: &PgPool, job: &JobInput) -> Result<Source> {
    let job = job.clone();
    db::serializable(pool,move |tx|{let job=job.clone();Box::pin(async move {
        job_on(tx,&job).await?;
        let mut keys=vec![format!("job:{}",job.id),format!("node:{}",job.node_id.as_deref().context("deployment node missing")?)];keys.sort();
        let rows=sqlx::query("SELECT resource_key,owner,epoch,generation FROM resource_leases WHERE resource_key=ANY($1) AND work_id=$2 AND owner IS NOT NULL AND epoch=hysteriax_execution_epoch() AND expires_at>clock_timestamp() AND NOT blocked ORDER BY resource_key FOR UPDATE")
            .bind(&keys).bind(&job.id).fetch_all(&mut **tx).await?;
        anyhow::ensure!(rows.len()==keys.len(),"deployment identity preparation requires full execution leases");
        let leases:Vec<Execution>=rows.iter().map(|r|Execution{resource:r.get("resource_key"),owner:r.get("owner"),epoch:r.get("epoch"),generation:r.get("generation"),work:job.id.clone()}).collect();
        anyhow::ensure!(leases.iter().all(|l|l.owner==leases[0].owner),"deployment identity preparation has mixed owners");
        let (revision,authorization_generation)=node_on(tx,&job).await?;
        Ok(Source{job,revision,authorization_generation,users:vec![],leases,target:None,identity_ids:vec![]})
    })}).await
}
async fn current_on(tx: &mut Transaction<'_, Postgres>, s: &Source) -> Result<()> {
    job_on(tx, &s.job).await?;
    for lease in &s.leases {
        lease.guard(tx).await?;
    }
    let (revision, authorization_generation) = node_on(tx, &s.job).await?;
    anyhow::ensure!(
        revision == s.revision && authorization_generation == s.authorization_generation,
        "node permissions changed during signing; retry deployment"
    );
    Ok(())
}
#[cfg(test)]
pub(super) async fn validate(pool: &PgPool, source: &Source) -> Result<()> {
    let source = source.clone();
    db::serializable(pool, move |tx| {
        let source = source.clone();
        Box::pin(async move { current_on(tx, &source).await })
    })
    .await
}
pub(super) async fn commit(
    pool: &PgPool,
    secrets: &SecretBox,
    source: &Source,
    candidates: Vec<PreparedIdentity>,
) -> Result<()> {
    let (source, secrets) = (source.clone(), secrets.clone());
    db::serializable(pool, move |tx| {
        let (source, secrets, candidates) = (source.clone(), secrets.clone(), candidates.clone());
        Box::pin(async move {
            anyhow::ensure!(
                !source.users.is_empty()
                    && source.users.len() <= 100
                    && candidates.len() == source.users.len()
                    && source.identity_ids.len() == source.users.len(),
                "invalid deployment identity page"
            );
            let (ca, version) = source
                .target
                .as_ref()
                .context("deployment identity target missing")?;
            for ((candidate, user), identity) in candidates
                .iter()
                .zip(&source.users)
                .zip(&source.identity_ids)
            {
                anyhow::ensure!(
                    candidate.matches_deployment(identity, user, ca, *version),
                    "deployment identity candidate belongs to another source"
                );
                execution::require_job_resource_scope(
                    &source.job.id,
                    &format!("identity:{identity}"),
                )?;
            }
            sqlx::query("SELECT id FROM users WHERE id=ANY($1) ORDER BY id FOR NO KEY UPDATE")
                .bind(&source.users)
                .fetch_all(&mut **tx)
                .await?;
            current_on(tx, &source).await?;
            for candidate in candidates {
                identities::commit_user(tx, &secrets, candidate).await?;
            }
            Ok(())
        })
    })
    .await
}

const PAGE: i64 = 100;

async fn pending_page(pool: &PgPool, source: &Source, ca: &str, version: i64) -> Result<Source> {
    let (source, ca) = (source.clone(), ca.to_owned());
    db::serializable(pool,move|tx| {
        let (mut source,ca)=(source.clone(),ca.clone());
        Box::pin(async move {
            current_on(tx,&source).await?;
            let now:chrono::DateTime<chrono::Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_ca_generations g JOIN credentials c ON c.id=g.ca_id WHERE g.ca_id=$1 AND g.ca_version=$2 AND g.state<>'retired' AND g.expires_at>$3 AND NOT c.archived)")
                .bind(&ca).bind(version).bind(now).fetch_one(&mut **tx).await?;
            anyhow::ensure!(valid,"deployment CA expired, retired or was archived");
            // Committed certificates are durable progress. Query pending owners
            // instead of skipping a numeric cursor past users enabled or newly
            // due while earlier pages were being signed. Inactive owners still
            // receive an empty shared identity for future regular maintenance.
            source.users=sqlx::query_scalar("SELECT a.user_id FROM node_assignments a JOIN users u ON u.id=a.user_id LEFT JOIN mtls_user_identities i ON i.user_id=a.user_id AND i.ca_id=$2 WHERE a.node_id=$1 AND (i.id IS NULL OR i.archived OR (u.enabled AND (u.expires_at IS NULL OR u.expires_at>$4) AND (u.quota_bytes IS NULL OR u.usage_bytes<u.quota_bytes) AND NOT EXISTS(SELECT 1 FROM mtls_user_versions v JOIN mtls_ca_generations g ON g.ca_id=v.ca_id AND g.ca_version=v.ca_version WHERE v.identity_id=i.id AND v.ca_id=$2 AND v.ca_version=$3 AND NOT v.discarded AND v.not_before<=$4 AND v.expires_at>$4 AND (v.next_action_at>$4 OR v.expires_at>=g.expires_at-interval '5 minutes')))) ORDER BY a.user_id LIMIT $5")
                .bind(&source.job.node_id).bind(&ca).bind(version).bind(now).bind(PAGE).fetch_all(&mut **tx).await?;
            source.identity_ids.clear();
            for user in &source.users {
                // No signing material exists yet. The committed shared row fixes
                // the resource ID before any signer competes for its lease.
                sqlx::query("INSERT INTO mtls_user_identities(id,user_id,ca_id,created_at,updated_at) VALUES(gen_random_uuid()::text,$1,$2,clock_timestamp(),clock_timestamp()) ON CONFLICT(user_id,ca_id) DO NOTHING")
                    .bind(user).bind(&ca).execute(&mut **tx).await?;
                let identity:String=sqlx::query_scalar("SELECT id FROM mtls_user_identities WHERE user_id=$1 AND ca_id=$2")
                    .bind(user).bind(&ca).fetch_one(&mut **tx).await?;
                source.identity_ids.push(identity);
            }
            source.target=Some((ca,version));
            Ok(source)
        })
    }).await
}

pub(super) async fn prepare(
    pool: &PgPool,
    secrets: &SecretBox,
    job: &JobInput,
    ca: &str,
    version: i64,
) -> Result<()> {
    let source = capture(pool, job).await?;
    loop {
        execution::check_current().await?;
        let page = pending_page(pool, &source, ca, version).await?;
        if page.users.is_empty() {
            return Ok(());
        }
        let signed = execution::with_identity_leases(&page.identity_ids, async {
            let mut candidates = Vec::with_capacity(page.users.len());
            for user in &page.users {
                execution::check_current().await?;
                candidates
                    .push(identities::prepare_user(pool, secrets, user, ca, version, false).await?);
            }
            commit(pool, secrets, &page, candidates).await
        })
        .await?;
        if signed.is_none() {
            execution::check_current().await?;
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
}

/// Rollback is read-only: walk at most one hundred owners in each snapshot and
/// fence every page against the same node configuration and membership source.
pub(super) async fn validate_rollback(
    pool: &PgPool,
    job: &JobInput,
    ca: &str,
    version: i64,
    versions: &[i64],
) -> Result<()> {
    let source = capture(pool, job).await?;
    let mut after = String::new();
    let mut deadline = None::<chrono::DateTime<chrono::Utc>>;
    loop {
        execution::check_current().await?;
        let (source, ca, versions, seek) = (
            source.clone(),
            ca.to_owned(),
            versions.to_vec(),
            after.clone(),
        );
        let (users,next_deadline):(Vec<String>,Option<chrono::DateTime<chrono::Utc>>)=db::serializable(pool,move|tx| {
            let (source,ca,versions,seek)=(source.clone(),ca.clone(),versions.clone(),seek.clone());
            Box::pin(async move {
                current_on(tx,&source).await?;
                let now:chrono::DateTime<chrono::Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
                anyhow::ensure!(deadline.is_none_or(|at|at>now),"rollback unavailable: historical shared certificate expired during preparation");
                let valid_ca:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_ca_generations g JOIN credentials c ON c.id=g.ca_id WHERE g.ca_id=$1 AND g.ca_version=$2 AND g.expires_at>$3 AND NOT c.archived)")
                    .bind(&ca).bind(version).bind(now).fetch_one(&mut **tx).await?;
                anyhow::ensure!(valid_ca,"rollback unavailable: historical CA expired or was archived; rebuild or sync the node");
                let users:Vec<String>=sqlx::query_scalar("SELECT user_id FROM node_assignments WHERE node_id=$1 AND user_id>$2 ORDER BY user_id LIMIT $3")
                    .bind(&source.job.node_id).bind(&seek).bind(PAGE).fetch_all(&mut **tx).await?;
                let mut earliest=deadline;
                for user in &users {
                    let expiry:Option<chrono::DateTime<chrono::Utc>>=sqlx::query_scalar("SELECT max(v.expires_at) FROM mtls_user_identities i JOIN mtls_user_version_publication v ON v.identity_id=i.id WHERE i.user_id=$1 AND i.ca_id=$2 AND NOT i.archived AND v.ca_version=ANY($3) AND v.released AND NOT v.discarded AND v.not_before<=$4 AND v.expires_at>$4")
                        .bind(user).bind(&ca).bind(&versions).bind(now).fetch_one(&mut **tx).await?;
                    let expiry=expiry.context("rollback unavailable: a currently authorized user has no valid historical shared identity; rebuild or sync the node")?;
                    earliest=Some(earliest.map_or(expiry,|at|at.min(expiry)));
                }
                Ok((users,earliest))
            })
        }).await?;
        let Some(last) = users.last() else {
            return Ok(());
        };
        after = last.clone();
        deadline = next_deadline;
        tokio::task::yield_now().await;
    }
}

#[cfg(test)]
mod tests;
