use anyhow::{Context, Result};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use super::{Operation, provider::Cloudflare};
use crate::error::ApiError;

pub async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    connection: &str,
    version: i64,
    batch: &str,
) -> Result<String, ApiError> {
    let row = sqlx::query("SELECT * FROM dns_connections WHERE id=$1")
        .bind(connection)
        .fetch_one(&mut **tx)
        .await?;
    let request = json!({"connection_id":connection,"version":version,"batch_id":batch,"expected_revision":row.get::<i64,_>("revision")});
    let key = format!("credential:{batch}:{connection}:{}", Uuid::new_v4());
    let result = super::enqueue(
        tx,
        Operation {
            key: &key,
            request: &request,
            connection,
            version,
            resource_type: "dns_connection",
            resource: connection,
            resource_name: &row.get::<String, _>("name"),
            resource_key: format!("dns-connection:{connection}"),
            action: "credential-apply",
            payload: request.clone(),
            node: None,
        },
    )
    .await?;
    let job = result["job_id"]
        .as_str()
        .ok_or_else(ApiError::internal)?
        .to_owned();
    sqlx::query("INSERT INTO dns_credential_batch_items(batch_id,connection_id,job_id,expected_revision) VALUES($1,$2,$3,$4) ON CONFLICT(batch_id,connection_id) DO UPDATE SET job_id=excluded.job_id,expected_revision=excluded.expected_revision,applied_at=NULL")
        .bind(batch).bind(connection).bind(&job).bind(row.get::<i64,_>("revision")).execute(&mut **tx).await?;
    Ok(job)
}
#[derive(Clone)]
struct Source {
    job: crate::deployment::JobInput,
    operation: String,
    expected: Value,
    leases: Vec<crate::execution::Execution>,
    data: Value,
}
enum Prepared {
    Applied(Value),
    Ready(Source),
}

/// Capture the complete publication source under the same parent order used
/// for confirmation. Finish this transaction before verifying provider access.
async fn source_on(
    tx: &mut Transaction<'_, Postgres>,
    job: &crate::deployment::JobInput,
    op: &sqlx::postgres::PgRow,
) -> Result<Value> {
    let connection: String = op.get("connection_id");
    let version: i64 = op.get("credential_version");
    let payload: Value = op.get("payload");
    anyhow::ensure!(
        op.get::<String, _>("action") == "credential-apply"
            && op.get::<String, _>("resource_type") == "dns_connection"
            && op.get::<String, _>("resource_id") == connection
            && payload["connection_id"] == connection
            && payload["version"].as_i64() == Some(version)
            && job
                .payload
                .get("dns_credential_version")
                .is_none_or(|value| value.as_i64() == Some(version)),
        "DNS credential operation binding changed"
    );
    let credential: String =
        sqlx::query_scalar("SELECT credential_id FROM dns_connections WHERE id=$1")
            .bind(&connection)
            .fetch_one(&mut **tx)
            .await?;
    let credential_source: Value =
        sqlx::query_scalar("SELECT to_jsonb(c) FROM credentials c WHERE id=$1 FOR SHARE")
            .bind(&credential)
            .fetch_one(&mut **tx)
            .await?;
    anyhow::ensure!(
        credential_source["latest_version"].as_i64() == Some(version)
            && credential_source["archived"] == false
            && credential_source["kind"] == "dns",
        "DNS credential publication was superseded or archived"
    );
    let connection_source:Value=sqlx::query_scalar("SELECT to_jsonb(c)-'verified_at'-'updated_at' FROM dns_connections c WHERE id=$1 FOR UPDATE")
        .bind(&connection).fetch_one(&mut **tx).await?;
    anyhow::ensure!(
        connection_source["credential_id"] == credential
            && connection_source["revision"] == payload["expected_revision"],
        "DNS connection changed while verifying credentials"
    );
    let batch = payload["batch_id"]
        .as_str()
        .context("DNS credential batch missing")?;
    let batch_source: Value =
        sqlx::query_scalar("SELECT to_jsonb(b) FROM credential_batches b WHERE id=$1 FOR SHARE")
            .bind(batch)
            .fetch_one(&mut **tx)
            .await?;
    anyhow::ensure!(
        batch_source["credential_id"] == credential
            && batch_source["version"].as_i64() == Some(version),
        "DNS credential batch source changed"
    );
    let item:Value=sqlx::query_scalar("SELECT to_jsonb(i) FROM dns_credential_batch_items i WHERE batch_id=$1 AND connection_id=$2 FOR UPDATE")
        .bind(batch).bind(&connection).fetch_one(&mut **tx).await?;
    anyhow::ensure!(
        item["job_id"] == job.id
            && item["expected_revision"] == payload["expected_revision"]
            && item["applied_at"].is_null(),
        "DNS credential batch belongs to another job or was applied without a receipt"
    );
    let zones:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(z)-'synced_at' FROM dns_zones z WHERE connection_id=$1 ORDER BY id FOR SHARE")
        .bind(&connection).fetch_all(&mut **tx).await?;
    Ok(
        json!({"connection":connection_source,"credential":credential_source,"batch":batch_source,"item":item,"zones":zones}),
    )
}

