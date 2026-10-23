use super::*;

pub(super) use super::super::execution::job_on;

#[derive(Clone)]
pub(super) struct Source {
    pub job: JobInput,
    pub operation: String,
    pub action: String,
    pub record: String,
    pub payload: Value,
    pub zone_id: String,
    pub zone: String,
    pub remote: Option<String>,
    pub operation_source: Value,
}
pub(super) enum Prepared {
    Applied(Value),
    Ready(Source),
}

async fn record_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source: &Source,
) -> Result<sqlx::postgres::PgRow> {
    let connection = source.operation_source["connection_id"]
        .as_str()
        .context("DNS connection missing")?;
    sqlx::query("SELECT id FROM dns_connections WHERE id=$1 FOR KEY SHARE")
        .bind(connection)
        .fetch_one(&mut **tx)
        .await?;
    let zone =
        sqlx::query("SELECT provider_zone_id,connection_id FROM dns_zones WHERE id=$1 FOR SHARE")
            .bind(&source.zone_id)
            .fetch_one(&mut **tx)
            .await?;
    let row = sqlx::query("SELECT * FROM dns_records WHERE id=$1 FOR UPDATE")
        .bind(&source.record)
        .fetch_one(&mut **tx)
        .await?;
    let expected = if source.action == "record-delete" {
        json!({"delete":true})
    } else {
        source.payload["desired"].clone()
    };
    anyhow::ensure!(
        source.payload["source_revision"].as_i64() == Some(row.get::<i64, _>("revision"))
            && row.get::<Option<Value>, _>("desired") == Some(expected),
        "DNS record pending source changed or has no revision proof"
    );
    anyhow::ensure!(
        row.get::<String, _>("zone_id") == source.zone_id
            && zone.get::<String, _>("provider_zone_id") == source.zone
            && row.get::<Option<String>, _>("provider_record_id") == source.remote
            && zone.get::<String, _>("connection_id")
                == source.operation_source["connection_id"]
                    .as_str()
                    .context("DNS connection missing")?,
        "DNS record provider binding changed"
    );
    let resource: Option<String> = sqlx::query_scalar("SELECT resource_key FROM jobs WHERE id=$1")
        .bind(&source.job.id)
        .fetch_one(&mut **tx)
        .await?;
    anyhow::ensure!(
        resource == Some(format!("dns-zone:{}", source.zone_id)),
        "DNS record job has a different zone scope"
    );
    Ok(row)
}

pub(super) async fn prepare(
    pool: &PgPool,
    job: &JobInput,
    operation: &str,
    action: &str,
    id: &str,
    payload: &Value,
) -> Result<Prepared> {
    let (job, operation, action, id, payload) = (
        job.clone(),
        operation.to_owned(),
        action.to_owned(),
        id.to_owned(),
        payload.clone(),
    );
    db::write(pool,move |tx| {
        let (job,operation,action,id,payload)=(job.clone(),operation.clone(),action.clone(),id.clone(),payload.clone());
        Box::pin(async move {
            let op=job_on(tx,&job,&operation,None).await?;
            anyhow::ensure!(op.get::<String,_>("resource_type")=="dns_record" && op.get::<String,_>("resource_id")==id
                && op.get::<String,_>("action")==action && op.get::<Value,_>("payload")==payload,"DNS record operation input changed");
            if op.get::<Option<chrono::DateTime<chrono::Utc>>,_>("applied_at").is_some() {
                return Ok(Prepared::Applied(op.get::<Option<Value>,_>("result").context("applied DNS receipt is missing")?));
            }
            let row=sqlx::query("SELECT r.zone_id,r.provider_record_id,z.provider_zone_id FROM dns_records r JOIN dns_zones z ON z.id=r.zone_id WHERE r.id=$1")
                .bind(&id).fetch_one(&mut **tx).await?;
            let source=Source{job,operation,action,record:id,payload,zone_id:row.get("zone_id"),zone:row.get("provider_zone_id"),remote:row.get("provider_record_id"),operation_source:op.get("frozen_source")};
            record_on(tx,&source).await?;
            Ok(Prepared::Ready(source))
        })
    }).await
}

