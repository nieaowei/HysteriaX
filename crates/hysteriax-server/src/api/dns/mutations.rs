use super::*;

pub(super) async fn create_connection_on(
    tx: &mut Transaction<'_, Postgres>,
    input: ConnectionInput,
    name: String,
    id: String,
    actor: String,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let active:Option<(String,Option<String>,bool)>=sqlx::query_as("SELECT c.kind,c.owner_user_id,c.archived FROM credentials c JOIN credential_versions v ON v.credential_id=c.id WHERE c.id=$1 AND v.version=$2 FOR KEY SHARE OF c,v")
                .bind(&input.credential_id).bind(input.credential_version).fetch_optional(&mut **tx).await?;
    if !matches!(active,Some((ref kind,None,false)) if kind=="dns") {
        return Err(ApiError::conflict("DNS credential changed or was archived"));
    }
    let row=sqlx::query("INSERT INTO dns_connections(id,name,provider,credential_id,credential_version) VALUES($1,$2,'cloudflare',$3,$4) RETURNING *")
                .bind(&id).bind(&name).bind(&input.credential_id).bind(input.credential_version).fetch_one(&mut **tx).await?;
    crate::api::audit_on(
        tx,
        &actor,
        "dns.connection.created",
        "dns",
        &row.get::<String, _>("id"),
        json!({"name":name}),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(dns::connection_json(&row))))
}

pub(super) async fn patch_connection_on(
    tx: &mut Transaction<'_, Postgres>,
    id: String,
    input: ConnectionPatch,
    name: String,
    actor: String,
) -> Result<Json<Value>, ApiError> {
    let row=sqlx::query("UPDATE dns_connections SET name=$1,revision=revision+1,updated_at=now() WHERE id=$2 AND revision=$3 RETURNING *")
                .bind(&name).bind(&id).bind(input.expected_revision).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::conflict("DNS connection changed; reload"))?;
    crate::api::audit_on(
        tx,
        &actor,
        "dns.connection.updated",
        "dns",
        &id,
        json!({"name":name}),
    )
    .await?;
    Ok(Json(dns::connection_json(&row)))
}

pub(super) async fn delete_connection_on(
    tx: &mut Transaction<'_, Postgres>,
    id: String,
    input: RevisionQuery,
    actor: String,
) -> Result<StatusCode, ApiError> {
    sqlx::query("SELECT id FROM dns_connections WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?;
    let referenced:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dns_zones WHERE connection_id=$1) OR EXISTS(SELECT 1 FROM dns_operations WHERE connection_id=$1) OR EXISTS(SELECT 1 FROM dns_credential_batch_items WHERE connection_id=$1)")
                .bind(&id).fetch_one(&mut **tx).await?;
    if referenced {
        return Err(ApiError::conflict(
            "DNS connection has zones or operation history and cannot be deleted",
        ));
    }
    let changed = sqlx::query("DELETE FROM dns_connections WHERE id=$1 AND revision=$2")
        .bind(&id)
        .bind(input.expected_revision)
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            if error.as_database_error().is_some_and(|e| {
                e.code().as_deref() == Some("23503")
                    && e.constraint().is_some_and(|name| {
                        matches!(
                            name,
                            "dns_zones_connection_id_fkey"
                                | "dns_operations_connection_id_fkey"
                                | "dns_credential_batch_items_connection_id_fkey"
                        )
                    })
            }) {
                ApiError::conflict(
                    "DNS connection has zones or operation history and cannot be deleted",
                )
            } else {
                ApiError::from(error)
            }
        })?;
    if changed.rows_affected() != 1 {
        return Err(ApiError::conflict("DNS connection changed; reload"));
    }
    crate::api::audit_on(tx, &actor, "dns.connection.deleted", "dns", &id, json!({})).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn connection_action_on(
    tx: &mut Transaction<'_, Postgres>,
    id: String,
    input: ActionInput,
    action: String,
    request: Value,
) -> Result<Value, ApiError> {
    if let Some(result) = dns::existing_operation(tx, &input.idempotency_key, &request).await? {
        return Ok(result);
    }
    let row = sqlx::query("SELECT * FROM dns_connections WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("DNS connection"))?;
    if row.get::<i64, _>("revision") != input.expected_revision {
        return Err(ApiError::conflict("DNS connection changed; reload"));
    }
    let result = dns::enqueue(
        tx,
        Operation {
            key: &input.idempotency_key,
            request: &request,
            connection: &id,
            version: row.get("credential_version"),
            resource_type: "dns_connection",
            resource: &id,
            resource_name: &row.get::<String, _>("name"),
            resource_key: format!("dns-connection:{id}"),
            action: &action,
            payload: json!({"expected_revision":input.expected_revision}),
            node: None,
        },
    )
    .await?;
    Ok(result)
}

pub(super) async fn patch_zone_on(
    tx: &mut Transaction<'_, Postgres>,
    id: String,
    input: ZonePatch,
    actor: String,
) -> Result<Json<Value>, ApiError> {
    sqlx::query("SELECT c.id FROM dns_connections c JOIN dns_zones z ON z.connection_id=c.id WHERE z.id=$1 FOR KEY SHARE OF c").bind(&id).fetch_optional(&mut **tx).await?;
    sqlx::query("SELECT id FROM dns_zones WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?;
    if !input.enabled {
        let bound: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dns_bindings WHERE zone_id=$1)")
                .bind(&id)
                .fetch_one(&mut **tx)
                .await?;
        if bound {
            return Err(ApiError::conflict("zone still has node bindings"));
        }
    }
    let row=sqlx::query("UPDATE dns_zones SET enabled=$1,revision=revision+1 WHERE id=$2 AND revision=$3 RETURNING *")
                .bind(input.enabled).bind(&id).bind(input.expected_revision).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::conflict("zone changed; reload"))?;
    crate::api::audit_on(
        tx,
        &actor,
        "dns.zone.updated",
        "dns",
        &id,
        json!({"enabled":input.enabled}),
    )
    .await?;
    Ok(Json(dns::zone_json(&row)))
}

