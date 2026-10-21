//! Explicit trust resumes the suspended operation without creating a retry job.
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{db, error::ApiError, state::AppState};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confirmation {
    expected_revision: i64,
    fingerprint: String,
}

pub(crate) fn supports_confirmation(kind: &str) -> bool {
    matches!(kind, "ssh-test" | "deploy" | "sync" | "rollback")
}

pub async fn confirm(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Confirmation>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let pool = state.pool.clone();
    let actor = super::audit_actor();
    db::serializable(&pool, move |tx| {
        let (state, id, input, actor) = (state.clone(), id.clone(), input.clone(), actor.clone());
        Box::pin(async move { confirm_on(tx, &state, id, input, actor).await })
    })
    .await
}

async fn confirm_on(
    tx: &mut Transaction<'_, Postgres>,
    state: &AppState,
    id: String,
    input: Confirmation,
    actor: String,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let node_id: Option<String> = sqlx::query_scalar("SELECT node_id FROM jobs WHERE id=$1")
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("job"))?;
    let node_id = node_id.ok_or_else(|| ApiError::not_found("node"))?;
    let node = sqlx::query("SELECT * FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(&node_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("node"))?;
    let job = sqlx::query("SELECT * FROM jobs WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("job"))?;
    let kind: String = job.get("kind");
    if !supports_confirmation(&kind) || job.get::<String, _>("status") != "waiting_confirmation" {
        return Err(ApiError::conflict(
            "job is no longer waiting for fingerprint confirmation",
        ));
    }
    if job.get::<Option<String>, _>("node_id").as_deref() != Some(node_id.as_str()) {
        return Err(ApiError::conflict(
            "job node changed; reload before confirming",
        ));
    }
    let result: Value = job
        .get::<Option<Value>, _>("result_json")
        .unwrap_or_default();
    let observed = &result["result"];
    let delete_recovery = kind == "ssh-test" && observed["delete_recovery"] == true;
    let state_value: String = node.get("state");
    let revision: i64 = node.get("desired_revision");
    let fingerprint: Option<String> = node.get("ssh_host_fingerprint");
    if input.expected_revision < 1
        || revision != input.expected_revision
        || observed["node_revision"].as_i64() != Some(revision)
        || observed["fingerprint"].as_str() != Some(input.fingerprint.as_str())
        || observed["expected_fingerprint"] != json!(fingerprint)
        || !(matches!(
            state_value.as_str(),
            "needs_fingerprint" | "fingerprint_changed"
        ) || (delete_recovery && state_value == "delete_failed"))
        || (kind != "ssh-test" && !observed["expected_fingerprint"].is_null())
    {
        return Err(ApiError::conflict(
            "node or observed fingerprint changed; run a new SSH test",
        ));
    }
    let next_revision = revision + 1;
    sqlx::query(
        "UPDATE nodes SET ssh_host_fingerprint=$1,desired_revision=$2,state=CASE WHEN $4 THEN 'syncing' ELSE state END,updated_at=now() WHERE id=$3",
    )
    .bind(&input.fingerprint)
    .bind(next_revision)
    .bind(&node_id)
    .bind(kind != "ssh-test")
    .execute(&mut **tx)
    .await?;
    // Trust changes the node revision, while the deployment snapshot is unchanged.
    let copied = sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) SELECT $1,node_id,$2,config_enc,content_sha256,now() FROM config_versions WHERE node_id=$3 AND revision=$4")
        .bind(Uuid::new_v4().to_string()).bind(next_revision).bind(&node_id).bind(revision).execute(&mut **tx).await?;
    if copied.rows_affected() != 1 {
        return Err(ApiError::internal());
    }
    crate::credentials::reference_index::flush_node_on(tx, &state.secrets, &node_id).await?;
    let original_target: Option<i64> = job.get("target_revision");
    // Deploy/sync keep the same snapshot content when advancing over this trust-only
    // revision. Rollback must keep its explicitly selected historical revision.
    let target = if kind == "ssh-test"
        || (matches!(kind.as_str(), "deploy" | "sync") && original_target == Some(revision))
    {
        Some(next_revision)
    } else {
        original_target
    };
    let mut payload: Value = job.get("payload_json");
    payload["confirmed_revision"] = json!(next_revision);
    payload["delete_recovery"] = json!(delete_recovery);
    if kind == "ssh-test"
        && let Some(state) = observed["ssh_restore_state"].as_str()
    {
        payload["ssh_restore_state"] = json!(state);
    }
    payload
        .as_object_mut()
        .expect("object payload")
        .entry("original_target_revision")
        .or_insert(json!(original_target));
    sqlx::query("UPDATE jobs SET status='queued',stage='fingerprint_confirmed',target_revision=$1,payload_json=$2,result_json=NULL,available_at=now(),updated_at=now(),finished_at=NULL WHERE id=$3")
        .bind(target).bind(payload).bind(&id).execute(&mut **tx).await?;
    cancel_waiting(tx, &node_id).await?;
    let log = json!({"created_at":super::now(),"stage":"fingerprint_confirmed",
        "attempt":job.get::<i64,_>("attempts"),"message":if kind == "ssh-test" { "SSH host fingerprint trusted; continuing the original test." } else { "SSH host fingerprint trusted; continuing the original operation." }});
    sqlx::query("UPDATE jobs SET logs_json=logs_json || jsonb_build_array($1::jsonb) WHERE id=$2")
        .bind(log)
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    crate::kick_requests::event(tx, &id, "job.fingerprint_confirmed",
        json!({"id":id,"kind":kind,"node_id":node_id,"status":"queued","stage":"fingerprint_confirmed","fingerprint":input.fingerprint})).await?;
    super::audit_on(
        tx,
        &actor,
        "node.ssh_fingerprint_confirmed",
        "node",
        &node_id,
        json!({"job_id":id,"fingerprint":input.fingerprint,"revision":next_revision}),
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"job_id":id,"status":"queued"})),
    ))
}

pub(crate) async fn cancel_waiting(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
) -> Result<(), ApiError> {
    let jobs = sqlx::query("UPDATE jobs SET status='cancelled',stage='superseded',finished_at=now(),updated_at=now() WHERE node_id=$1 AND status='waiting_confirmation' RETURNING id,kind")
        .bind(node).fetch_all(&mut **tx).await?;
    for job in jobs {
        let id: String = job.get("id");
        crate::kick_requests::event(tx, &id, "job.cancelled", json!({"id":id,"kind":job.get::<String,_>("kind"),"node_id":node,"status":"cancelled","stage":"superseded"})).await?;
    }
    Ok(())
}
