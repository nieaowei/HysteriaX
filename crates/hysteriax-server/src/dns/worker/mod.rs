mod records;
mod refresh;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::provider::{Cloudflare, editable};
use crate::{
    db,
    deployment::{JobInput, JobOutput},
    security::SecretBox,
};

pub async fn execute(pool: &PgPool, secrets: &SecretBox, job: &JobInput) -> Result<JobOutput> {
    let id = job.payload["dns_operation_id"]
        .as_str()
        .context("missing DNS operation")?;
    let (input, operation) = (job.clone(), id.to_owned());
    let row = db::write(pool, move |tx| {
        let (job, id) = (input.clone(), operation.clone());
        Box::pin(async move { records::job_on(tx, &job, &id, None).await })
    })
    .await?;
    let operation_source: Value = row.get("frozen_source");
    if row
        .get::<Option<chrono::DateTime<chrono::Utc>>, _>("applied_at")
        .is_some()
    {
        return Ok(output(
            row.get::<Option<Value>, _>("result")
                .unwrap_or_else(|| json!({})),
        ));
    }
    let connection: String = row.get("connection_id");
    let version: i64 = job.payload["dns_credential_version"]
        .as_i64()
        .unwrap_or_else(|| row.get("credential_version"));
    let action: String = row.get("action");
    let resource: String = row.get("resource_id");
    let payload: Value = row.get("payload");
    if action == "record-check" {
        return super::probe::execute_job(pool, job, id, &operation_source)
            .await
            .map(output);
    }
    if action == "credential-apply" {
        return super::rotation::execute(pool, secrets, job, id, &operation_source)
            .await
            .map(output);
    }
    let provider = super::provider(pool, secrets, &connection, version).await?;
    let result = match action.as_str() {
        "verify" | "connection-refresh" => {
            refresh::connection(pool, &provider, job, id, &operation_source).await?
        }
        "zone-refresh" => refresh::zone(pool, &provider, job, id, &operation_source).await?,
        "record-create" | "record-update" | "record-delete" => {
            write_record(pool, &provider, job, id, &action, &resource, &payload).await?
        }
        _ => bail!("unsupported DNS operation"),
    };
    let (input, operation, expected, action, resource, saved, audit_id) = (
        job.clone(),
        id.to_owned(),
        operation_source,
        action,
        resource,
        result.clone(),
        Uuid::new_v4().to_string(),
    );
    db::write(pool,move |tx| {let (job,id,expected,action,resource,result,audit_id)=(input.clone(),operation.clone(),expected.clone(),action.clone(),resource.clone(),saved.clone(),audit_id.clone());Box::pin(async move {
        records::job_on(tx,&job,&id,Some(&expected)).await?;
        let changed=sqlx::query("UPDATE dns_operations SET result=$1,applied_at=clock_timestamp() WHERE id=$2 AND applied_at IS NULL")
            .bind(&result).bind(&id).execute(&mut **tx).await?;
        if changed.rows_affected()!=0 {
            sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,'admin',$2,'dns',$3,$4,clock_timestamp())")
                .bind(audit_id).bind(format!("dns.{action}")).bind(resource).bind(json!({"operation_id":id,"job_id":job.id})).execute(&mut **tx).await?;
        }
        Ok::<_,anyhow::Error>(())
    })}).await?;
    Ok(output(result))
}
fn output(result: Value) -> JobOutput {
    JobOutput {
        stage: "dns_completed".into(),
        result,
        node_state: None,
        deployed_revision: None,
        deployed_config: None,
        deployed_sha256: None,
        published_connection: None,
        delete_node: false,
    }
}

async fn write_record(
    pool: &PgPool,
    provider: &Cloudflare,
    job: &JobInput,
    operation: &str,
    action: &str,
    id: &str,
    payload: &Value,
) -> Result<Value> {
    let source = match records::prepare(pool, job, operation, action, id, payload).await? {
        records::Prepared::Applied(receipt) => return Ok(receipt),
        records::Prepared::Ready(source) => source,
    };
    let zone = &source.zone;
    let remote_id = &source.remote;
    let desired = &payload["desired"];
    let result = if action == "record-create" {
        let records = provider.records(zone).await?;
        let marker = format!("HysteriaX operation:{operation}");
        if let Some(created) = records
            .iter()
            .find(|r| r["comment"].as_str() == Some(&marker))
        {
            if editable(created) != *desired {
                bail!("created record changed externally; review before retrying");
            }
            created.clone()
        } else {
            if records.iter().any(|r| {
                r["name"] == desired["name"]
                    && (r["type"] == desired["type"]
                        || r["type"] == "NS"
                        || r["type"] == "CNAME"
                        || (desired["type"] == "CNAME"
                            && matches!(r["type"].as_str(), Some("A" | "AAAA"))))
            }) {
                bail!(
                    "record name conflicts with an existing remote record; refresh and select it explicitly"
                );
            }
            provider.create(zone, desired, operation).await?
        }
    } else {
        let remote_id = remote_id
            .as_deref()
            .context("provider record ID is missing")?;
        let current = provider.record(zone, remote_id).await?;
        if action == "record-delete" && current.is_none() {
            json!({"deleted":true})
        } else {
            let current =
                current.context("record was removed externally; refresh before retrying")?;
            if action == "record-update" && editable(&current) == *desired {
                current
            } else {
                if editable(&current) != editable(&payload["baseline"]) {
                    bail!("record changed externally; refresh and review the differences");
                }
                if action == "record-delete" {
                    provider.delete(zone, remote_id).await?;
                    json!({"deleted":true})
                } else {
                    provider.update(zone, remote_id, desired).await?
                }
            }
        }
    };
    records::commit(pool, &source, &result).await
}

#[cfg(test)]
mod tests;