pub(super) async fn refresh_zone_on(
    tx: &mut Transaction<'_, Postgres>,
    id: String,
    input: ActionInput,
    request: Value,
) -> Result<Value, ApiError> {
    if let Some(result) = dns::existing_operation(tx, &input.idempotency_key, &request).await? {
        return Ok(result);
    }
    let row = zone_for_write(tx, &id).await?;
    if row.get::<i64, _>("revision") != input.expected_revision {
        return Err(ApiError::conflict("zone changed; reload"));
    }
    let connection: String = row.get("connection_id");
    let result = dns::enqueue(
        tx,
        Operation {
            key: &input.idempotency_key,
            request: &request,
            connection: &connection,
            version: row.get("credential_version"),
            resource_type: "dns_zone",
            resource: &id,
            resource_name: &row.get::<String, _>("name"),
            resource_key: format!("dns-zone:{id}"),
            action: "zone-refresh",
            payload: json!({}),
            node: None,
        },
    )
    .await?;
    Ok(result)
}

pub(super) async fn create_record_on(
    tx: &mut Transaction<'_, Postgres>,
    input: CreateRecord,
    request: Value,
) -> Result<Value, ApiError> {
    if let Some(result) = dns::existing_operation(tx, &input.idempotency_key, &request).await? {
        return Ok(result);
    }
    let zone = zone_for_write(tx, &input.zone_id).await?;
    let record = input
        .record
        .normalize(&zone.get::<String, _>("name"))
        .map_err(bad)?;
    let result =
        create_record_in_tx(tx, &zone, &input.idempotency_key, &request, &record, None).await?;
    Ok(result)
}

pub(super) async fn record_action_on(
    tx: &mut Transaction<'_, Postgres>,
    id: String,
    input: ActionInput,
    record: Option<RecordInput>,
    action: String,
    request: Value,
) -> Result<Value, ApiError> {
    sqlx::query("SELECT n.id FROM nodes n WHERE EXISTS(SELECT 1 FROM dns_bindings b WHERE b.node_id=n.id AND b.record_ids ? $1) OR n.published_connection->>'public_host'=(SELECT name FROM dns_records WHERE id=$1) ORDER BY n.id FOR NO KEY UPDATE OF n").bind(&id).fetch_all(&mut **tx).await?;
    let zone_id: String =
        sqlx::query_scalar("SELECT zone_id FROM dns_records WHERE id=$1 AND deleted_at IS NULL")
            .bind(&id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| ApiError::not_found("DNS record"))?;
    let zone = zone_for_write(tx, &zone_id).await?;
    if let Some(result) = dns::existing_operation(tx, &input.idempotency_key, &request).await? {
        return Ok(result);
    }
    let row =
        sqlx::query("SELECT * FROM dns_records WHERE id=$1 AND deleted_at IS NULL FOR UPDATE")
            .bind(&id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| ApiError::not_found("DNS record"))?;
    if row.get::<i64, _>("revision") != input.expected_revision {
        return Err(ApiError::conflict("DNS record changed; reload"));
    }
    if row.get::<Option<Value>, _>("desired").is_some() && action != "record-check" {
        return Err(ApiError::conflict(
            "DNS record has an unfinished operation; retry that task or refresh after completion",
        ));
    }
    let bound:Option<String>=sqlx::query_scalar("SELECT n.id FROM nodes n WHERE EXISTS(SELECT 1 FROM dns_bindings b WHERE b.node_id=n.id AND b.record_ids ? $1) OR n.published_connection->>'public_host'=$2 LIMIT 1")
                .bind(&id).bind(row.get::<String,_>("name")).fetch_optional(&mut **tx).await?;
    if action == "record-delete" && bound.is_some() {
        return Err(ApiError::conflict(
            "record is used by a binding or published connection; finish the domain switch before deleting it",
        ));
    }
    if !matches!(
        row.get::<String, _>("record_type").as_str(),
        "A" | "AAAA" | "CNAME"
    ) {
        return Err(ApiError::bad_request("this record type is read-only"));
    }
    let desired = if let Some(record) = record {
        let record = record
            .normalize(&zone.get::<String, _>("name"))
            .map_err(bad)?;
        if bound.is_some()
            && (record.name != row.get::<String, _>("name")
                || record.record_type != row.get::<String, _>("record_type")
                || record.proxied)
        {
            return Err(ApiError::conflict(
                "bound records must retain their hostname, type and DNS-only status; reassign the node first",
            ));
        }
        Some(record.remote())
    } else {
        None
    };
    if action != "record-check" {
        sqlx::query("UPDATE dns_records SET desired=$1,state='pending',revision=revision+1,updated_at=now() WHERE id=$2")
                    .bind(desired.clone().unwrap_or_else(||json!({"delete":true}))).bind(&id).execute(&mut **tx).await?;
    }
    let connection: String = zone.get("connection_id");
    let result=dns::enqueue(tx,Operation{key:&input.idempotency_key,request:&request,connection:&connection,version:zone.get("credential_version"),resource_type:"dns_record",resource:&id,resource_name:&row.get::<String,_>("name"),resource_key:format!("dns-zone:{zone_id}"),action:&action,payload:json!({"desired":desired,"baseline":row.get::<Option<Value>,_>("remote_snapshot")}),node:bound.as_deref()}).await?;
    Ok(result)
}

