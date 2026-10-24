use super::*;
use crate::{db, deployment::JobInput, execution::Execution};
use chrono::{DateTime, Utc};
use sqlx::Row;

#[derive(Clone)]
pub(super) struct Record {
    pub id: String,
    pub name: String,
    pub content: String,
    pub kind: RecordType,
    pub zone_name: String,
    pub proxied: bool,
    pub target_proxied: bool,
    zone: String,
    connection: String,
    source: Value,
}

async fn read_on(connection: &mut sqlx::PgConnection, id: &str) -> Result<Record> {
    let row=sqlx::query("SELECT r.*,z.name AS zone_name,z.connection_id,jsonb_build_object('id',r.id,'revision',r.revision,'name',r.name,'record_type',r.record_type,'content',r.content,'proxied',r.proxied,'state',r.state,'desired',r.desired,'deleted_at',r.deleted_at,'zone_id',r.zone_id,'zone_name',z.name,'zone_revision',z.revision,'zone_enabled',z.enabled,'provider_zone_id',z.provider_zone_id,'connection_id',z.connection_id) AS source FROM dns_records r JOIN dns_zones z ON z.id=r.zone_id WHERE r.id=$1")
        .bind(id).fetch_one(&mut *connection).await?;
    anyhow::ensure!(
        row.get::<String, _>("state") == "synced"
            && row.get::<Option<Value>, _>("desired").is_none()
            && row.get::<Option<DateTime<Utc>>, _>("deleted_at").is_none(),
        "DNS record must be written before checking resolution"
    );
    let kind = match row.get::<String, _>("record_type").as_str() {
        "A" => RecordType::A,
        "AAAA" => RecordType::AAAA,
        "CNAME" => RecordType::CNAME,
        _ => bail!("unsupported DNS record check"),
    };
    let content: String = row.get("content");
    let target_proxied = if kind == RecordType::CNAME {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dns_records WHERE name=$1 AND proxied AND deleted_at IS NULL)")
            .bind(content.trim_end_matches('.').to_ascii_lowercase()).fetch_one(&mut *connection).await?
    } else {
        false
    };
    Ok(Record {
        id: id.into(),
        name: row.get("name"),
        content,
        kind,
        zone_name: row.get("zone_name"),
        proxied: row.get("proxied"),
        target_proxied,
        zone: row.get("zone_id"),
        connection: row.get("connection_id"),
        source: row.get("source"),
    })
}

/// Own and finish this short snapshot before starting any DNS I/O.
pub(super) async fn capture(pool: &PgPool, id: &str) -> Result<Record> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let record = read_on(&mut tx, id).await?;
    tx.commit().await?;
    Ok(record)
}
fn same(actual: &Record, expected: &Record) -> Result<()> {
    anyhow::ensure!(
        actual.source == expected.source && actual.target_proxied == expected.target_proxied,
        "DNS probe source or CNAME proxy policy changed"
    );
    Ok(())
}
pub(super) async fn validate(pool: &PgPool, expected: &Record) -> Result<()> {
    same(&capture(pool, &expected.id).await?, expected)
}

#[derive(Clone)]
pub(super) struct Work {
    pub record: Record,
    job: JobInput,
    operation: String,
    expected: Value,
    leases: Vec<Execution>,
    created: DateTime<Utc>,
}
pub(super) enum Prepared {
    Applied(Value),
    Ready(Work),
}

pub(super) async fn prepare_job(
    pool: &PgPool,
    job: &JobInput,
    operation: &str,
    expected: &Value,
) -> Result<Prepared> {
    let (job, operation, expected) = (job.clone(), operation.to_owned(), expected.clone());
    db::write(pool,move |tx| {let (job,operation,expected)=(job.clone(),operation.clone(),expected.clone());Box::pin(async move {
        let op=super::super::execution::job_on(tx,&job,&operation,Some(&expected)).await?;
        anyhow::ensure!(op.get::<String,_>("action")=="record-check" && op.get::<String,_>("resource_type")=="dns_record","DNS probe job has another operation");
        if op.get::<Option<DateTime<Utc>>,_>("applied_at").is_some() {return Ok(Prepared::Applied(op.get::<Option<Value>,_>("result").context("DNS probe receipt missing")?));}
        let record=read_on(tx,&op.get::<String,_>("resource_id")).await?;
        anyhow::ensure!(expected["payload"]["source_revision"]==record.source["revision"],"DNS probe requested record revision changed or has no source proof");
        anyhow::ensure!(record.connection==op.get::<String,_>("connection_id"),"DNS probe connection changed");
        let resource:Option<String>=sqlx::query_scalar("SELECT resource_key FROM jobs WHERE id=$1").bind(&job.id).fetch_one(&mut **tx).await?;
        anyhow::ensure!(resource==Some(format!("dns-zone:{}",record.zone)),"DNS probe zone binding changed");
        let mut keys=vec![format!("job:{}",job.id),resource.context("DNS probe resource missing")?];if let Some(node)=&job.node_id {keys.push(format!("node:{node}"));}keys.sort();keys.dedup();
        let rows=sqlx::query("SELECT resource_key,owner,epoch,generation,work_id FROM resource_leases WHERE resource_key=ANY($1) AND work_id=$2 AND owner IS NOT NULL AND epoch=hysteriax_execution_epoch() AND expires_at>clock_timestamp() AND NOT blocked ORDER BY resource_key FOR UPDATE")
            .bind(&keys).bind(&job.id).fetch_all(&mut **tx).await?;
        anyhow::ensure!(rows.len()==keys.len(),"DNS probe requires its full execution scope");
        let leases:Vec<Execution>=rows.iter().map(|r|Execution{resource:r.get("resource_key"),owner:r.get("owner"),epoch:r.get("epoch"),generation:r.get("generation"),work:r.get("work_id")}).collect();
        anyhow::ensure!(leases.iter().all(|l|l.owner==leases[0].owner),"DNS probe has mixed owners");
        Ok(Prepared::Ready(Work{record,job,operation,expected,leases,created:op.get("created_at")}))
    })}).await
}

