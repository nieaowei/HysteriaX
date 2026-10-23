use crate::deployment::JobInput;
use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::Row;

pub(super) async fn job_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job: &JobInput,
    operation: &str,
    expected: Option<&Value>,
) -> Result<sqlx::postgres::PgRow> {
    crate::execution::require_job_scope(&job.id, job.node_id.as_deref())?;
    let row=sqlx::query("SELECT kind,node_id,target_revision,payload_json,attempts,resource_key FROM jobs WHERE id=$1 AND status='running' FOR UPDATE")
        .bind(&job.id).fetch_optional(&mut **tx).await?.context("DNS job is no longer running")?;
    anyhow::ensure!(
        row.get::<String, _>("kind") == job.kind
            && row.get::<Option<String>, _>("node_id") == job.node_id
            && row.get::<Option<i64>, _>("target_revision") == job.target_revision
            && row.get::<Value, _>("payload_json") == job.payload
            && row.get::<i64, _>("attempts") == job.attempts,
        "DNS job source changed"
    );
    let resource: Option<String> = row.get("resource_key");
    let resource = resource.context("DNS job requires a resource binding")?;
    crate::execution::require_job_resource_scope(&job.id, &resource)?;
    let mut keys = vec![format!("job:{}", job.id), resource];
    if let Some(node) = &job.node_id {
        keys.push(format!("node:{node}"));
    }
    let uncertain:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM execution_operations WHERE (work_id=$1 OR resource_key=ANY($2)) AND state IN ('dispatching','reconciling','needs_attention'))")
        .bind(&job.id).bind(keys).fetch_one(&mut **tx).await?;
    anyhow::ensure!(!uncertain, "DNS execution has an unverified remote step");
    anyhow::ensure!(
        job.payload["dns_operation_id"].as_str() == Some(operation),
        "DNS job operation changed"
    );
    let op=sqlx::query("SELECT o.*,to_jsonb(o)-'result'-'applied_at' AS frozen_source FROM dns_operations o WHERE id=$1 FOR UPDATE")
        .bind(operation).fetch_one(&mut **tx).await?;
    anyhow::ensure!(
        op.get::<Option<String>, _>("job_id").as_deref() == Some(&job.id)
            && job.kind == format!("dns-{}", op.get::<String, _>("action")),
        "DNS operation belongs to a different job or action"
    );
    if let Some(expected) = expected {
        anyhow::ensure!(
            op.get::<Value, _>("frozen_source") == *expected,
            "DNS operation source changed"
        );
    }
    Ok(op)
}
