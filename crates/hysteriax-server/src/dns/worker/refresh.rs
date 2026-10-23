use super::*;
use crate::execution::Execution;

#[derive(Clone)]
pub(super) struct Source {
    job: JobInput,
    operation: String,
    operation_source: Value,
    resource: String,
    lease: Execution,
    connection: String,
    connection_source: Value,
    zone: Option<(String, Value)>,
}
pub(super) enum Prepared {
    Applied(Value),
    Ready(Source),
}

async fn connection_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
    exclusive: bool,
) -> Result<Value> {
    let statement = format!(
        "SELECT to_jsonb(c)-'updated_at'-'verified_at' FROM dns_connections c WHERE id=$1 FOR {}",
        if exclusive { "NO KEY UPDATE" } else { "SHARE" }
    );
    Ok(sqlx::query_scalar(&statement)
        .bind(id)
        .fetch_one(&mut **tx)
        .await?)
}

pub(super) async fn prepare(
    pool: &PgPool,
    job: &JobInput,
    operation: &str,
    expected: &Value,
) -> Result<Prepared> {
    let (job, operation, expected) = (job.clone(), operation.to_owned(), expected.clone());
    db::write(pool,move |tx| {let (job,operation,expected)=(job.clone(),operation.clone(),expected.clone());Box::pin(async move {
        let op=records::job_on(tx,&job,&operation,Some(&expected)).await?;
        if op.get::<Option<chrono::DateTime<chrono::Utc>>,_>("applied_at").is_some() {return Ok(Prepared::Applied(op.get::<Option<Value>,_>("result").context("DNS refresh receipt missing")?));}
        let action:String=op.get("action");let target:String=op.get("resource_id");let connection:String=op.get("connection_id");
        let resource=if action=="zone-refresh" {
            anyhow::ensure!(op.get::<String,_>("resource_type")=="dns_zone","DNS zone refresh target changed");
            format!("dns-zone:{target}")
        } else {
            anyhow::ensure!(matches!(action.as_str(),"verify"|"connection-refresh") && op.get::<String,_>("resource_type")=="dns_connection" && target==connection,"DNS connection refresh target changed");
            format!("dns-connection:{target}")
        };
        let actual:Option<String>=sqlx::query_scalar("SELECT resource_key FROM jobs WHERE id=$1").bind(&job.id).fetch_one(&mut **tx).await?;
        anyhow::ensure!(actual.as_deref()==Some(resource.as_str()),"DNS refresh resource binding changed");
        let row=sqlx::query("SELECT owner,generation FROM resource_leases WHERE resource_key=$1 AND work_id=$2 AND owner IS NOT NULL AND expires_at>clock_timestamp() AND NOT blocked FOR UPDATE")
            .bind(&resource).bind(&job.id).fetch_optional(&mut **tx).await?.context("DNS refresh requires its resource execution lease")?;
        let lease=Execution{resource:resource.clone(),work:job.id.clone(),owner:row.get("owner"),generation:row.get("generation")};
        let connection_source=connection_on(tx,&connection,action!="zone-refresh").await?;
        let zone=if action=="zone-refresh" {
            let value:Value=sqlx::query_scalar("SELECT to_jsonb(z)-'synced_at' FROM dns_zones z WHERE id=$1 FOR UPDATE")
                .bind(&target).fetch_one(&mut **tx).await?;
            anyhow::ensure!(value["connection_id"]==connection && value["enabled"]==true && connection_source["status"]=="verified","DNS zone or connection is no longer enabled for refresh");
            Some((target,value))
        } else {
            anyhow::ensure!(expected["payload"]["expected_revision"]==connection_source["revision"],"DNS connection changed before verification");
            None
        };
        let version=job.payload["dns_credential_version"].as_i64().unwrap_or(op.get("credential_version"));
        anyhow::ensure!(connection_source["credential_version"].as_i64()==Some(version),"DNS refresh credential binding changed");
        Ok(Prepared::Ready(Source{job,operation,operation_source:expected,resource,lease,connection,connection_source,zone}))
    })}).await
}

