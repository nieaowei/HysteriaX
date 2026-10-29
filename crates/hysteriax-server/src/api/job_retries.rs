use crate::{error::ApiError, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;

pub(crate) mod v2;

#[derive(Clone, Deserialize)]
pub struct RetryRequest {
    pub expected_revision: i64,
}

async fn retry_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: String,
    input: RetryRequest,
    actor: String,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    lock_retry_parents_on(tx, &id).await?;
    let original = sqlx::query("SELECT * FROM jobs WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("job"))?;
    let status: String = original.get("status");
    let parent: Option<String> = original.get("retry_of_job_id");
    let managed = original.get::<Value, _>("payload_json")["mtls_operation"] == true;
    if status != "failed"
        && !(matches!(status.as_str(), "cancelled" | "rolled_back")
            && (parent.is_some() || managed))
    {
        return Err(ApiError::conflict(
            "only failed jobs or unsuccessful retry attempts can be retried",
        ));
    }
    if let Some(child) = sqlx::query("SELECT id,status FROM jobs WHERE retry_of_job_id=$1")
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?
    {
        let status: String = child.get("status");
        if matches!(
            status.as_str(),
            "queued" | "running" | "waiting_confirmation"
        ) {
            return Ok((
                StatusCode::ACCEPTED,
                Json(json!({"job_id":child.get::<String,_>("id"),"status":status})),
            ));
        }
        return Err(ApiError::conflict(
            "this job already has a completed retry; open the latest attempt",
        ));
    }
    let uncertain:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM execution_operations WHERE work_id=$1 AND state IN ('dispatching','reconciling','needs_attention') UNION ALL SELECT 1 FROM resource_leases WHERE work_id=$1 AND blocked)").bind(&id).fetch_one(&mut **tx).await?;
    if uncertain {
        return Err(ApiError::conflict(
            "remote outcome requires verification before retrying this job",
        ));
    }
    // Publish a new parent tuple with the retry. A waiter with an older
    // SERIALIZABLE snapshot must retry before it can infer there is no child.
    sqlx::query("UPDATE jobs SET updated_at=clock_timestamp() WHERE id=$1")
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    if managed {
        let child = crate::credentials::rotations::retry_node_job(tx, &id, input.expected_revision)
            .await
            .map_err(|error| {
                if crate::db::RetryableTransactionError::retryable_transaction(&error) {
                    ApiError::from(error)
                } else {
                    ApiError::conflict(error.to_string())
                }
            })?;
        audit_retry_on(tx, &actor, &id, &child, json!({})).await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({"job_id":child,"status":"queued"})),
        ));
    }
    let kind: String = original.get("kind");
    if kind == "authorization-change" {
        let child = super::enqueue_job_with_payload_in_tx(
            tx,
            &kind,
            None,
            None,
            original.get("payload_json"),
        )
        .await?;
        sqlx::query("UPDATE jobs SET retry_of_job_id=$1,resource_key=$2,resource_type=$3,resource_id=$4,resource_name=$5 WHERE id=$6")
            .bind(&id).bind(original.get::<Option<String>,_>("resource_key")).bind(original.get::<Option<String>,_>("resource_type"))
            .bind(original.get::<Option<String>,_>("resource_id")).bind(original.get::<Option<String>,_>("resource_name")).bind(&child).execute(&mut **tx).await?;
        audit_retry_on(tx, &actor, &id, &child, json!({})).await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({"job_id":child,"status":"queued"})),
        ));
    }
    if kind.starts_with("dns-") {
        let mut payload: Value = original.get("payload_json");
        let operation = sqlx::query("SELECT * FROM dns_operations WHERE id=$1")
            .bind(payload["dns_operation_id"].as_str())
            .fetch_one(&mut **tx)
            .await?;
        let resource: String = operation.get("resource_id");
        let resource_type: String = operation.get("resource_type");
        let revision = match resource_type.as_str() {
            "dns_record" => {
                sqlx::query_scalar::<_, i64>("SELECT revision FROM dns_records WHERE id=$1")
                    .bind(&resource)
                    .fetch_one(&mut **tx)
                    .await?
            }
            "dns_zone" => {
                sqlx::query_scalar::<_, i64>("SELECT revision FROM dns_zones WHERE id=$1")
                    .bind(&resource)
                    .fetch_one(&mut **tx)
                    .await?
            }
            "dns_connection" => {
                sqlx::query_scalar::<_, i64>("SELECT revision FROM dns_connections WHERE id=$1")
                    .bind(&resource)
                    .fetch_one(&mut **tx)
                    .await?
            }
            _ => return Err(ApiError::bad_request("unsupported DNS retry target")),
        };
        if revision != input.expected_revision {
            return Err(ApiError::conflict(
                "DNS resource changed; reload before retrying",
            ));
        }
        if kind == "dns-credential-apply" {
            return Err(ApiError::bad_request(
                "retry DNS credential updates from the latest credential batch",
            ));
        }
        if matches!(
            kind.as_str(),
            "dns-record-create" | "dns-record-update" | "dns-record-delete"
        ) && operation
            .get::<Option<chrono::DateTime<chrono::Utc>>, _>("applied_at")
            .is_none()
        {
            let desired: Option<Value> =
                sqlx::query_scalar("SELECT desired FROM dns_records WHERE id=$1")
                    .bind(&resource)
                    .fetch_one(&mut **tx)
                    .await?;
            if desired.is_none() {
                return Err(ApiError::conflict(
                    "DNS operation was superseded by refresh or a newer edit",
                ));
            }
        }
        // Each new retry pins the connection version verified at its own enqueue
        // time; the original operation keeps its historical credential version.
        let verified_version: i64 = sqlx::query_scalar(
            "SELECT credential_version FROM dns_connections WHERE id=$1 AND status='verified'",
        )
        .bind(operation.get::<String, _>("connection_id"))
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::conflict("verify the DNS connection before retrying"))?;
        payload["dns_credential_version"] = json!(verified_version);
        let node: Option<String> = original.get("node_id");
        let child =
            super::enqueue_job_with_payload_in_tx(tx, &kind, node.as_deref(), None, payload)
                .await?;
        sqlx::query("UPDATE jobs SET retry_of_job_id=$1,resource_key=$2,resource_type=$3,resource_id=$4,resource_name=$5 WHERE id=$6")
            .bind(&id).bind(original.get::<Option<String>,_>("resource_key")).bind(&resource_type).bind(&resource)
            .bind(original.get::<Option<String>,_>("resource_name")).bind(&child).execute(&mut **tx).await?;
        sqlx::query("UPDATE dns_operations SET job_id=$1 WHERE id=$2")
            .bind(&child)
            .bind(operation.get::<String, _>("id"))
            .execute(&mut **tx)
            .await?;
        audit_retry_on(tx, &actor, &id, &child, json!({})).await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({"job_id":child,"status":"queued"})),
        ));
    }
    let retry_kind = match kind.as_str() {
        "ssh-test" => "ssh-test",
        "deploy" | "sync" => "sync",
        "rollback" => "rollback",
        "kick" => "kick",
        _ => {
            return Err(ApiError::bad_request(
                "this job kind does not support retry",
            ));
        }
    };
    let node: Option<String> = original.get("node_id");
    let node = node.ok_or_else(|| ApiError::conflict("job has no node to retry"))?;
    let row = sqlx::query("SELECT desired_revision,state FROM nodes WHERE id=$1")
        .bind(&node)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("node"))?;
    let revision: i64 = row.get("desired_revision");
    if revision != input.expected_revision {
        return Err(ApiError::conflict(
            "node revision changed; reload before retrying",
        ));
    }
    let state_value: String = row.get("state");
    if !super::nodes::action_allowed(&state_value, retry_kind) {
        return Err(ApiError::conflict("node is being deleted"));
    }
    if retry_kind == "kick" {
        let changed = sqlx::query("UPDATE kick_requests SET state='active',updated_at=now() WHERE node_id=$1 AND latest_job_id=$2 AND state IN ('active','waiting_recovery','needs_attention')")
            .bind(&node).bind(&id).execute(&mut **tx).await?;
        if changed.rows_affected() == 0 {
            return Err(ApiError::conflict(
                "kick request is completed, cancelled or superseded; open the latest attempt",
            ));
        }
        let child = crate::kick_requests::ensure_job(tx, &node, Some(&id)).await?;
        audit_retry_on(tx, &actor, &id, &child, json!({"node_id":node})).await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({"job_id":child,"status":"queued"})),
        ));
    }
    let target = if retry_kind == "rollback" {
        let target: Option<i64> = original.get("target_revision");
        let target = target.ok_or_else(|| ApiError::conflict("rollback has no target revision"))?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM config_versions WHERE node_id=$1 AND revision=$2 AND deployed_success=TRUE)")
            .bind(&node).bind(target).fetch_one(&mut **tx).await?;
        if !exists {
            return Err(ApiError::conflict("rollback target is no longer available"));
        }
        target
    } else {
        revision
    };
    let mut payload = if retry_kind == "sync" {
        json!({"force":true})
    } else {
        json!({})
    };
    if retry_kind == "ssh-test" && state_value == "delete_failed" {
        payload["delete_recovery"] = json!(true);
    }
    let child =
        super::enqueue_job_with_payload_in_tx(tx, retry_kind, Some(&node), Some(target), payload)
            .await?;
    sqlx::query("UPDATE jobs SET retry_of_job_id=$2 WHERE id=$1")
        .bind(&child)
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    audit_retry_on(
        tx,
        &actor,
        &id,
        &child,
        json!({"node_id":node,"target_revision":target}),
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"job_id":child,"status":"queued"})),
    ))
}