pub(super) async fn set_binding_on(
    tx: &mut Transaction<'_, Postgres>,
    node: String,
    input: SetBinding,
    prepared: std::sync::Arc<crate::dns::binding::Prepared>,
    app: AppState,
    request: Value,
) -> Result<Value, ApiError> {
    if let Some(previous) =
        dns::existing_operation(tx, &input.allocation.idempotency_key, &request).await?
    {
        return Ok(previous);
    }
    // Domain binding, node revision and follow-up sync are one database transaction.
    crate::dns::binding::change_hostname_in_tx(
        &app,
        tx,
        &node,
        input.expected_revision,
        &prepared.hostname,
    )
    .await?;
    let result =
        crate::dns::binding::bind_in_tx(tx, &node, &input.allocation, &prepared, &request).await?;
    crate::dns::binding::queue_sync(tx, &node).await?;
    Ok(result)
}

pub(super) async fn unbind_on(
    tx: &mut Transaction<'_, Postgres>,
    node: String,
    input: Unbind,
    request: Value,
    app: AppState,
    actor: String,
) -> Result<Value, ApiError> {
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(&node)
        .fetch_optional(&mut **tx)
        .await?;
    if let Some(previous) = dns::existing_operation(tx, &input.idempotency_key, &request).await? {
        return Ok(previous);
    }
    let zone_id: String = sqlx::query_scalar("SELECT zone_id FROM dns_bindings WHERE node_id=$1")
        .bind(&node)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("DNS binding"))?;
    lock_zone_on(tx, &zone_id).await?;
    let binding=sqlx::query("SELECT b.*,c.credential_version FROM dns_bindings b JOIN dns_zones z ON z.id=b.zone_id JOIN dns_connections c ON c.id=z.connection_id WHERE node_id=$1")
                .bind(&node).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::not_found("DNS binding"))?;
    if input.public_host.trim() == binding.get::<String, _>("hostname") {
        return Err(ApiError::bad_request(
            "choose a different public address before unbinding",
        ));
    }
    crate::dns::binding::change_hostname_in_tx(
        &app,
        tx,
        &node,
        input.expected_revision,
        input.public_host.trim(),
    )
    .await?;
    sqlx::query("DELETE FROM dns_bindings WHERE node_id=$1")
        .bind(&node)
        .execute(&mut **tx)
        .await?;
    let connection: String = sqlx::query_scalar("SELECT connection_id FROM dns_zones WHERE id=$1")
        .bind(binding.get::<String, _>("zone_id"))
        .fetch_one(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO dns_operations(id,idempotency_key,request_sha256,connection_id,credential_version,resource_type,resource_id,action,payload,result,applied_at) VALUES($1,$2,$3,$4,$5,'node',$6,'unbind',$7,$8,now())")
                .bind(Uuid::new_v4().to_string()).bind(&input.idempotency_key).bind(dns::request_hash(&request)).bind(connection).bind(binding.get::<i64,_>("credential_version")).bind(&node).bind(request).bind(json!({"node_id":node})).execute(&mut **tx).await?;
    crate::dns::binding::queue_sync(tx, &node).await?;
    crate::api::audit_on(
        tx,
        &actor,
        "dns.node.unbound",
        "dns",
        &node,
        json!({"public_host":input.public_host,"records_retained":true}),
    )
    .await?;
    Ok(json!({"node_id":node,"records_retained":true}))
}
