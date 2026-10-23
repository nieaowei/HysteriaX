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
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Path, State},
        http::StatusCode,
        routing::get,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Clone)]
    pub(super) struct Remote {
        pub(super) records: Arc<Mutex<Vec<Value>>>,
        pub(super) writes: Arc<AtomicUsize>,
    }
    async fn list(State(remote): State<Remote>) -> Json<Value> {
        Json(
            json!({"success":true,"result":remote.records.lock().unwrap().clone(),"result_info":{"total_pages":1}}),
        )
    }
    async fn read(
        State(remote): State<Remote>,
        Path(id): Path<String>,
    ) -> (StatusCode, Json<Value>) {
        match remote
            .records
            .lock()
            .unwrap()
            .iter()
            .find(|record| record["id"] == id)
            .cloned()
        {
            Some(record) => (
                StatusCode::OK,
                Json(json!({"success":true,"result":record})),
            ),
            None => (StatusCode::NOT_FOUND, Json(json!({"success":false}))),
        }
    }
    async fn delete(State(remote): State<Remote>, Path(id): Path<String>) -> Json<Value> {
        remote
            .records
            .lock()
            .unwrap()
            .retain(|record| record["id"] != id);
        remote.writes.fetch_add(1, Ordering::SeqCst);
        Json(json!({"success":true,"result":{"id":id}}))
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
    async fn update(
        State(remote): State<Remote>,
        Path(id): Path<String>,
        Json(mut record): Json<Value>,
    ) -> Json<Value> {
        record["id"] = json!(id);
        let mut records = remote.records.lock().unwrap();
        let existing = records.iter_mut().find(|row| row["id"] == id).unwrap();
        *existing = record.clone();
        remote.writes.fetch_add(1, Ordering::SeqCst);
        Json(json!({"success":true,"result":record}))
    }
    pub(super) async fn provider(remote: Remote) -> (Cloudflare, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/zones/remote-zone/dns_records", get(list).post(create))
            .route(
                "/zones/remote-zone/dns_records/{id}",
                get(read).patch(update).delete(delete),
            )
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
    async fn ssh_address_change_reaches_provider_and_replay_does_not_write_twice() {
        let (state, ssh, _, _) = crate::dns::tests::fixture().await;
        let (node, ids) = crate::dns::tests::allocated_node(&state, &ssh).await;
        let old = sqlx::query("SELECT id,remote_snapshot FROM dns_records")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        let remote = Remote {
            records: Arc::new(Mutex::new(
                old.iter()
                    .map(|row| {
                        let mut value: Value = row.get("remote_snapshot");
                        value["id"] = json!(row.get::<String, _>("id"));
                        value
                    })
                    .collect(),
            )),
            writes: Arc::new(AtomicUsize::new(0)),
        };
        let (provider, server) = provider(remote.clone()).await;
        let Json(receipt) = crate::api::nodes::patch(
            State(state.clone()),
            axum::extract::Path(node.clone()),
            Json(
                serde_json::from_value(json!({"expected_revision":1,"ssh_host":"9.9.9.9"}))
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
        let row = sqlx::query("SELECT * FROM jobs WHERE id=$1")
            .bind(receipt["dns_job_ids"][0].as_str())
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let job = JobInput {
            id: row.get("id"),
            kind: row.get("kind"),
            node_id: Some(node),
            target_revision: None,
            payload: row.get("payload_json"),
            attempts: 1,
        };
        sqlx::query("UPDATE jobs SET status='running',attempts=1 WHERE id=$1")
            .bind(&job.id)
            .execute(&state.pool)
            .await
            .unwrap();
        let operation = job.payload["dns_operation_id"].as_str().unwrap();
        let payload: Value = sqlx::query_scalar("SELECT payload FROM dns_operations WHERE id=$1")
            .bind(operation)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        write_record(
            &state.pool,
            &provider,
            &job,
            operation,
            "record-update",
            &ids[0],
            &payload,
        )
        .await
        .unwrap();
        execute(&state.pool, &state.secrets, &job).await.unwrap();
        assert_eq!(remote.writes.load(Ordering::SeqCst), 1);
        assert_eq!(
            remote
                .records
                .lock()
                .unwrap()
                .iter()
                .find(|r| r["id"] == ids[0])
                .unwrap()["content"],
            "9.9.9.9"
        );
        let row = sqlx::query("SELECT content,state,desired FROM dns_records WHERE id=$1")
            .bind(&ids[0])
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("content"), "9.9.9.9");
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
    async fn deleted_node_cleanup_removes_remote_records_and_replays_safely() {
        let (state, ssh, _, _) = crate::dns::tests::fixture().await;
        let (node, ids) = crate::dns::tests::allocated_node(&state, &ssh).await;
        let records = sqlx::query("SELECT id,remote_snapshot FROM dns_records")
            .fetch_all(&state.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                let mut record: Value = row.get("remote_snapshot");
                record["id"] = json!(row.get::<String, _>("id"));
                record
            })
            .collect();
        let remote = Remote {
            records: Arc::new(Mutex::new(records)),
            writes: Arc::new(AtomicUsize::new(0)),
        };
        let (provider, server) = provider(remote.clone()).await;
        let _ = crate::api::nodes::delete(
            State(state.clone()),
            Path(node),
            axum::extract::Query(crate::api::nodes::RevisionQuery {
                expected_revision: Some(1),
            }),
        )
        .await
        .unwrap();
        let rows = sqlx::query("SELECT j.*,o.id AS operation_id,o.resource_id,o.payload FROM jobs j JOIN dns_operations o ON o.job_id=j.id WHERE j.kind='dns-record-delete'").fetch_all(&state.pool).await.unwrap();
        for row in rows {
            let job = JobInput {
                id: row.get("id"),
                kind: row.get("kind"),
                node_id: None,
                target_revision: None,
                payload: row.get("payload_json"),
                attempts: 1,
            };
            sqlx::query("UPDATE jobs SET status='running',attempts=1 WHERE id=$1")
                .bind(&job.id)
                .execute(&state.pool)
                .await
                .unwrap();
            let operation: String = row.get("operation_id");
            let id: String = row.get("resource_id");
            let payload: Value = row.get("payload");
            write_record(
                &state.pool,
                &provider,
                &job,
                &operation,
                "record-delete",
                &id,
                &payload,
            )
            .await
            .unwrap();
            // An acknowledged operation can be replayed without provider access.
            execute(&state.pool, &state.secrets, &job).await.unwrap();
        }
        assert!(remote.records.lock().unwrap().is_empty());
        assert_eq!(remote.writes.load(Ordering::SeqCst), ids.len());
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM dns_records WHERE state='deleted' AND deleted_at IS NOT NULL AND desired IS NULL").fetch_one(&state.pool).await.unwrap(), ids.len() as i64);
        server.abort();
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
        sqlx::query("UPDATE jobs SET status='running',attempts=1 WHERE id=$1")
            .bind(&job.id)
            .execute(&state.pool)
            .await
            .unwrap();
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
        sqlx::query("UPDATE jobs SET status='running',attempts=1 WHERE id=$1")
            .bind(&job.id)
            .execute(&state.pool)
            .await
            .unwrap();
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
        let previous = refresh::tests::start(&state, &receipt).await;
        let error = crate::execution::supervise(
            state.pool.clone(),
            previous.leases.clone(),
            write_record(
                &state.pool,
                &provider,
                &job,
                operation,
                "record-update",
                "record",
                &payload,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.to_string().contains("changed externally"));
        assert_eq!(remote.writes.load(Ordering::SeqCst), 0);
        sqlx::query("UPDATE jobs SET status='failed' WHERE id=$1")
            .bind(&job.id)
            .execute(&state.pool)
            .await
            .unwrap();
        let mut tx = state.pool.begin().await.unwrap();
        for lease in &previous.leases {
            lease.release(&mut tx).await.unwrap();
        }
        tx.commit().await.unwrap();
        refresh::tests::refresh_zone_for_test(&state, &provider, &zone).await;
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