async fn prepare(
    pool: &PgPool,
    job: &crate::deployment::JobInput,
    operation: &str,
    expected: &Value,
) -> Result<Prepared> {
    let (job, operation, expected) = (job.clone(), operation.to_owned(), expected.clone());
    crate::db::serializable(pool,move |tx| {let (job,operation,expected)=(job.clone(),operation.clone(),expected.clone());Box::pin(async move {
        let op=super::execution::job_on(tx,&job,&operation,Some(&expected)).await?;
        if op.get::<Option<chrono::DateTime<chrono::Utc>>,_>("applied_at").is_some() {
            return Ok(Prepared::Applied(op.get::<Option<Value>,_>("result").context("DNS credential receipt missing")?));
        }
        let resource=format!("dns-connection:{}",op.get::<String,_>("connection_id"));
        let actual:Option<String>=sqlx::query_scalar("SELECT resource_key FROM jobs WHERE id=$1").bind(&job.id).fetch_one(&mut **tx).await?;
        anyhow::ensure!(actual.as_deref()==Some(&resource),"DNS credential resource binding changed");
        let mut keys=vec![resource,format!("job:{}",job.id)];
        if let Some(node)=&job.node_id {keys.push(format!("node:{node}"));}keys.sort();keys.dedup();
        let rows=sqlx::query("SELECT resource_key,owner,epoch,generation FROM resource_leases WHERE resource_key=ANY($1) AND work_id=$2 AND owner IS NOT NULL AND epoch=hysteriax_execution_epoch() AND expires_at>clock_timestamp() AND NOT blocked ORDER BY resource_key FOR UPDATE")
            .bind(&keys).bind(&job.id).fetch_all(&mut **tx).await?;
        anyhow::ensure!(rows.len()==keys.len(),"DNS credential requires its full execution scope");
        let leases:Vec<crate::execution::Execution>=rows.iter().map(|l|crate::execution::Execution{resource:l.get("resource_key"),work:job.id.clone(),owner:l.get("owner"),epoch:l.get("epoch"),generation:l.get("generation")}).collect();
        anyhow::ensure!(leases.iter().all(|l|l.owner==leases[0].owner),"DNS credential has mixed owners");
        let data=source_on(tx,&job,&op).await?;
        Ok(Prepared::Ready(Source{job,operation,expected,leases,data}))
    })}).await
}