async fn current_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source: &Source,
) -> Result<Option<Value>> {
    let op = records::job_on(
        tx,
        &source.job,
        &source.operation,
        Some(&source.operation_source),
    )
    .await?;
    source.lease.guard(tx).await?;
    let actual: Option<String> = sqlx::query_scalar("SELECT resource_key FROM jobs WHERE id=$1")
        .bind(&source.job.id)
        .fetch_one(&mut **tx)
        .await?;
    anyhow::ensure!(
        actual.as_deref() == Some(source.resource.as_str()),
        "DNS refresh resource changed"
    );
    if op
        .get::<Option<chrono::DateTime<chrono::Utc>>, _>("applied_at")
        .is_some()
    {
        return Ok(Some(
            op.get::<Option<Value>, _>("result")
                .context("DNS refresh receipt missing")?,
        ));
    }
    anyhow::ensure!(
        connection_on(tx, &source.connection, source.zone.is_none()).await?
            == source.connection_source,
        "DNS connection changed while refresh was in flight"
    );
    if let Some((id, expected)) = &source.zone {
        let value: Value = sqlx::query_scalar(
            "SELECT to_jsonb(z)-'synced_at' FROM dns_zones z WHERE id=$1 FOR UPDATE",
        )
        .bind(id)
        .fetch_one(&mut **tx)
        .await?;
        anyhow::ensure!(
            value == *expected,
            "DNS zone changed while refresh was in flight"
        );
    }
    Ok(None)
}

async fn complete_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    s: &Source,
    result: &Value,
    audit: &str,
) -> Result<()> {
    sqlx::query("UPDATE dns_operations SET result=$1,applied_at=clock_timestamp() WHERE id=$2 AND job_id=$3 AND applied_at IS NULL")
        .bind(result).bind(&s.operation).bind(&s.job.id).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,'admin',$2,'dns',$3,$4,clock_timestamp())")
        .bind(audit).bind(format!("dns.{}",s.operation_source["action"].as_str().context("DNS refresh action missing")?))
        .bind(s.operation_source["resource_id"].as_str()).bind(json!({"operation_id":s.operation,"job_id":s.job.id})).execute(&mut **tx).await?;
    Ok(())
}

