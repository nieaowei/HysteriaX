use anyhow::{Result, bail};
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
pub async fn apply(
    pool: &PgPool,
    provider: &Cloudflare,
    connection: &str,
    version: i64,
    payload: &Value,
) -> Result<Value> {
    let row=sqlx::query("SELECT d.*,c.latest_version,c.archived FROM dns_connections d JOIN credentials c ON c.id=d.credential_id WHERE d.id=$1").bind(connection).fetch_one(pool).await?;
    if row.get::<i64, _>("latest_version") != version || row.get::<bool, _>("archived") {
        bail!("DNS credential publication was superseded or archived");
    }
    provider.zones().await?;
    let zones =
        sqlx::query("SELECT provider_zone_id FROM dns_zones WHERE connection_id=$1 AND enabled")
            .bind(connection)
            .fetch_all(pool)
            .await?;
    for zone in zones {
        let zone: String = zone.get("provider_zone_id");
        provider.zone(&zone).await?;
        provider.records(&zone).await?;
    }
    let mut tx = crate::db::begin_write(pool).await?;
    let latest:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials WHERE id=$1 AND latest_version=$2 AND NOT archived)")
        .bind(row.get::<String,_>("credential_id")).bind(version).fetch_one(&mut *tx).await?;
    if !latest {
        bail!("DNS credential publication was superseded");
    }
    let current =
        sqlx::query("SELECT revision,credential_version FROM dns_connections WHERE id=$1")
            .bind(connection)
            .fetch_one(&mut *tx)
            .await?;
    if current.get::<i64, _>("credential_version") != version {
        if Some(current.get::<i64, _>("revision")) != payload["expected_revision"].as_i64() {
            bail!("DNS connection changed while verifying credentials; retry the latest batch");
        }
        sqlx::query("UPDATE dns_connections SET credential_version=$1,revision=revision+1,status='verified',verified_at=now(),updated_at=now() WHERE id=$2")
            .bind(version).bind(connection).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE dns_credential_batch_items SET applied_at=now() WHERE batch_id=$1 AND connection_id=$2")
        .bind(payload["batch_id"].as_str()).bind(connection).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(json!({"connection_id":connection,"credential_version":version,"read_access_verified":true}))
}
pub async fn batch_items(pool: &PgPool, batch: &str) -> Result<Vec<Value>> {
    let rows=sqlx::query("SELECT i.connection_id,i.job_id,c.name,j.status,j.stage,j.error_message FROM dns_credential_batch_items i JOIN dns_connections c ON c.id=i.connection_id JOIN jobs j ON j.id=i.job_id WHERE i.batch_id=$1 ORDER BY c.name")
        .bind(batch).fetch_all(pool).await?;
    Ok(rows.into_iter().map(|r|json!({"connection_id":r.get::<String,_>("connection_id"),"job_id":r.get::<String,_>("job_id"),"name":r.get::<String,_>("name"),"status":r.get::<String,_>("status"),"stage":r.get::<String,_>("stage"),"error_message":r.get::<Option<String>,_>("error_message")})).collect())
}
