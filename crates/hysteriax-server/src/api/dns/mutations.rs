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
