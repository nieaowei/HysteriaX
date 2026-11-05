use super::CURRENT;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteStep {
    pub operation_id: String,
    pub resource: String,
    pub owner: String,
    pub generation: i64,
    pub epoch: String,
    pub work: String,
    pub step: String,
    pub kind: String,
    pub request_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predecessor: Option<String>,
}

/// Read the immutable task-local resource before preparing remote metadata.
pub(crate) fn resource_for(kind: &str) -> anyhow::Result<Option<super::Execution>> {
    let Ok(scope) = CURRENT.try_with(Clone::clone) else {
        return Ok(None);
    };
    let prefix = if kind.starts_with("dns") {
        "dns-"
    } else {
        "node:"
    };
    let lease = scope
        .leases
        .iter()
        .find(|l| l.resource.starts_with(prefix))
        .or_else(|| scope.leases.first())
        .ok_or_else(|| anyhow::anyhow!("remote work requires a resource lease"))?;
    Ok(Some(lease.clone()))
}

pub async fn run<T>(
    kind: &str,
    digest: String,
    request: impl std::future::Future<Output = anyhow::Result<T>>,
    evidence: impl FnOnce(&T) -> Value,
) -> anyhow::Result<T> {
    with_context(kind, digest, Value::Null, move |_| request, evidence).await
}

/// The factory itself runs only after the durable intent has committed.
pub(crate) async fn with_context<T, F: std::future::Future<Output = anyhow::Result<T>>>(
    kind: &str,
    digest: String,
    verification: Value,
    request: impl FnOnce(Option<RemoteStep>) -> F,
    evidence: impl FnOnce(&T) -> Value,
) -> anyhow::Result<T> {
    let Some(scope) = CURRENT.try_with(Clone::clone).ok() else {
        // Only historical unit fixtures may bypass leased remote execution.
        #[cfg(test)]
        return request(None).await;
        #[cfg(not(test))]
        anyhow::bail!("mutating remote work requires an execution scope");
    };
    let prefix = if kind.starts_with("dns") {
        "dns-"
    } else {
        "node:"
    };
    let lease = scope
        .leases
        .iter()
        .find(|lease| lease.resource.starts_with(prefix))
        .or_else(|| scope.leases.first())
        .ok_or_else(|| anyhow::anyhow!("remote work requires a resource lease"))?
        .clone();
    let index = scope
        .sequence
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let step = format!("{kind}:{index}");
    let predecessor = verification
        .get("expected_head")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut intent = serde_json::json!({"kind":kind,"request_digest":digest});
    if !verification.is_null() {
        intent["verification"] = verification;
    }
    let job = scope
        .leases
        .iter()
        .find(|l| l.resource.starts_with("job:"))
        .map(|l| l.work.clone());
    let prepared_id = uuid::Uuid::new_v4().to_string();
    let (operation,fresh)=crate::db::write(&scope.pool,|tx| {
        let (lease,job,step,intent,id)=(lease.clone(),job.clone(),step.clone(),intent.clone(),prepared_id.clone());
        Box::pin(async move {
            if let Some(job)=job {
                let outcome:Option<(Option<String>,)>=sqlx::query_as("SELECT r.outcome FROM mtls_node_operations o JOIN mtls_rotations r ON r.id=o.rotation_id WHERE o.job_id=$1 FOR KEY SHARE OF r")
                    .bind(job).fetch_optional(&mut **tx).await?;
                anyhow::ensure!(outcome.is_none_or(|(outcome,)|outcome.is_none()),"CA target was superseded before dispatch");
            }
            Ok::<_,anyhow::Error>(lease.dispatch_prepared(tx,&step,&intent,&id).await?)
        })
    }).await?;
    anyhow::ensure!(
        fresh,
        "operation was previously dispatched; remote verification is required before recovery"
    );
    let context = RemoteStep {
        operation_id: operation.clone(),
        resource: lease.resource.clone(),
        owner: lease.owner.clone(),
        generation: lease.generation,
        epoch: lease.epoch.clone(),
        work: lease.work.clone(),
        step,
        kind: kind.into(),
        request_digest: digest,
        predecessor,
    };
    let output = request(Some(context)).await?;
    let evidence = evidence(&output);
    crate::db::write(&scope.pool, |tx| {
        let (lease, operation, evidence) = (lease.clone(), operation.clone(), evidence.clone());
        Box::pin(async move {
            lease.verified(tx, &operation, &evidence).await?;
            Ok::<_, anyhow::Error>(())
        })
    })
    .await?;
    Ok(output)
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "remote_retry_tests.rs"]
mod retry_tests;

#[cfg(test)]
#[path = "remote_predecessor_tests.rs"]
mod predecessor_tests;
