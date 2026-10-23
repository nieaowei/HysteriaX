use super::*;
use crate::db::RetryableTransactionError;
use anyhow::Context;

fn transient(error: &anyhow::Error) -> bool {
    if error.retryable_transaction() {
        return true;
    }
    error.chain().any(|cause| {
        let Some(error) = cause.downcast_ref::<sqlx::Error>() else {
            return false;
        };
        match error {
            sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolTimedOut => true,
            sqlx::Error::Database(error) => error.code().is_some_and(|code| {
                code.starts_with("08")
                    || code.starts_with("53")
                    || matches!(
                        code.as_ref(),
                        "55P03" | "57014" | "57P01" | "57P02" | "57P03"
                    )
            }),
            _ => false,
        }
    })
}

/// Retry only persistence of an already captured result; never repeat remote
/// work. Keep domain/source errors intact so they cannot renew forever.
pub(super) async fn retry_persistence<T, F, Fut>(mut persist: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut delay = 1;
    loop {
        match persist().await {
            Ok(value) => return Ok(value),
            Err(error) if transient(&error) => {
                tracing::warn!(%error,"job persistence deferred without repeating remote work");
            }
            Err(error) => return Err(error),
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(30);
    }
}

/// A rejected publication after remote work requires explicit reconciliation.
/// This is a local publication barrier, not evidence about remote completion.
/// Return whether the caller may release its resources normally.
pub(super) async fn reject_completion(
    pool: &PgPool,
    job: &JobInput,
    message: &str,
) -> anyhow::Result<bool> {
    completion_barrier(pool, job, message, true).await
}

/// Preserve a remote publication barrier even if saving the failure event was
/// rejected. This records no business outcome; recovery still owes that event.
pub(super) async fn quarantine_completion(pool: &PgPool, job: &JobInput) -> anyhow::Result<bool> {
    completion_barrier(pool, job, "", false).await
}

async fn completion_barrier(
    pool: &PgPool,
    job: &JobInput,
    message: &str,
    record_failure: bool,
) -> anyhow::Result<bool> {
    crate::execution::require_job_scope(&job.id, job.node_id.as_deref())?;
    let (job, message, id) = (
        job.clone(),
        message.to_owned(),
        uuid::Uuid::new_v4().to_string(),
    );
    db::write(pool,move |tx| {
        let (job,message,id)=(job.clone(),message.clone(),id.clone());
        Box::pin(async move {
            let Some(row)=sqlx::query("SELECT kind,node_id,target_revision,payload_json,attempts,resource_key FROM jobs WHERE id=$1 AND status='running' FOR UPDATE")
                .bind(&job.id).fetch_optional(&mut **tx).await? else { return Ok(true); };
            anyhow::ensure!(row.get::<String,_>("kind")==job.kind && row.get::<Option<String>,_>("node_id")==job.node_id
                && row.get::<Option<i64>,_>("target_revision")==job.target_revision && row.get::<Value,_>("payload_json")==job.payload
                && row.get::<i64,_>("attempts")==job.attempts,"rejected completion source changed");
            let mut keys=vec![format!("job:{}",job.id)];
            if let Some(node)=&job.node_id {keys.push(format!("node:{node}"));}
            let extra:Option<String>=row.get("resource_key");
            if let Some(resource)=&extra {crate::execution::require_job_resource_scope(&job.id,resource)?;keys.push(resource.clone());}
            let remote:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM execution_operations WHERE work_id=$1)")
                .bind(&job.id).fetch_one(&mut **tx).await?;
            if remote {
                let resource=job.node_id.as_ref().map(|n|format!("node:{n}")).or(extra).unwrap_or_else(||format!("job:{}",job.id));
                let lease=sqlx::query("SELECT owner,generation FROM resource_leases WHERE resource_key=$1 AND work_id=$2 AND owner IS NOT NULL AND expires_at>clock_timestamp() AND NOT blocked FOR UPDATE")
                    .bind(&resource).bind(&job.id).fetch_optional(&mut **tx).await?.context("rejected completion lease was lost")?;
                sqlx::query("INSERT INTO execution_operations(operation_id,resource_key,owner,generation,work_id,step,state,intent,evidence) VALUES($1,$2,$3,$4,$5,'completion-rejected','needs_attention','{\"kind\":\"publication_barrier\"}','{\"reason\":\"completion_rejected\"}') ON CONFLICT(work_id,step) DO NOTHING")
                    .bind(id).bind(resource).bind(lease.get::<String,_>("owner")).bind(lease.get::<i64,_>("generation")).bind(&job.id).execute(&mut **tx).await?;
            }
            if record_failure {fail_on(tx,&job,&message,false,false,false,false,None).await?;}
            if remote {
                // guard_current held the exact incarnations throughout this
                // transaction; block them together with the failed event.
                sqlx::query("UPDATE resource_leases SET blocked=true WHERE resource_key=ANY($1) AND work_id=$2")
                    .bind(keys).bind(&job.id).execute(&mut **tx).await?;
            }
            Ok(!remote)
        })
    }).await
}

#[cfg(test)]
mod tests;
