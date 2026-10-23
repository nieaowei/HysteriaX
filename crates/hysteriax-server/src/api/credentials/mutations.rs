use super::{AdminActor, PatchCredential, find_references_on, name, record};
use crate::api::enqueue_job_with_payload_in_tx;
use crate::{credentials as vault, error::ApiError, security::SecretBox};
use axum::http::StatusCode;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
type Tx = Transaction<'static, Postgres>;

pub(super) async fn patch_on(
    tx: &mut Tx,
    id: String,
    input: PatchCredential,
    actor: AdminActor,
) -> Result<Value, ApiError> {
    let name = name(&input.name)?.to_owned();
    sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?;
    let managed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credential_versions WHERE credential_id=$1 AND metadata->>'managed'='true' UNION ALL SELECT 1 FROM credentials WHERE id=$1 AND owner_user_id IS NOT NULL)").bind(&id).fetch_one(&mut **tx).await?;
    if managed {
        return Err(ApiError::bad_request(
            "system-issued identities cannot be edited",
        ));
    }
    if input.archived {
        let active_ca: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_node_trust WHERE ca_id=$1 AND (desired OR deployed) UNION ALL SELECT 1 FROM mtls_user_identities WHERE ca_id=$1 AND NOT archived UNION ALL SELECT 1 FROM mtls_rotations WHERE ca_id=$1 AND phase<>'complete' UNION ALL SELECT 1 FROM mtls_live_activation_trust WHERE ca_id=$1)").bind(&id).fetch_one(&mut **tx).await?;
        if active_ca {
            return Err(ApiError::conflict("CA identity is in use"));
        }
    }
    let rows = sqlx::query("UPDATE credentials SET name=$1,archived=$2,reminder_at=$3,revision=revision+1,updated_at=$4 WHERE id=$5 AND revision=$6").bind(name).bind(input.archived).bind(input.reminder_at).bind(Utc::now()).bind(&id).bind(input.expected_revision).execute(&mut **tx).await?;
    if rows.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "credential changed or was deleted; reload before editing",
        ));
    }
    record(
        tx,
        &actor,
        "credential.updated",
        &id,
        json!({"archived":input.archived}),
    )
    .await?;
    Ok(json!({"id":id,"revision":input.expected_revision+1}))
}

pub(super) async fn publish_on(
    tx: &mut Tx,
    id: String,
    expected: i64,
    prepared: super::preparation::Publication,
    actor: AdminActor,
) -> Result<Value, ApiError> {
    vault::reference_index::assert_clean_on(tx).await?;
    // The same node -> credential order used by source-reference writes.
    let nodes=sqlx::query("SELECT n.id,n.desired_revision,CASE WHEN n.ssh_credential_id=$1 THEN 'ssh' ELSE 'config' END mode FROM nodes n WHERE n.state NOT IN ('deleting','delete_failed') AND (n.ssh_credential_id=$1 OR EXISTS(SELECT 1 FROM credential_reference_edges e WHERE e.credential_id=$1 AND e.kind='node' AND e.source_id=n.id AND e.source='desired')) ORDER BY n.id FOR NO KEY UPDATE OF n")
        .bind(&id).fetch_all(&mut **tx).await?;
    let dns_connections =
        sqlx::query("SELECT id FROM dns_connections WHERE credential_id=$1 ORDER BY id FOR UPDATE")
            .bind(&id)
            .fetch_all(&mut **tx)
            .await?;
    let row = sqlx::query("SELECT * FROM credentials WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("credential"))?;
    if row.get::<i64, _>("revision") != expected || row.get::<bool, _>("archived") {
        return Err(ApiError::conflict("credential changed or is archived"));
    }
    let kind: String = row.get("kind");
    if kind == "ca_identity" {
        return Err(ApiError::bad_request(
            "CA versions require managed rotation",
        ));
    }
    let managed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credential_versions WHERE credential_id=$1 AND metadata->>'managed'='true' UNION ALL SELECT 1 FROM credentials WHERE id=$1 AND owner_user_id IS NOT NULL)").bind(&id).fetch_one(&mut **tx).await?;
    if managed {
        return Err(ApiError::bad_request(
            "system-issued identities cannot be edited",
        ));
    }
    if kind != prepared.kind {
        return Err(ApiError::conflict(
            "credential kind changed during preparation",
        ));
    }
    let metadata = prepared.metadata.clone();
    if kind == "dns" {
        let prior: Value = sqlx::query_scalar(
            "SELECT metadata FROM credential_versions WHERE credential_id=$1 AND version=$2",
        )
        .bind(&id)
        .bind(row.get::<i64, _>("latest_version"))
        .fetch_one(&mut **tx)
        .await?;
        if prior["provider"] != metadata["provider"] {
            return Err(ApiError::bad_request(
                "DNS provider cannot change between versions; create a new credential instead",
            ));
        }
    }
    let version = row.get::<i64, _>("latest_version") + 1;
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES($1,$2,$3,$4,$5)").bind(&id).bind(version).bind(&prepared.ciphertext).bind(metadata).bind(prepared.timestamp).execute(&mut **tx).await?;
    sqlx::query(
        "UPDATE credentials SET latest_version=$1,revision=revision+1,updated_at=$2 WHERE id=$3",
    )
    .bind(version)
    .bind(prepared.timestamp)
    .bind(&id)
    .execute(&mut **tx)
    .await?;
    let batch = prepared.batch.clone();
    sqlx::query(
        "INSERT INTO credential_batches(id,credential_id,version,created_at) VALUES($1,$2,$3,$4)",
    )
    .bind(&batch)
    .bind(&id)
    .bind(version)
    .bind(prepared.timestamp)
    .execute(&mut **tx)
    .await?;
    let mut count = 0;
    for node in nodes {
        let node_id: String = node.get("id");
        let revision: i64 = node.get("desired_revision");
        let mode: String = node.get("mode");
        let job = enqueue_job_with_payload_in_tx(
            tx,
            "credential-apply",
            Some(&node_id),
            None,
            json!({"batch_id":batch,"credential_id":id,"version":version,"mode":mode}),
        )
        .await?;
        sqlx::query("INSERT INTO credential_batch_items(batch_id,node_id,expected_revision,job_id) VALUES($1,$2,$3,$4)")
            .bind(&batch).bind(&node_id).bind(revision).bind(job).execute(&mut **tx).await?;
        count += 1;
    }
    for connection in dns_connections {
        crate::dns::rotation::enqueue(tx, &connection.get::<String, _>("id"), version, &batch)
            .await?;
        count += 1;
    }
    record(
        tx,
        &actor,
        "credential.version_published",
        &id,
        json!({"version":version,"batch_id":batch,"affected_count":count}),
    )
    .await?;
    Ok(
        json!({"id":id,"revision":expected+1,"version":version,"batch_id":batch,"affected_count":count}),
    )
}