async fn validate_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    record: &Record,
) -> Result<()> {
    sqlx::query("SELECT id FROM dns_connections WHERE id=$1 FOR KEY SHARE")
        .bind(&record.connection)
        .fetch_one(&mut **tx)
        .await?;
    sqlx::query("SELECT id FROM dns_zones WHERE id=$1 FOR SHARE")
        .bind(&record.zone)
        .fetch_one(&mut **tx)
        .await?;
    sqlx::query("SELECT id FROM dns_records WHERE id=$1 FOR NO KEY UPDATE")
        .bind(&record.id)
        .fetch_one(&mut **tx)
        .await?;
    same(&read_on(tx, &record.id).await?, record)
}

pub(super) async fn save_job(pool: &PgPool, work: &Work, detail: &Value) -> Result<Value> {
    let expected = work
        .record
        .content
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let status = detail["status"]
        .as_str()
        .context("DNS probe status missing")?;
    anyhow::ensure!(
        detail["record_id"] == work.record.id
            && detail["expected"] == expected
            && matches!(status, "pending" | "verified" | "proxied")
            && (status == "proxied") == (work.record.proxied || work.record.target_proxied),
        "DNS probe observation does not match its source"
    );
    let (work, detail, audit) = (
        work.clone(),
        detail.clone(),
        uuid::Uuid::new_v4().to_string(),
    );
    db::serializable(pool,move |tx| {let (w,detail,audit)=(work.clone(),detail.clone(),audit.clone());Box::pin(async move {
        let op=super::super::execution::job_on(tx,&w.job,&w.operation,Some(&w.expected)).await?;
        for lease in &w.leases {lease.guard(tx).await?;}
        if op.get::<Option<DateTime<Utc>>,_>("applied_at").is_some() {return op.get::<Option<Value>,_>("result").context("DNS probe receipt missing");}
        validate_on(tx,&w.record).await?;
        let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
        let status=detail["status"].as_str().context("DNS probe status missing")?;
        sqlx::query("UPDATE dns_records SET resolution_status=$1,resolution_detail=$2,checked_at=$3 WHERE id=$4")
            .bind(status).bind(&detail).bind(now).bind(&w.record.id).execute(&mut **tx).await?;
        if status=="pending" {
            if now-w.created<chrono::Duration::minutes(10) {
                sqlx::query("UPDATE jobs SET status='queued',stage='dns_propagation_wait',attempts=0,available_at=$2+interval '30 seconds',updated_at=$2 WHERE id=$1")
                    .bind(&w.job.id).bind(now).execute(&mut **tx).await?;
                crate::kick_requests::event(tx,&w.job.id,"job.retrying",json!({"id":w.job.id,"kind":w.job.kind,"status":"queued","stage":"dns_propagation_wait"})).await?;
            } else {
                crate::jobs::fail_on(tx,&w.job,"DNS propagation check exceeded ten minutes; check resolution again",false,false,false,false,None).await?;
            }
            // The queued/failed transition and releasing these exact leases are
            // one commit. The outer executor must not publish this output again.
            for lease in &w.leases {anyhow::ensure!(lease.release(tx).await?,"DNS probe resource became uncertain");}
        } else {
            sqlx::query("UPDATE dns_operations SET result=$1,applied_at=$2 WHERE id=$3 AND job_id=$4 AND applied_at IS NULL")
                .bind(&detail).bind(now).bind(&w.operation).bind(&w.job.id).execute(&mut **tx).await?;
            sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,'admin','dns.record-check','dns',$2,$3,$4)")
                .bind(audit).bind(&w.record.id).bind(json!({"operation_id":w.operation,"job_id":w.job.id})).bind(now).execute(&mut **tx).await?;
        }
        Ok(detail)
    })}).await
}

#[cfg(test)]
mod tests;