async fn commit(pool: &PgPool, source: &Source) -> Result<Value> {
    let (source, audit) = (source.clone(), Uuid::new_v4().to_string());
    crate::db::serializable(pool,move |tx| {let (s,audit)=(source.clone(),audit.clone());Box::pin(async move {
        let op=super::execution::job_on(tx,&s.job,&s.operation,Some(&s.expected)).await?;
        for lease in &s.leases {lease.guard(tx).await?;}
        if op.get::<Option<chrono::DateTime<chrono::Utc>>,_>("applied_at").is_some() {
            return op.get::<Option<Value>,_>("result").context("DNS credential receipt missing");
        }
        anyhow::ensure!(source_on(tx,&s.job,&op).await?==s.data,"DNS credential verification source changed");
        let connection=op.get::<String,_>("connection_id");let version=op.get::<i64,_>("credential_version");
        sqlx::query("UPDATE dns_connections SET credential_version=$1,revision=revision+CASE WHEN credential_version<>$1 THEN 1 ELSE 0 END,status='verified',verified_at=clock_timestamp(),updated_at=clock_timestamp() WHERE id=$2")
            .bind(version).bind(&connection).execute(&mut **tx).await?;
        sqlx::query("UPDATE dns_credential_batch_items SET applied_at=clock_timestamp() WHERE batch_id=$1 AND connection_id=$2 AND job_id=$3 AND applied_at IS NULL")
            .bind(s.data["batch"]["id"].as_str()).bind(&connection).bind(&s.job.id).execute(&mut **tx).await?;
        let result=json!({"connection_id":connection,"credential_version":version,"read_access_verified":true});
        sqlx::query("UPDATE dns_operations SET result=$1,applied_at=clock_timestamp() WHERE id=$2 AND job_id=$3 AND applied_at IS NULL")
            .bind(&result).bind(&s.operation).bind(&s.job.id).execute(&mut **tx).await?;
        sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,'admin','dns.credential-apply','dns',$2,$3,clock_timestamp())")
            .bind(audit).bind(&connection).bind(json!({"operation_id":s.operation,"job_id":s.job.id})).execute(&mut **tx).await?;
        Ok(result)
    })}).await
}

pub async fn execute(
    pool: &PgPool,
    secrets: &crate::security::SecretBox,
    job: &crate::deployment::JobInput,
    operation: &str,
    expected: &Value,
) -> Result<Value> {
    // Resolve the immutable credential named by this preparation, rather than
    // rereading a mutable connection after the provider has been constructed.
    execute_with(
        pool,
        job,
        operation,
        expected,
        |credential, version| async move {
            super::cloudflare_credential(pool, secrets, &credential, version, true).await
        },
    )
    .await
}

async fn execute_with<F, Fut>(
    pool: &PgPool,
    job: &crate::deployment::JobInput,
    operation: &str,
    expected: &Value,
    load: F,
) -> Result<Value>
where
    F: FnOnce(String, i64) -> Fut,
    Fut: std::future::Future<Output = Result<Cloudflare>>,
{
    let source = match prepare(pool, job, operation, expected).await? {
        Prepared::Applied(receipt) => return Ok(receipt),
        Prepared::Ready(source) => source,
    };
    let credential = source.data["credential"]["id"]
        .as_str()
        .context("DNS credential ID missing")?
        .to_owned();
    let version = source.expected["credential_version"]
        .as_i64()
        .context("DNS credential version missing")?;
    let provider = load(credential, version).await?;
    verify(pool, &provider, source).await
}

#[cfg(test)]
async fn apply(
    pool: &PgPool,
    provider: &Cloudflare,
    job: &crate::deployment::JobInput,
    operation: &str,
    expected: &Value,
) -> Result<Value> {
    let source = match prepare(pool, job, operation, expected).await? {
        Prepared::Applied(receipt) => return Ok(receipt),
        Prepared::Ready(source) => source,
    };
    verify(pool, provider, source).await
}

async fn verify(pool: &PgPool, provider: &Cloudflare, source: Source) -> Result<Value> {
    provider.zones().await?;
    for zone in source.data["zones"]
        .as_array()
        .context("DNS credential zones missing")?
    {
        if zone["enabled"] == true {
            let id = zone["provider_zone_id"]
                .as_str()
                .context("DNS credential zone ID missing")?;
            provider.zone(id).await?;
            provider.records(id).await?;
        }
    }
    commit(pool, &source).await
}

#[cfg(test)]
mod loader_tests;
#[cfg(test)]
mod tests;