async fn audit_retry_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &str,
    parent: &str,
    child: &str,
    mut detail: Value,
) -> Result<(), ApiError> {
    detail["job_id"] = json!(child);
    super::audit_on(tx, actor, "job.retry_requested", "job", parent, detail).await
}

async fn lock_retry_parents_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
) -> Result<(), ApiError> {
    let original = sqlx::query("SELECT node_id,kind,payload_json FROM jobs WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("job"))?;
    if let Some(node) = original.get::<Option<String>, _>("node_id") {
        sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
            .bind(node)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| ApiError::not_found("node"))?;
    }
    let payload: Value = original.get("payload_json");
    if let Some(operation) = payload["dns_operation_id"].as_str() {
        let operation=sqlx::query("SELECT o.connection_id,CASE WHEN o.resource_type='dns_zone' THEN o.resource_id WHEN o.resource_type='dns_record' THEN (SELECT r.zone_id FROM dns_records r WHERE r.id=o.resource_id) END AS zone_id,o.resource_type,o.resource_id FROM dns_operations o WHERE o.id=$1").bind(operation).fetch_optional(&mut **tx).await?.ok_or_else(|| ApiError::not_found("DNS operation"))?;
        sqlx::query("SELECT id FROM dns_connections WHERE id=$1 FOR NO KEY UPDATE")
            .bind(operation.get::<String, _>("connection_id"))
            .fetch_optional(&mut **tx)
            .await?;
        if let Some(zone) = operation.get::<Option<String>, _>("zone_id") {
            sqlx::query("SELECT id FROM dns_zones WHERE id=$1 FOR SHARE")
                .bind(zone)
                .fetch_optional(&mut **tx)
                .await?;
        }
        if operation.get::<String, _>("resource_type") == "dns_record" {
            sqlx::query("SELECT id FROM dns_records WHERE id=$1 FOR UPDATE")
                .bind(operation.get::<String, _>("resource_id"))
                .fetch_optional(&mut **tx)
                .await?;
        }
    }
    if payload["mtls_operation"] == true {
        let operation =
            sqlx::query("SELECT ca_id,rotation_id FROM mtls_node_operations WHERE job_id=$1")
                .bind(id)
                .fetch_one(&mut **tx)
                .await?;
        let ca: String = operation.get("ca_id");
        sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR NO KEY UPDATE")
            .bind(&ca)
            .fetch_optional(&mut **tx)
            .await?;
        sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE")
            .bind(ca)
            .fetch_optional(&mut **tx)
            .await?;
        if let Some(rotation) = operation.get::<Option<String>, _>("rotation_id") {
            sqlx::query("SELECT id FROM mtls_rotations WHERE id=$1 FOR UPDATE")
                .bind(rotation)
                .fetch_optional(&mut **tx)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod transaction_tests;

// Direct domain fixtures are test-only; production writes use protocol-2 handlers.
#[cfg(test)]
#[path = "job_retries_test_support.rs"]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::*;

#[cfg(test)]
use crate::db;
