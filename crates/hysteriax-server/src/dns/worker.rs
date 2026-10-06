use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::{
    audit,
    provider::{Cloudflare, editable},
};
use crate::{
    db,
    deployment::{JobInput, JobOutput},
    security::SecretBox,
};

pub async fn execute(pool: &PgPool, secrets: &SecretBox, job: &JobInput) -> Result<JobOutput> {
    let id = job.payload["dns_operation_id"]
        .as_str()
        .context("missing DNS operation")?;
    let row = sqlx::query("SELECT * FROM dns_operations WHERE id=$1")
        .bind(id)
        .fetch_one(pool)
        .await?;
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
    let provider = super::provider(pool, secrets, &connection, version).await?;
    let action: String = row.get("action");
    let resource: String = row.get("resource_id");
    let payload: Value = row.get("payload");
    let result = match action.as_str() {
        "credential-apply" => {
            super::rotation::apply(pool, &provider, &connection, version, &payload).await?
        }
        "verify" | "connection-refresh" => {
            refresh_connection(pool, &provider, &connection, &payload).await?
        }
        "zone-refresh" => refresh_zone(pool, &provider, &resource).await?,
        "record-create" | "record-update" | "record-delete" => {
            write_record(pool, &provider, job, id, &action, &resource, &payload).await?
        }
        "record-check" => {
            let result = super::probe::check(pool, &resource).await?;
            if result["status"] == "pending" {
                let created: chrono::DateTime<chrono::Utc> = row.get("created_at");
                if chrono::Utc::now() - created < chrono::Duration::minutes(10) {
                    let mut tx = db::begin_write(pool).await?;
                    sqlx::query("UPDATE jobs SET status='queued',stage='dns_propagation_wait',attempts=0,available_at=now()+interval '30 seconds',updated_at=now() WHERE id=$1 AND status='running'")
                        .bind(&job.id).execute(&mut *tx).await?;
                    crate::kick_requests::event(&mut tx,&job.id,"job.retrying",json!({"id":job.id,"kind":job.kind,"status":"queued","stage":"dns_propagation_wait"})).await?;
                    tx.commit().await?;
                    return Ok(output(result));
                }
                bail!("DNS propagation check exceeded ten minutes; check resolution again");
            }
            result
        }
        _ => bail!("unsupported DNS operation"),
    };
    let mut tx = db::begin_write(pool).await?;
    // Completion and audit are committed together; recovered jobs can safely acknowledge again.
    let changed = sqlx::query(
        "UPDATE dns_operations SET result=$1,applied_at=now() WHERE id=$2 AND applied_at IS NULL",
    )
    .bind(&result)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if changed.rows_affected() != 0 {
        audit(
            &mut tx,
            &action,
            &resource,
            json!({"operation_id":id,"job_id":job.id}),
        )
        .await?;
    }
    tx.commit().await?;
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

async fn refresh_connection(
    pool: &PgPool,
    provider: &Cloudflare,
    id: &str,
    payload: &Value,
) -> Result<Value> {
    let zones = provider.zones().await?;
    let mut tx = db::begin_write(pool).await?;
    let current = sqlx::query("SELECT revision FROM dns_connections WHERE id=$1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if payload["expected_revision"].as_i64() != Some(current.get("revision")) {
        bail!("DNS connection changed before verification");
    }
    for zone in &zones {
        let remote_id = zone["id"].as_str().context("missing provider zone ID")?;
        let name = super::hostname(
            zone["name"]
                .as_str()
                .context("missing provider zone name")?,
        )?;
        let owner: Option<String> =
            sqlx::query_scalar("SELECT connection_id FROM dns_zones WHERE provider_zone_id=$1")
                .bind(remote_id)
                .fetch_optional(&mut *tx)
                .await?;
        if owner.as_deref().is_some_and(|owner| owner != id) {
            continue;
        }
        sqlx::query("INSERT INTO dns_zones(id,connection_id,provider_zone_id,name,synced_at) VALUES($1,$2,$3,$4,now()) ON CONFLICT(provider_zone_id) DO UPDATE SET name=excluded.name,synced_at=now()")
            .bind(Uuid::new_v4().to_string()).bind(id).bind(remote_id).bind(name).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE dns_connections SET status='verified',verified_at=now(),updated_at=now() WHERE id=$1")
        .bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(json!({"zones":zones.len(),"connection_id":id}))
}

async fn refresh_zone(pool: &PgPool, provider: &Cloudflare, id: &str) -> Result<Value> {
    let zone = sqlx::query("SELECT provider_zone_id FROM dns_zones WHERE id=$1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    let records = provider
        .records(&zone.get::<String, _>("provider_zone_id"))
        .await?;
    let mut tx = db::begin_write(pool).await?;
    let mut seen = Vec::<String>::new();
    for remote in &records {
        let remote_id = remote["id"]
            .as_str()
            .context("missing provider record ID")?;
        seen.push(remote_id.into());
        let mut existing = sqlx::query("SELECT id,remote_snapshot,desired FROM dns_records WHERE zone_id=$1 AND provider_record_id=$2")
            .bind(id).bind(remote_id).fetch_optional(&mut *tx).await?;
        if existing.is_none()
            && let Some(operation) = remote["comment"]
                .as_str()
                .and_then(|comment| comment.strip_prefix("HysteriaX operation:"))
        {
            existing = sqlx::query("SELECT r.id,r.remote_snapshot,r.desired FROM dns_records r JOIN dns_operations o ON o.resource_id=r.id WHERE r.zone_id=$1 AND o.id=$2 AND o.action='record-create'")
                .bind(id).bind(operation).fetch_optional(&mut *tx).await?;
        }
        if let Some(existing) = &existing
            && existing.get::<Option<Value>, _>("desired").is_some()
        {
            let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs WHERE resource_id=$1 AND kind IN ('dns-record-create','dns-record-update','dns-record-delete') AND status IN ('queued','running'))")
                .bind(existing.get::<String,_>("id")).fetch_one(&mut *tx).await?;
            if active {
                continue;
            }
            sqlx::query("UPDATE dns_operations SET applied_at=now(),result=$1 WHERE resource_id=$2 AND applied_at IS NULL AND action IN ('record-create','record-update','record-delete')")
                .bind(json!({"reconciled_by_refresh":true,"observed":super::provider::editable(remote)}))
                .bind(existing.get::<String,_>("id")).execute(&mut *tx).await?;
        }
        let local_id = existing
            .as_ref()
            .map(|r| r.get::<String, _>("id"))
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let changed = existing
            .as_ref()
            .is_some_and(|r| r.get::<Option<Value>, _>("remote_snapshot").as_ref() != Some(remote));
        sqlx::query("INSERT INTO dns_records(id,zone_id,provider_record_id,name,record_type,content,ttl,proxied,remote_snapshot,state) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,'synced') ON CONFLICT(id) DO UPDATE SET provider_record_id=excluded.provider_record_id,name=excluded.name,record_type=excluded.record_type,content=excluded.content,ttl=excluded.ttl,proxied=excluded.proxied,remote_snapshot=excluded.remote_snapshot,desired=NULL,state='synced',deleted_at=NULL,revision=dns_records.revision+$10,updated_at=now(),resolution_status=CASE WHEN $10=1 THEN 'unchecked' ELSE dns_records.resolution_status END")
            .bind(&local_id).bind(id).bind(remote_id).bind(remote["name"].as_str().context("missing record name")?)
            .bind(remote["type"].as_str().context("missing record type")?).bind(remote["content"].as_str().unwrap_or(""))
            .bind(remote["ttl"].as_i64().unwrap_or(1)).bind(remote["proxied"].as_bool().unwrap_or(false)).bind(remote)
            .bind(if changed {1_i64}else{0_i64}).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE dns_records SET state='remote_missing',deleted_at=now(),revision=revision+1,updated_at=now() WHERE zone_id=$1 AND provider_record_id IS NOT NULL AND NOT(provider_record_id=ANY($2)) AND desired IS NULL AND deleted_at IS NULL")
        .bind(id).bind(&seen).execute(&mut *tx).await?;
    sqlx::query("UPDATE dns_zones SET synced_at=now() WHERE id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(json!({"zone_id":id,"records":records.len()}))
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
    let row = sqlx::query("SELECT r.*,z.provider_zone_id FROM dns_records r JOIN dns_zones z ON z.id=r.zone_id WHERE r.id=$1")
        .bind(id).fetch_one(pool).await?;
    let zone: String = row.get("provider_zone_id");
    let remote_id: Option<String> = row.get("provider_record_id");
    let desired = &payload["desired"];
    let result = if action == "record-create" {
        let records = provider.records(&zone).await?;
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
            provider.create(&zone, desired, operation).await?
        }
    } else {
        let remote_id = remote_id
            .as_deref()
            .context("provider record ID is missing")?;
        let current = provider.record(&zone, remote_id).await?;
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
                    provider.delete(&zone, remote_id).await?;
                    json!({"deleted":true})
                } else {
                    provider.update(&zone, remote_id, desired).await?
                }
            }
        }
    };
    let mut tx = db::begin_write(pool).await?;
    if action == "record-delete" {
        sqlx::query("UPDATE dns_records SET state='deleted',deleted_at=now(),desired=NULL,revision=revision+1,updated_at=now() WHERE id=$1")
            .bind(id).execute(&mut *tx).await?;
    } else {
        sqlx::query("UPDATE dns_records SET provider_record_id=$1,name=$2,record_type=$3,content=$4,ttl=$5,proxied=$6,remote_snapshot=$7,desired=NULL,state='synced',resolution_status='unchecked',revision=revision+1,updated_at=now() WHERE id=$8")
            .bind(result["id"].as_str().context("missing written provider record ID")?).bind(result["name"].as_str().context("missing written name")?)
            .bind(result["type"].as_str().context("missing written type")?).bind(result["content"].as_str().context("missing written content")?)
            .bind(result["ttl"].as_i64().unwrap_or(1)).bind(result["proxied"].as_bool().unwrap_or(false)).bind(&result).bind(id).execute(&mut *tx).await?;
    }
    if action != "record-delete" {
        let operation_row =
            sqlx::query("SELECT connection_id,credential_version FROM dns_operations WHERE id=$1")
                .bind(operation)
                .fetch_one(&mut *tx)
                .await?;
        let connection: String = operation_row.get("connection_id");
        let key = format!("check:{operation}");
        super::enqueue(
            &mut tx,
            super::Operation {
                key: &key,
                request: &json!({"record_id":id,"after_operation":operation}),
                connection: &connection,
                version: job.payload["dns_credential_version"]
                    .as_i64()
                    .unwrap_or_else(|| operation_row.get("credential_version")),
                resource_type: "dns_record",
                resource: id,
                resource_name: result["name"].as_str().context("missing record name")?,
                resource_key: format!("dns-zone:{}", row.get::<String, _>("zone_id")),
                action: "record-check",
                payload: json!({}),
                node: job.node_id.as_deref(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!(e.message))?;
    }
    // Commit the provider result with the record, closing the DB-commit/recovery gap.
    sqlx::query(
        "UPDATE dns_operations SET result=$1,applied_at=now() WHERE id=$2 AND applied_at IS NULL",
    )
    .bind(json!({"record_id":id,"deleted":action=="record-delete"}))
    .bind(operation)
    .execute(&mut *tx)
    .await?;
    audit(&mut tx, action, id, json!({"operation_id":operation})).await?;
    tx.commit().await?;
    Ok(json!({"record_id":id,"deleted":action=="record-delete"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Clone)]
    struct Remote {
        records: Arc<Mutex<Vec<Value>>>,
        writes: Arc<AtomicUsize>,
    }
    async fn list(State(remote): State<Remote>) -> Json<Value> {
        Json(
            json!({"success":true,"result":remote.records.lock().unwrap().clone(),"result_info":{"total_pages":1}}),
        )
    }
    async fn read(State(remote): State<Remote>) -> Json<Value> {
        Json(json!({"success":true,"result":remote.records.lock().unwrap()[0]}))
    }
    async fn create(
        State(remote): State<Remote>,
        Json(mut record): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        record["id"] = json!("remote-created");
        remote.records.lock().unwrap().push(record);
        remote.writes.fetch_add(1, Ordering::SeqCst);
        // Provider commits, but the caller observes an error: recovery must query the marker.
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success":false,"errors":[{"message":"fixture-provider-token"}]})),
        )
    }
    async fn provider(remote: Remote) -> (Cloudflare, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/zones/remote-zone/dns_records", get(list).post(create))
            .route("/zones/remote-zone/dns_records/{id}", get(read))
            .with_state(remote);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider = Cloudflare::with_base(
            "fixture-provider-token".into(),
            format!("http://{}", listener.local_addr().unwrap()),
        )
        .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (provider, server)
    }
    #[tokio::test]
    async fn ambiguous_create_and_repeated_completion_never_duplicate_remote_record() {
        let (state, _, _, zone) = crate::dns::tests::fixture().await;
        let (_, Json(receipt)) = crate::api::dns::create_record(
            State(state.clone()),
            Json(crate::api::dns::CreateRecord {
                zone_id: zone,
                idempotency_key: "ambiguous-create".into(),
                record: super::super::RecordInput {
                    name: "test.example.test".into(),
                    record_type: "A".into(),
                    content: "8.8.8.8".into(),
                    ttl: 1,
                    proxied: false,
                },
            }),
        )
        .await
        .unwrap();
        let job_row = sqlx::query("SELECT * FROM jobs WHERE id=$1")
            .bind(receipt["job_id"].as_str())
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let job = JobInput {
            id: job_row.get("id"),
            kind: job_row.get("kind"),
            node_id: None,
            target_revision: None,
            payload: job_row.get("payload_json"),
            attempts: 1,
        };
        let operation = job.payload["dns_operation_id"].as_str().unwrap();
        let payload: Value = sqlx::query_scalar("SELECT payload FROM dns_operations WHERE id=$1")
            .bind(operation)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let id = receipt["resource_id"].as_str().unwrap();
        let remote = Remote {
            records: Arc::new(Mutex::new(vec![])),
            writes: Arc::new(AtomicUsize::new(0)),
        };
        let (provider, server) = provider(remote.clone()).await;
        let error = write_record(
            &state.pool,
            &provider,
            &job,
            operation,
            "record-create",
            id,
            &payload,
        )
        .await
        .unwrap_err();
        assert!(super::super::provider::retry_delay(&error).is_some());
        assert!(!error.to_string().contains("fixture-provider-token"));
        write_record(
            &state.pool,
            &provider,
            &job,
            operation,
            "record-create",
            id,
            &payload,
        )
        .await
        .unwrap();
        // The actual worker acknowledges the already-applied operation without contacting the provider.
        execute(&state.pool, &state.secrets, &job).await.unwrap();
        assert_eq!(remote.writes.load(Ordering::SeqCst), 1);
        let row = sqlx::query("SELECT * FROM dns_records WHERE id=$1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("state"), "synced");
        assert!(row.get::<Option<Value>, _>("desired").is_none());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind='dns-record-check'")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            1
        );
        server.abort();
    }

    #[tokio::test]
    async fn external_edit_is_not_overwritten_and_refresh_reconciles_terminal_intent() {
        let (state, _, _, zone) = crate::dns::tests::fixture().await;
        let baseline = json!({"id":"remote-existing","type":"A","name":"existing.example.test","content":"8.8.8.8","ttl":300,"proxied":false});
        sqlx::query("INSERT INTO dns_records(id,zone_id,provider_record_id,name,record_type,content,ttl,remote_snapshot,state) VALUES('record',$1,'remote-existing','existing.example.test','A','8.8.8.8',300,$2,'synced')")
            .bind(&zone).bind(&baseline).execute(&state.pool).await.unwrap();
        let (_, Json(receipt)) = crate::api::dns::update_record(
            State(state.clone()),
            axum::extract::Path("record".into()),
            Json(crate::api::dns::UpdateRecord {
                expected_revision: 1,
                idempotency_key: "update-existing".into(),
                record: super::super::RecordInput {
                    name: "existing.example.test".into(),
                    record_type: "A".into(),
                    content: "9.9.9.9".into(),
                    ttl: 300,
                    proxied: false,
                },
            }),
        )
        .await
        .unwrap();
        let job_row = sqlx::query("SELECT * FROM jobs WHERE id=$1")
            .bind(receipt["job_id"].as_str())
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let job = JobInput {
            id: job_row.get("id"),
            kind: job_row.get("kind"),
            node_id: None,
            target_revision: None,
            payload: job_row.get("payload_json"),
            attempts: 1,
        };
        let operation = job.payload["dns_operation_id"].as_str().unwrap();
        let payload: Value = sqlx::query_scalar("SELECT payload FROM dns_operations WHERE id=$1")
            .bind(operation)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let mut external = baseline;
        external["content"] = json!("8.8.4.4");
        let remote = Remote {
            records: Arc::new(Mutex::new(vec![external])),
            writes: Arc::new(AtomicUsize::new(0)),
        };
        let (provider, server) = provider(remote.clone()).await;
        let error = write_record(
            &state.pool,
            &provider,
            &job,
            operation,
            "record-update",
            "record",
            &payload,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("changed externally"));
        assert_eq!(remote.writes.load(Ordering::SeqCst), 0);
        sqlx::query("UPDATE jobs SET status='failed' WHERE id=$1")
            .bind(&job.id)
            .execute(&state.pool)
            .await
            .unwrap();
        refresh_zone(&state.pool, &provider, &zone).await.unwrap();
        let record = sqlx::query("SELECT content,desired FROM dns_records WHERE id='record'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(record.get::<String, _>("content"), "8.8.4.4");
        assert!(record.get::<Option<Value>, _>("desired").is_none());
        let reconciled: bool =
            sqlx::query_scalar("SELECT applied_at IS NOT NULL FROM dns_operations WHERE id=$1")
                .bind(operation)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(reconciled);
        server.abort();
    }
}
