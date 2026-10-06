use crate::{db, error::ApiError, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;

#[derive(Deserialize)]
pub struct RetryRequest {
    pub expected_revision: i64,
}

pub async fn retry(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<RetryRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let original = sqlx::query("SELECT * FROM jobs WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("job"))?;
    let status: String = original.get("status");
    let parent: Option<String> = original.get("retry_of_job_id");
    if status != "failed"
        && !(matches!(status.as_str(), "cancelled" | "rolled_back") && parent.is_some())
    {
        return Err(ApiError::conflict(
            "only failed jobs or unsuccessful retry attempts can be retried",
        ));
    }
    if let Some(child) = sqlx::query("SELECT id,status FROM jobs WHERE retry_of_job_id=$1")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
    {
        let status: String = child.get("status");
        if matches!(status.as_str(), "queued" | "running") {
            return Ok((
                StatusCode::ACCEPTED,
                Json(json!({"job_id":child.get::<String,_>("id"),"status":status})),
            ));
        }
        return Err(ApiError::conflict(
            "this job already has a completed retry; open the latest attempt",
        ));
    }
    let kind: String = original.get("kind");
    if kind.starts_with("dns-") {
        let mut payload: Value = original.get("payload_json");
        let operation = sqlx::query("SELECT * FROM dns_operations WHERE id=$1")
            .bind(payload["dns_operation_id"].as_str())
            .fetch_one(&mut *tx)
            .await?;
        let resource: String = operation.get("resource_id");
        let resource_type: String = operation.get("resource_type");
        let revision = match resource_type.as_str() {
            "dns_record" => {
                sqlx::query_scalar::<_, i64>("SELECT revision FROM dns_records WHERE id=$1")
                    .bind(&resource)
                    .fetch_one(&mut *tx)
                    .await?
            }
            "dns_zone" => {
                sqlx::query_scalar::<_, i64>("SELECT revision FROM dns_zones WHERE id=$1")
                    .bind(&resource)
                    .fetch_one(&mut *tx)
                    .await?
            }
            "dns_connection" => {
                sqlx::query_scalar::<_, i64>("SELECT revision FROM dns_connections WHERE id=$1")
                    .bind(&resource)
                    .fetch_one(&mut *tx)
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
                    .fetch_one(&mut *tx)
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
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::conflict("verify the DNS connection before retrying"))?;
        payload["dns_credential_version"] = json!(verified_version);
        let node: Option<String> = original.get("node_id");
        let child =
            super::enqueue_job_with_payload_in_tx(&mut tx, &kind, node.as_deref(), None, payload)
                .await?;
        sqlx::query("UPDATE jobs SET retry_of_job_id=$1,resource_key=$2,resource_type=$3,resource_id=$4,resource_name=$5 WHERE id=$6")
            .bind(&id).bind(original.get::<Option<String>,_>("resource_key")).bind(&resource_type).bind(&resource)
            .bind(original.get::<Option<String>,_>("resource_name")).bind(&child).execute(&mut *tx).await?;
        sqlx::query("UPDATE dns_operations SET job_id=$1 WHERE id=$2")
            .bind(&child)
            .bind(operation.get::<String, _>("id"))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
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
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("node"))?;
    let revision: i64 = row.get("desired_revision");
    if revision != input.expected_revision {
        return Err(ApiError::conflict(
            "node revision changed; reload before retrying",
        ));
    }
    if matches!(
        row.get::<String, _>("state").as_str(),
        "deleting" | "delete_failed"
    ) {
        return Err(ApiError::conflict("node is being deleted"));
    }
    if retry_kind == "kick" {
        let changed = sqlx::query("UPDATE kick_requests SET state='active',updated_at=now() WHERE node_id=$1 AND latest_job_id=$2 AND state IN ('waiting_recovery','needs_attention')")
            .bind(&node).bind(&id).execute(&mut *tx).await?;
        if changed.rows_affected() != 1 {
            return Err(ApiError::conflict(
                "kick request is completed, cancelled or superseded; open the latest attempt",
            ));
        }
        crate::kick_requests::schedule(&mut tx, Some(&node), false).await?;
        let child: String = sqlx::query_scalar("SELECT latest_job_id FROM kick_requests WHERE node_id=$1 AND latest_job_id IN (SELECT id FROM jobs WHERE retry_of_job_id=$2)")
            .bind(&node).bind(&id).fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,'admin','job.retry_requested','job',$2,$3,now())")
            .bind(uuid::Uuid::new_v4().to_string()).bind(&id).bind(json!({"job_id":child,"node_id":node})).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({"job_id":child,"status":"queued"})),
        ));
    }
    let target = if retry_kind == "rollback" {
        let target: Option<i64> = original.get("target_revision");
        let target = target.ok_or_else(|| ApiError::conflict("rollback has no target revision"))?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM config_versions WHERE node_id=$1 AND revision=$2 AND deployed_success=TRUE)")
            .bind(&node).bind(target).fetch_one(&mut *tx).await?;
        if !exists {
            return Err(ApiError::conflict("rollback target is no longer available"));
        }
        target
    } else {
        revision
    };
    let payload = if retry_kind == "sync" {
        json!({"force":true})
    } else {
        json!({})
    };
    let child = super::enqueue_job_with_payload_in_tx(
        &mut tx,
        retry_kind,
        Some(&node),
        Some(target),
        payload,
    )
    .await?;
    sqlx::query("UPDATE jobs SET retry_of_job_id=$2 WHERE id=$1")
        .bind(&child)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,'admin','job.retry_requested','job',$2,$3,now())")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&id).bind(json!({"job_id":child,"node_id":node,"target_revision":target})).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"job_id":child,"status":"queued"})),
    ))
}