pub(super) async fn delete_on(
    tx: &mut Tx,
    id: String,
    expected: i64,
    secrets: std::sync::Arc<SecretBox>,
    actor: AdminActor,
) -> Result<StatusCode, ApiError> {
    // Parent first: block new foreign-key pins before inspecting children.
    let revision: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM credentials WHERE id=$1 FOR UPDATE")
            .bind(&id)
            .fetch_optional(&mut **tx)
            .await?;
    if revision != Some(expected) {
        return Err(ApiError::conflict("credential changed or was deleted"));
    }
    if !find_references_on(tx, &secrets, &id).await?.is_empty() {
        return Err(ApiError::conflict(
            "credential is referenced; archive it instead",
        ));
    }
    // Terminal batch metadata can be removed once no business/history or active
    // task references remain; the jobs and audit log still retain their IDs.
    sqlx::query("DELETE FROM mtls_rotations WHERE ca_id=$1 AND phase='complete'")
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM mtls_trust_cleanup WHERE ca_id=$1 AND completed_at IS NOT NULL")
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM credential_batches WHERE credential_id=$1")
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM mtls_ca_generations WHERE ca_id=$1")
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM mtls_authorities WHERE credential_id=$1")
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    let result = sqlx::query("DELETE FROM credentials WHERE id=$1 AND revision=$2")
        .bind(&id)
        .bind(expected)
        .execute(&mut **tx)
        .await?;
    if result.rows_affected() != 1 {
        return Err(ApiError::conflict("credential changed or was deleted"));
    }
    record(tx, &actor, "credential.deleted", &id, json!({})).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn retry_on(
    tx: &mut Tx,
    id: String,
    actor: AdminActor,
) -> Result<Value, ApiError> {
    sqlx::query("SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM credential_batch_items WHERE batch_id=$1 UNION SELECT p.node_id FROM mtls_rotation_nodes p JOIN mtls_rotations r ON r.id=p.rotation_id WHERE r.batch_id=$1) ORDER BY n.id FOR NO KEY UPDATE OF n")
        .bind(&id).fetch_all(&mut **tx).await?;
    sqlx::query("SELECT c.id FROM dns_connections c JOIN dns_credential_batch_items i ON i.connection_id=c.id WHERE i.batch_id=$1 ORDER BY c.id FOR UPDATE OF c")
        .bind(&id).fetch_all(&mut **tx).await?;
    sqlx::query("SELECT c.id FROM credentials c JOIN credential_batches b ON b.credential_id=c.id WHERE b.id=$1 FOR UPDATE OF c")
        .bind(&id).fetch_optional(&mut **tx).await?;
    sqlx::query("SELECT id FROM credential_batches WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?;
    let batch=sqlx::query("SELECT b.*,c.latest_version,c.archived FROM credential_batches b JOIN credentials c ON c.id=b.credential_id WHERE b.id=$1").bind(&id).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::not_found("credential batch"))?;
    if let Some(rotation) =
        sqlx::query("SELECT id,phase,outcome FROM mtls_rotations WHERE batch_id=$1 FOR UPDATE")
            .bind(&id)
            .fetch_optional(&mut **tx)
            .await?
    {
        if batch.get::<bool, _>("archived") {
            return Err(ApiError::conflict("CA identity is archived"));
        }
        if rotation.get::<Option<String>, _>("outcome").is_some() {
            return Err(ApiError::conflict(
                "this CA target was superseded; open the current batch",
            ));
        }
        let rotation_id: String = rotation.get("id");
        sqlx::query("UPDATE mtls_rotations SET retry_requested=TRUE,next_action_at=$2,error_message=NULL WHERE id=$1 AND phase<>'complete'").bind(&rotation_id).bind(Utc::now()).execute(&mut **tx).await?;
        let count=sqlx::query("UPDATE mtls_rotation_nodes p SET job_id=NULL,error_message=NULL FROM jobs j WHERE p.rotation_id=$1 AND p.job_id=j.id AND j.status IN ('failed','rolled_back','cancelled')").bind(&rotation_id).execute(&mut **tx).await?.rows_affected();
        record(tx,&actor,"ca.rotation_retried",&batch.get::<String,_>("credential_id"),json!({"rotation_id":rotation_id,"version":batch.get::<i64,_>("version"),"affected_count":count})).await?;
        return Ok(json!({"batch_id":id,"affected_count":count}));
    }
    if batch.get::<bool, _>("archived")
        || batch.get::<i64, _>("version") != batch.get::<i64, _>("latest_version")
    {
        return Err(ApiError::conflict(
            "cannot retry archived or superseded credential version",
        ));
    }
    let items=sqlx::query("SELECT i.*,j.payload_json FROM credential_batch_items i JOIN jobs j ON j.id=i.job_id WHERE i.batch_id=$1 AND j.status IN ('failed','rolled_back','cancelled') ORDER BY i.node_id,i.user_id FOR UPDATE OF i").bind(&id).fetch_all(&mut **tx).await?;
    let mut count = 0;
    for item in items {
        let node: String = item.get("node_id");
        let user: String = item.get("user_id");
        let credential: String = batch.get("credential_id");
        let current=sqlx::query("SELECT desired_revision,ssh_credential_id,desired_config_enc FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed')").bind(&node).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::conflict("referenced node was deleted"))?;
        if !user.is_empty() {
            return Err(ApiError::conflict(
                "legacy per-node certificate batches cannot be retried",
            ));
        }
        let mode = if current
            .get::<Option<String>, _>("ssh_credential_id")
            .as_deref()
            == Some(&credential)
        {
            "ssh"
        } else {
            "config"
        };
        let revision: i64 = current.get("desired_revision");
        let payload = json!({"batch_id":id,"credential_id":credential,"version":batch.get::<i64,_>("version"),"mode":mode,"user_id":user});
        let job =
            enqueue_job_with_payload_in_tx(tx, "credential-apply", Some(&node), None, payload)
                .await?;
        sqlx::query("UPDATE credential_batch_items SET expected_revision=$1,job_id=$2,applied_at=NULL,apply_stage=NULL WHERE batch_id=$3 AND node_id=$4 AND user_id=$5").bind(revision).bind(job).bind(&id).bind(node).bind(user).execute(&mut **tx).await?;
        count += 1;
    }
    let dns_items=sqlx::query("SELECT i.connection_id FROM dns_credential_batch_items i JOIN jobs j ON j.id=i.job_id WHERE i.batch_id=$1 AND j.status IN ('failed','cancelled') ORDER BY i.connection_id FOR UPDATE OF i").bind(&id).fetch_all(&mut **tx).await?;
    for item in dns_items {
        crate::dns::rotation::enqueue(
            tx,
            &item.get::<String, _>("connection_id"),
            batch.get("version"),
            &id,
        )
        .await?;
        count += 1;
    }
    record(
        tx,
        &actor,
        "credential.batch_retried",
        &batch.get::<String, _>("credential_id"),
        json!({"batch_id":id,"affected_count":count}),
    )
    .await?;
    Ok(json!({"batch_id":id,"affected_count":count}))
}