pub(super) async fn connection(
    pool: &PgPool,
    provider: &Cloudflare,
    job: &JobInput,
    operation: &str,
    expected: &Value,
) -> Result<Value> {
    let source = match prepare(pool, job, operation, expected).await? {
        Prepared::Applied(v) => return Ok(v),
        Prepared::Ready(s) => s,
    };
    let zones = provider.zones().await?;
    commit_connection(pool, &source, &zones).await
}
pub(super) async fn commit_connection(
    pool: &PgPool,
    source: &Source,
    zones: &[Value],
) -> Result<Value> {
    anyhow::ensure!(source.zone.is_none(), "connection refresh has a zone scope");
    let mut prepared = std::collections::BTreeMap::new();
    for z in zones {
        let remote = z["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("provider zone ID missing")?
            .to_owned();
        let name =
            super::super::hostname(z["name"].as_str().context("provider zone name missing")?)?;
        if let Some((_, old)) = prepared.get(&remote) {
            anyhow::ensure!(old == &name, "conflicting duplicate provider zone");
        }
        prepared
            .entry(remote)
            .or_insert((Uuid::new_v4().to_string(), name));
    }
    let (source, audit) = (source.clone(), Uuid::new_v4().to_string());
    db::write(pool,move |tx| {let (s,zones,audit)=(source.clone(),prepared.clone(),audit.clone());Box::pin(async move {
        if let Some(receipt)=current_on(tx,&s).await? {return Ok(receipt);}
        for (remote,(id,name)) in &zones {
            sqlx::query("INSERT INTO dns_zones(id,connection_id,provider_zone_id,name,synced_at) VALUES($1,$2,$3,$4,clock_timestamp()) ON CONFLICT(provider_zone_id) DO UPDATE SET name=excluded.name,synced_at=excluded.synced_at,revision=dns_zones.revision+CASE WHEN dns_zones.name IS DISTINCT FROM excluded.name THEN 1 ELSE 0 END WHERE dns_zones.connection_id=excluded.connection_id")
                .bind(id).bind(&s.connection).bind(remote).bind(name).execute(&mut **tx).await?;
        }
        sqlx::query("UPDATE dns_connections SET status='verified',verified_at=clock_timestamp(),updated_at=clock_timestamp() WHERE id=$1").bind(&s.connection).execute(&mut **tx).await?;
        let result=json!({"zones":zones.len(),"connection_id":s.connection});complete_on(tx,&s,&result,&audit).await?;Ok(result)
    })}).await
}

pub(super) async fn zone(
    pool: &PgPool,
    provider: &Cloudflare,
    job: &JobInput,
    operation: &str,
    expected: &Value,
) -> Result<Value> {
    let source = match prepare(pool, job, operation, expected).await? {
        Prepared::Applied(v) => return Ok(v),
        Prepared::Ready(s) => s,
    };
    let (_, z) = source
        .zone
        .as_ref()
        .context("zone refresh has a connection scope")?;
    let records = provider
        .records(
            z["provider_zone_id"]
                .as_str()
                .context("provider zone ID missing")?,
        )
        .await?;
    commit_zone(pool, &source, &records).await
}

pub(super) async fn commit_zone(
    pool: &PgPool,
    source: &Source,
    records: &[Value],
) -> Result<Value> {
    let (zone, _) = source
        .zone
        .as_ref()
        .context("zone refresh has a connection scope")?;
    let mut prepared = std::collections::BTreeMap::new();
    for record in records {
        let id = record["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("provider record ID missing")?
            .to_owned();
        super::super::hostname(
            record["name"]
                .as_str()
                .context("provider record name missing")?,
        )?;
        anyhow::ensure!(
            record["type"].as_str().is_some_and(|t| !t.is_empty()),
            "provider record type missing"
        );
        if let Some((_, old)) = prepared.get(&id) {
            anyhow::ensure!(old == record, "conflicting duplicate provider record");
        }
        prepared
            .entry(id)
            .or_insert((Uuid::new_v4().to_string(), record.clone()));
    }
    let (source, zone, audit) = (source.clone(), zone.clone(), Uuid::new_v4().to_string());
    db::write(pool,move |tx| {let (s,zone,records,audit)=(source.clone(),zone.clone(),prepared.clone(),audit.clone());Box::pin(async move {
        if let Some(receipt)=current_on(tx,&s).await? {return Ok(receipt);}
        sqlx::query("SELECT id FROM dns_records WHERE zone_id=$1 ORDER BY id FOR UPDATE").bind(&zone).fetch_all(&mut **tx).await?;
        let mut seen=vec![];
        for (remote_id,(new_id,remote)) in &records {
            seen.push(remote_id.clone());
            let mut existing=sqlx::query("SELECT id,revision,remote_snapshot,desired,state,deleted_at FROM dns_records WHERE zone_id=$1 AND provider_record_id=$2")
                .bind(&zone).bind(remote_id).fetch_optional(&mut **tx).await?;
            if existing.is_none() && let Some(operation)=remote["comment"].as_str().and_then(|v|v.strip_prefix("HysteriaX operation:")) {
                existing=sqlx::query("SELECT r.id,r.revision,r.remote_snapshot,r.desired,r.state,r.deleted_at FROM dns_records r JOIN dns_operations o ON o.resource_id=r.id WHERE r.zone_id=$1 AND o.id=$2 AND o.action='record-create'")
                    .bind(&zone).bind(operation).fetch_optional(&mut **tx).await?;
            }
            if let Some(row)=&existing && row.get::<Option<Value>,_>("desired").is_some() {
                let id:String=row.get("id");
                let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs WHERE resource_id=$1 AND kind IN ('dns-record-create','dns-record-update','dns-record-delete') AND status IN ('queued','running','waiting_confirmation'))")
                    .bind(&id).fetch_one(&mut **tx).await?;
                if active {continue;}
                let candidates:Vec<(String,String,i64)>=sqlx::query_as("SELECT o.id,j.id,jl.generation FROM dns_operations o JOIN jobs j ON j.id=o.job_id JOIN resource_leases jl ON jl.resource_key='job:'||j.id AND jl.generation>0 AND (jl.work_id IS NULL OR jl.work_id=j.id) WHERE o.resource_id=$1 AND o.applied_at IS NULL AND o.action IN ('record-create','record-update','record-delete') AND o.payload->'source_revision'=to_jsonb($2::bigint) AND $3::jsonb=CASE WHEN o.action='record-delete' THEN '{\"delete\":true}'::jsonb ELSE o.payload->'desired' END AND j.resource_key=$4 AND j.status IN ('failed','succeeded','cancelled') AND j.stage NOT IN ('reconciling','needs_attention') AND NOT EXISTS(SELECT 1 FROM execution_operations e WHERE e.work_id=j.id AND e.state IN ('dispatching','reconciling','needs_attention')) ORDER BY o.id FOR UPDATE OF o")
                    .bind(&id).bind(row.get::<i64,_>("revision")).bind(row.get::<Value,_>("desired")).bind(&s.resource).fetch_all(&mut **tx).await?;
                if candidates.len()!=1 {continue;}
                let (operation,old_job,old_generation)=&candidates[0];
                let evidence=json!({"reconciled_by_refresh":true,"observed":editable(remote),"terminal_job_id":old_job,"terminal_job_generation":old_generation,"source_revision":row.get::<i64,_>("revision"),"refresh_operation_id":s.operation,"refresh_owner":s.lease.owner,"refresh_generation":s.lease.generation});
                sqlx::query("UPDATE dns_operations SET applied_at=clock_timestamp(),result=$1 WHERE id=$2 AND applied_at IS NULL")
                    .bind(evidence).bind(operation).execute(&mut **tx).await?;
            }
            let id=existing.as_ref().map(|r|r.get::<String,_>("id")).unwrap_or_else(||new_id.clone());
            let changed=existing.as_ref().is_some_and(|r|r.get::<Option<Value>,_>("remote_snapshot").as_ref()!=Some(remote)
                || r.get::<Option<Value>,_>("desired").is_some() || r.get::<String,_>("state")!="synced"
                || r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("deleted_at").is_some());
            sqlx::query("INSERT INTO dns_records(id,zone_id,provider_record_id,name,record_type,content,ttl,proxied,remote_snapshot,state) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'synced') ON CONFLICT(id) DO UPDATE SET provider_record_id=excluded.provider_record_id,name=excluded.name,record_type=excluded.record_type,content=excluded.content,ttl=excluded.ttl,proxied=excluded.proxied,remote_snapshot=excluded.remote_snapshot,desired=NULL,state='synced',deleted_at=NULL,revision=dns_records.revision+$10,updated_at=clock_timestamp(),resolution_status=CASE WHEN $10=1 THEN 'unchecked' ELSE dns_records.resolution_status END")
                .bind(id).bind(&zone).bind(remote_id).bind(remote["name"].as_str().context("provider name missing")?).bind(remote["type"].as_str().context("provider type missing")?)
                .bind(remote["content"].as_str().unwrap_or("")).bind(remote["ttl"].as_i64().unwrap_or(1)).bind(remote["proxied"].as_bool().unwrap_or(false)).bind(remote).bind(if changed {1_i64}else{0}).execute(&mut **tx).await?;
        }
        sqlx::query("UPDATE dns_records SET state='remote_missing',deleted_at=clock_timestamp(),revision=revision+1,updated_at=clock_timestamp() WHERE zone_id=$1 AND provider_record_id IS NOT NULL AND NOT(provider_record_id=ANY($2)) AND desired IS NULL AND deleted_at IS NULL")
            .bind(&zone).bind(seen).execute(&mut **tx).await?;
        sqlx::query("UPDATE dns_zones SET synced_at=clock_timestamp() WHERE id=$1").bind(&zone).execute(&mut **tx).await?;
        let result=json!({"zone_id":zone,"records":records.len()});complete_on(tx,&s,&result,&audit).await?;Ok(result)
    })}).await
}

#[cfg(test)]
pub(super) mod tests;