pub(super) async fn commit(pool: &PgPool, source: &Source, result: &Value) -> Result<Value> {
    let (source, result, audit_id) = (source.clone(), result.clone(), Uuid::new_v4().to_string());
    db::write(pool,move |tx| {
        let (s,result,audit_id)=(source.clone(),result.clone(),audit_id.clone());
        Box::pin(async move {
            let op=job_on(tx,&s.job,&s.operation,Some(&s.operation_source)).await?;
            anyhow::ensure!(op.get::<String,_>("resource_type")=="dns_record" && op.get::<String,_>("resource_id")==s.record
                && op.get::<String,_>("action")==s.action && op.get::<Value,_>("payload")==s.payload,"prepared DNS record does not match its operation");
            if op.get::<Option<chrono::DateTime<chrono::Utc>>,_>("applied_at").is_some() {return op.get::<Option<Value>,_>("result").context("applied DNS receipt is missing");}
            record_on(tx,&s).await?;
            if s.action=="record-delete" {
                sqlx::query("UPDATE dns_records SET state='deleted',deleted_at=clock_timestamp(),desired=NULL,revision=revision+1,updated_at=clock_timestamp() WHERE id=$1")
                    .bind(&s.record).execute(&mut **tx).await?;
            } else {
                anyhow::ensure!(editable(&result)==s.payload["desired"],"provider result does not match the requested DNS record");
                let remote_id=result["id"].as_str().context("missing written provider record ID")?;
                anyhow::ensure!(s.remote.as_deref().is_none_or(|id|id==remote_id),"provider result has a different record ID");
                sqlx::query("UPDATE dns_records SET provider_record_id=$1,name=$2,record_type=$3,content=$4,ttl=$5,proxied=$6,remote_snapshot=$7,desired=NULL,state='synced',resolution_status='unchecked',revision=revision+1,updated_at=clock_timestamp() WHERE id=$8")
                    .bind(remote_id).bind(result["name"].as_str().context("missing written name")?).bind(result["type"].as_str().context("missing written type")?)
                    .bind(result["content"].as_str().context("missing written content")?).bind(result["ttl"].as_i64().unwrap_or(1)).bind(result["proxied"].as_bool().unwrap_or(false)).bind(&result).bind(&s.record).execute(&mut **tx).await?;
                super::super::enqueue(tx,super::super::Operation {
                    key:&format!("check:{}",s.operation),request:&json!({"record_id":s.record,"after_operation":s.operation}),
                    connection:s.operation_source["connection_id"].as_str().context("DNS connection missing")?,
                    version:s.job.payload["dns_credential_version"].as_i64().unwrap_or(s.operation_source["credential_version"].as_i64().context("DNS credential version missing")?),
                    resource_type:"dns_record",resource:&s.record,resource_name:result["name"].as_str().context("missing record name")?,
                    resource_key:format!("dns-zone:{}",s.zone_id),action:"record-check",payload:json!({}),node:s.job.node_id.as_deref(),
                }).await?;
            }
            let receipt=json!({"record_id":s.record,"deleted":s.action=="record-delete"});
            sqlx::query("UPDATE dns_operations SET result=$1,applied_at=clock_timestamp() WHERE id=$2 AND applied_at IS NULL")
                .bind(&receipt).bind(&s.operation).execute(&mut **tx).await?;
            sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,'admin',$2,'dns',$3,$4,clock_timestamp())")
                .bind(audit_id).bind(format!("dns.{}",s.action)).bind(&s.record).bind(json!({"operation_id":s.operation})).execute(&mut **tx).await?;
            Ok(receipt)
        })
    }).await
}

#[cfg(test)]
mod tests;
