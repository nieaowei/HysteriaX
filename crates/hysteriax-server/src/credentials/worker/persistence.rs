use super::*;
use crate::{
    db,
    execution::{self, Execution},
};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction, postgres::PgRow};
use uuid::Uuid;

#[derive(Clone)]
pub(super) struct Source {
    job: JobInput,
    identity: Value,
    data: Value,
    leases: Vec<Execution>,
}
impl Source {
    pub(super) fn revision(&self) -> i64 {
        self.data["item"]["expected_revision"]
            .as_i64()
            .expect("SQL bigint")
    }
}
pub(super) enum Prepared {
    Applied(JobOutput),
    Ready(Source, PgRow),
}
fn identity(job: &JobInput) -> Value {
    json!({"id":job.id,"kind":job.kind,"node_id":job.node_id,"target_revision":job.target_revision,"payload":job.payload})
}
async fn job_on(tx: &mut Transaction<'_, Postgres>, job: &JobInput) -> Result<()> {
    execution::require_job_scope(&job.id, job.node_id.as_deref())?;
    anyhow::ensure!(
        job.kind == "credential-apply" && job.node_id.is_some(),
        "credential application job required"
    );
    let row=sqlx::query("SELECT kind,node_id,target_revision,payload_json,attempts,resource_key FROM jobs WHERE id=$1 AND status='running' FOR UPDATE")
        .bind(&job.id).fetch_optional(&mut **tx).await?.context("credential job is no longer running")?;
    anyhow::ensure!(
        row.get::<String, _>("kind") == job.kind
            && row.get::<Option<String>, _>("node_id") == job.node_id
            && row.get::<Option<i64>, _>("target_revision") == job.target_revision
            && row.get::<Value, _>("payload_json") == job.payload
            && row.get::<i64, _>("attempts") == job.attempts
            && row.get::<Option<String>, _>("resource_key").is_none(),
        "credential job source changed"
    );
    let keys = vec![
        format!("job:{}", job.id),
        format!("node:{}", job.node_id.as_deref().unwrap()),
    ];
    let unknown:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM execution_operations WHERE (work_id=$1 OR resource_key=ANY($2)) AND state IN ('dispatching','reconciling','needs_attention'))")
        .bind(&job.id).bind(keys).fetch_one(&mut **tx).await?;
    anyhow::ensure!(
        !unknown,
        "credential application has an unverified remote step"
    );
    Ok(())
}
async fn receipt_on(
    tx: &mut Transaction<'_, Postgres>,
    job: &JobInput,
) -> Result<Option<JobOutput>> {
    let row = sqlx::query(
        "SELECT source,stage,result FROM credential_application_receipts WHERE job_id=$1",
    )
    .bind(&job.id)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|r| {
        anyhow::ensure!(
            r.get::<Value, _>("source") == identity(job),
            "credential receipt belongs to another source"
        );
        let mut result: Value = r.get("result");
        result["already_applied"] = json!(true);
        Ok(output(&r.get::<String, _>("stage"), result))
    })
    .transpose()
}
async fn data_on(tx: &mut Transaction<'_, Postgres>, job: &JobInput) -> Result<(Value, PgRow)> {
    let node = job.node_id.as_deref().context("credential node missing")?;
    let credential = job.payload["credential_id"]
        .as_str()
        .context("credential ID missing")?;
    let version = job.payload["version"]
        .as_i64()
        .context("credential version missing")?;
    let batch = job.payload["batch_id"]
        .as_str()
        .context("credential batch missing")?;
    let user = job.payload["user_id"].as_str().unwrap_or("");
    let current=sqlx::query("SELECT n.*,to_jsonb(n)-'updated_at' AS source FROM nodes n WHERE id=$1 AND state NOT IN ('deleting','delete_failed') FOR NO KEY UPDATE")
        .bind(node).fetch_optional(&mut **tx).await?.context("node is missing or being deleted")?;
    let cred: Value =
        sqlx::query_scalar("SELECT to_jsonb(c) FROM credentials c WHERE id=$1 FOR SHARE")
            .bind(credential)
            .fetch_one(&mut **tx)
            .await?;
    anyhow::ensure!(
        cred["latest_version"].as_i64() == Some(version) && cred["archived"] == false,
        "credential update superseded or archived"
    );
    let batch_source: Value =
        sqlx::query_scalar("SELECT to_jsonb(b) FROM credential_batches b WHERE id=$1 FOR SHARE")
            .bind(batch)
            .fetch_one(&mut **tx)
            .await?;
    anyhow::ensure!(
        batch_source["credential_id"] == credential
            && batch_source["version"].as_i64() == Some(version),
        "credential batch source changed"
    );
    let item:Value=sqlx::query_scalar("SELECT to_jsonb(i) FROM credential_batch_items i WHERE batch_id=$1 AND node_id=$2 AND user_id=$3 FOR UPDATE")
        .bind(batch).bind(node).bind(user).fetch_one(&mut **tx).await?;
    anyhow::ensure!(
        item["job_id"] == job.id && item["applied_at"].is_null(),
        "credential batch was replaced or applied without an application receipt"
    );
    anyhow::ensure!(
        current.get::<i64, _>("desired_revision")
            == item["expected_revision"]
                .as_i64()
                .context("credential revision missing")?,
        "node changed before credential application"
    );
    anyhow::ensure!(
        user.is_empty() && matches!(job.payload["mode"].as_str(), Some("ssh" | "config")),
        "unsupported credential application target"
    );
    if job.payload["mode"] == "ssh" {
        anyhow::ensure!(
            current
                .get::<Option<String>, _>("ssh_credential_id")
                .as_deref()
                == Some(credential),
            "node changed its SSH credential binding"
        );
    }
    Ok((
        json!({"node":current.get::<Value,_>("source"),"credential":cred,"batch":batch_source,"item":item}),
        current,
    ))
}
pub(super) async fn prepare(pool: &PgPool, job: &JobInput) -> Result<Prepared> {
    let job = job.clone();
    db::serializable(pool,move |tx|{let job=job.clone();Box::pin(async move {
        job_on(tx,&job).await?;
        if let Some(receipt)=receipt_on(tx,&job).await? {return Ok(Prepared::Applied(receipt));}
        let mut keys=vec![format!("job:{}",job.id),format!("node:{}",job.node_id.as_deref().unwrap())];keys.sort();
        let rows=sqlx::query("SELECT resource_key,owner,epoch,generation FROM resource_leases WHERE resource_key=ANY($1) AND work_id=$2 AND owner IS NOT NULL AND epoch=hysteriax_execution_epoch() AND expires_at>clock_timestamp() AND NOT blocked ORDER BY resource_key FOR UPDATE")
            .bind(&keys).bind(&job.id).fetch_all(&mut **tx).await?;
        anyhow::ensure!(rows.len()==keys.len(),"credential application requires full execution leases");
        let leases:Vec<Execution>=rows.iter().map(|r|Execution{resource:r.get("resource_key"),owner:r.get("owner"),epoch:r.get("epoch"),generation:r.get("generation"),work:job.id.clone()}).collect();
        anyhow::ensure!(leases.iter().all(|l|l.owner==leases[0].owner),"credential execution has mixed owners");
        let (data,current)=data_on(tx,&job).await?;
        Ok(Prepared::Ready(Source{identity:identity(&job),job,data,leases},current))
    })}).await
}
pub(super) async fn commit(
    pool: &PgPool,
    secrets: &SecretBox,
    source: &Source,
    snapshot: &Value,
    config: Option<&Value>,
) -> Result<JobOutput> {
    let plain = snapshot.to_string();
    let (source, secrets, snapshot_id, snapshot_enc, snapshot_sha, desired) = (
        source.clone(),
        secrets.clone(),
        Uuid::new_v4().to_string(),
        secrets.encrypt(&plain)?,
        hex::encode(Sha256::digest(plain.as_bytes())),
        config
            .map(|c| secrets.encrypt(&c.to_string()))
            .transpose()?,
    );
    anyhow::ensure!(
        desired.is_some() == (source.job.payload["mode"] == "config"),
        "credential application candidate mode changed"
    );
    db::serializable(pool,move |tx|{let (s,secrets,snapshot_id,snapshot_enc,snapshot_sha,desired)=(source.clone(),secrets.clone(),snapshot_id.clone(),snapshot_enc.clone(),snapshot_sha.clone(),desired.clone());Box::pin(async move {
        job_on(tx,&s.job).await?;for lease in &s.leases {lease.guard(tx).await?;}
        if let Some(receipt)=receipt_on(tx,&s.job).await? {return Ok(receipt);}
        anyhow::ensure!(data_on(tx,&s.job).await?.0==s.data,"credential application source changed during verification");
        let node=s.job.node_id.as_deref().unwrap();let revision=s.revision();
        let credential=s.job.payload["credential_id"].as_str().unwrap();let version=s.job.payload["version"].as_i64().unwrap();
        let batch=s.job.payload["batch_id"].as_str().unwrap();
        if let Some(cipher)=&desired {
            sqlx::query("UPDATE nodes SET desired_config_enc=$1,desired_revision=$2,updated_at=clock_timestamp() WHERE id=$3").bind(cipher).bind(revision+1).bind(node).execute(&mut **tx).await?;
        } else {
            sqlx::query("UPDATE nodes SET ssh_credential_version=$1,desired_revision=$2,updated_at=clock_timestamp() WHERE id=$3").bind(version).bind(revision+1).bind(node).execute(&mut **tx).await?;
        }
        sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES($1,$2,$3,$4,$5,clock_timestamp())")
            .bind(snapshot_id).bind(node).bind(revision+1).bind(snapshot_enc).bind(snapshot_sha).execute(&mut **tx).await?;
        super::super::reference_index::flush_node_on(tx,&secrets,node).await?;
        let (stage,result,followup)=if desired.is_some() {
            let sync=crate::api::enqueue_job_with_payload_in_tx(tx,"sync",Some(node),Some(revision+1),json!({"credential_batch_id":batch,"credential_id":credential,"credential_version":version})).await?;
            ("credential_deployment_queued",json!({"credential_id":credential,"version":version,"sync_job_id":sync}),sync)
        } else {("credential_applied",json!({"credential_id":credential,"version":version,"node_id":node,"ssh_verified":true}),s.job.id.clone())};
        sqlx::query("UPDATE credential_batch_items SET job_id=$1,applied_at=clock_timestamp(),apply_stage=$2 WHERE batch_id=$3 AND node_id=$4 AND user_id='' AND job_id=$5 AND applied_at IS NULL")
            .bind(&followup).bind(stage).bind(batch).bind(node).bind(&s.job.id).execute(&mut **tx).await?;
        let proof:Vec<Value>=s.leases.iter().map(|l|json!({"resource":l.resource,"owner":l.owner,"generation":l.generation,"work":l.work})).collect();
        sqlx::query("INSERT INTO credential_application_receipts(job_id,source,execution,stage,result) VALUES($1,$2,$3,$4,$5)")
            .bind(&s.job.id).bind(&s.identity).bind(json!({"leases":proof,"node_revision":revision,"batch_id":batch,"source_sha256":hex::encode(Sha256::digest(s.data.to_string().as_bytes()))})).bind(stage).bind(&result).execute(&mut **tx).await?;
        Ok(output(stage,result))
    })}).await
}

#[cfg(test)]
mod tests;
