use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    db,
    dns::{self, Operation, RecordInput},
    error::ApiError,
    state::AppState,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionInput {
    pub name: String,
    pub credential_id: String,
    pub credential_version: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionPatch {
    pub expected_revision: i64,
    pub name: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionInput {
    pub expected_revision: i64,
    pub idempotency_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZonePatch {
    pub expected_revision: i64,
    pub enabled: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRecord {
    pub zone_id: String,
    pub idempotency_key: String,
    pub record: RecordInput,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRecord {
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub record: RecordInput,
}
#[derive(Deserialize, Default)]
pub struct RecordQuery {
    pub zone_id: Option<String>,
}
#[derive(Deserialize)]
pub struct RevisionQuery {
    pub expected_revision: i64,
}
fn bad(e: anyhow::Error) -> ApiError {
    ApiError::bad_request(e.to_string())
}
fn valid_name(name: &str) -> Result<&str, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 180 || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "name must contain 1–180 printable characters",
        ));
    }
    Ok(name)
}

pub async fn list_connections(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let rows = sqlx::query("SELECT * FROM dns_connections ORDER BY lower(name),id")
        .fetch_all(&state.pool)
        .await?;
    Ok(Json(rows.iter().map(dns::connection_json).collect()))
}
pub async fn get_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT * FROM dns_connections WHERE id=$1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("DNS connection"))?;
    Ok(Json(dns::connection_json(&row)))
}
pub async fn create_connection(
    State(state): State<AppState>,
    Json(input): Json<ConnectionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let name = valid_name(&input.name)?;
    dns::cloudflare_credential(
        &state.pool,
        &state.secrets,
        &input.credential_id,
        input.credential_version,
        false,
    )
    .await
    .map_err(|_| ApiError::bad_request("select an active Cloudflare DNS credential"))?;
    let mut tx = db::begin_write(&state.pool).await?;
    let active: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials WHERE id=$1 AND NOT archived)")
            .bind(&input.credential_id)
            .fetch_one(&mut *tx)
            .await?;
    if !active {
        return Err(ApiError::conflict("credential was archived"));
    }
    let row=sqlx::query("INSERT INTO dns_connections(id,name,provider,credential_id,credential_version) VALUES($1,$2,'cloudflare',$3,$4) RETURNING *")
        .bind(Uuid::new_v4().to_string()).bind(name).bind(&input.credential_id).bind(input.credential_version).fetch_one(&mut *tx).await?;
    dns::audit(
        &mut tx,
        "connection.created",
        &row.get::<String, _>("id"),
        json!({"name":name}),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(dns::connection_json(&row))))
}
pub async fn patch_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ConnectionPatch>,
) -> Result<Json<Value>, ApiError> {
    let name = valid_name(&input.name)?;
    let mut tx = db::begin_write(&state.pool).await?;
    let row=sqlx::query("UPDATE dns_connections SET name=$1,revision=revision+1,updated_at=now() WHERE id=$2 AND revision=$3 RETURNING *")
        .bind(name).bind(&id).bind(input.expected_revision).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::conflict("DNS connection changed; reload"))?;
    dns::audit(&mut tx, "connection.updated", &id, json!({"name":name})).await?;
    tx.commit().await?;
    Ok(Json(dns::connection_json(&row)))
}
pub async fn delete_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(input): Query<RevisionQuery>,
) -> Result<StatusCode, ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let referenced:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dns_zones WHERE connection_id=$1) OR EXISTS(SELECT 1 FROM dns_operations WHERE connection_id=$1) OR EXISTS(SELECT 1 FROM dns_credential_batch_items WHERE connection_id=$1)")
        .bind(&id).fetch_one(&mut *tx).await?;
    if referenced {
        return Err(ApiError::conflict(
            "DNS connection has zones or operation history and cannot be deleted",
        ));
    }
    let changed = sqlx::query("DELETE FROM dns_connections WHERE id=$1 AND revision=$2")
        .bind(&id)
        .bind(input.expected_revision)
        .execute(&mut *tx)
        .await?;
    if changed.rows_affected() != 1 {
        return Err(ApiError::conflict("DNS connection changed; reload"));
    }
    dns::audit(&mut tx, "connection.deleted", &id, json!({})).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn connection_action(
    state: &AppState,
    id: &str,
    input: ActionInput,
    action: &str,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let request = json!({"connection_id":id,"action":action,"input":input});
    if let Some(result) = dns::existing_operation(&mut tx, &input.idempotency_key, &request).await?
    {
        return Ok((StatusCode::ACCEPTED, Json(result)));
    }
    let row = sqlx::query("SELECT * FROM dns_connections WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("DNS connection"))?;
    if row.get::<i64, _>("revision") != input.expected_revision {
        return Err(ApiError::conflict("DNS connection changed; reload"));
    }
    let result = dns::enqueue(
        &mut tx,
        Operation {
            key: &input.idempotency_key,
            request: &request,
            connection: id,
            version: row.get("credential_version"),
            resource_type: "dns_connection",
            resource: id,
            resource_name: &row.get::<String, _>("name"),
            resource_key: format!("dns-connection:{id}"),
            action,
            payload: json!({"expected_revision":input.expected_revision}),
            node: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
pub async fn verify_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ActionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    connection_action(&state, &id, input, "verify").await
}
pub async fn refresh_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ActionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    connection_action(&state, &id, input, "connection-refresh").await
}

pub async fn list_zones(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let rows = sqlx::query("SELECT * FROM dns_zones ORDER BY name,id")
        .fetch_all(&state.pool)
        .await?;
    Ok(Json(rows.iter().map(dns::zone_json).collect()))
}
pub async fn patch_zone(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ZonePatch>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    if !input.enabled {
        let bound: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dns_bindings WHERE zone_id=$1)")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await?;
        if bound {
            return Err(ApiError::conflict("zone still has node bindings"));
        }
    }
    let row=sqlx::query("UPDATE dns_zones SET enabled=$1,revision=revision+1 WHERE id=$2 AND revision=$3 RETURNING *")
        .bind(input.enabled).bind(&id).bind(input.expected_revision).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::conflict("zone changed; reload"))?;
    dns::audit(
        &mut tx,
        "zone.updated",
        &id,
        json!({"enabled":input.enabled}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(dns::zone_json(&row)))
}
pub async fn refresh_zone(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ActionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let request = json!({"zone_id":id,"action":"zone-refresh","input":input});
    if let Some(result) = dns::existing_operation(&mut tx, &input.idempotency_key, &request).await?
    {
        return Ok((StatusCode::ACCEPTED, Json(result)));
    }
    let row = zone_for_write(&mut tx, &id).await?;
    if row.get::<i64, _>("revision") != input.expected_revision {
        return Err(ApiError::conflict("zone changed; reload"));
    }
    let connection: String = row.get("connection_id");
    let result = dns::enqueue(
        &mut tx,
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
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
pub async fn zone_for_write(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<sqlx::postgres::PgRow, ApiError> {
    let row=sqlx::query("SELECT z.*,c.credential_version,c.status,c.credential_id FROM dns_zones z JOIN dns_connections c ON c.id=z.connection_id JOIN credentials v ON v.id=c.credential_id WHERE z.id=$1 AND NOT v.archived")
        .bind(id).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::bad_request("zone or active DNS credential not found"))?;
    if !row.get::<bool, _>("enabled") || row.get::<String, _>("status") != "verified" {
        return Err(ApiError::bad_request(
            "enable a zone from a verified DNS connection",
        ));
    }
    Ok(row)
}

pub async fn list_records(
    State(state): State<AppState>,
    Query(query): Query<RecordQuery>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let rows=sqlx::query("SELECT r.*,(SELECT n.id FROM nodes n WHERE EXISTS(SELECT 1 FROM dns_bindings b WHERE b.node_id=n.id AND b.record_ids ? r.id) OR n.published_connection->>'public_host'=r.name LIMIT 1) AS bound_node_id FROM dns_records r WHERE ($1::text IS NULL OR r.zone_id=$1) AND (r.deleted_at IS NULL OR EXISTS(SELECT 1 FROM dns_bindings b WHERE b.record_ids ? r.id)) ORDER BY r.name,r.record_type,r.id")
        .bind(query.zone_id).fetch_all(&state.pool).await?;
    Ok(Json(
        rows.iter()
            .map(|r| {
                let mut v = dns::record_json(r);
                v["bound_node_id"] = json!(r.get::<Option<String>, _>("bound_node_id"));
                v
            })
            .collect(),
    ))
}
#[derive(Default, Deserialize)]
pub(crate) struct RecordsPageQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    zone_id: Option<String>,
    connection_id: Option<String>,
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
}

impl RecordsPageQuery {
    fn validate(&self) -> Result<(i64, i64, &str, &str), ApiError> {
        let page = self.page.unwrap_or(1);
        let size = self.page_size.unwrap_or(50);
        if page < 1 || !(1..=200).contains(&size) {
            return Err(ApiError::bad_request(
                "page must be positive and page_size must be between 1 and 200",
            ));
        }
        let sort = match self.sort.as_deref().unwrap_or("name") {
            "name" => "r.name",
            "content" => "r.content",
            _ => return Err(ApiError::bad_request("invalid DNS record sort field")),
        };
        let order = match self.order.as_deref().unwrap_or("asc") {
            "asc" => "ASC",
            "desc" => "DESC",
            _ => return Err(ApiError::bad_request("invalid DNS record sort order")),
        };
        Ok((page, size, sort, order))
    }
}

pub(crate) async fn list_records_page(
    State(state): State<AppState>,
    Query(query): Query<RecordsPageQuery>,
) -> Result<Json<Value>, ApiError> {
    let (requested_page, page_size, sort, order) = query.validate()?;
    let zone = query.zone_id.as_deref().filter(|value| !value.is_empty());
    let connection = query
        .connection_id
        .as_deref()
        .filter(|value| !value.is_empty());
    let search = query.q.as_deref().unwrap_or("").trim();
    let filter = "($1::text IS NULL OR r.zone_id=$1) AND ($2::text IS NULL OR z.connection_id=$2) AND ($3 = '' OR strpos(lower(r.name),lower($3)) > 0 OR strpos(lower(r.content),lower($3)) > 0 OR strpos(lower(r.id),lower($3)) > 0) AND (r.deleted_at IS NULL OR EXISTS(SELECT 1 FROM dns_bindings b WHERE b.record_ids ? r.id))";
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM dns_records r JOIN dns_zones z ON z.id=r.zone_id WHERE {filter}"
    ))
    .bind(zone)
    .bind(connection)
    .bind(search)
    .fetch_one(&mut *tx)
    .await?;
    let page = requested_page.min(((total + page_size - 1) / page_size).max(1));
    let order_by = if sort == "r.name" {
        format!("r.name COLLATE \"C\" {order}, r.record_type COLLATE \"C\" {order}, r.id {order}")
    } else {
        format!(
            "r.content COLLATE \"C\" {order}, r.name COLLATE \"C\" {order}, r.record_type COLLATE \"C\" {order}, r.id {order}"
        )
    };
    // Preserve the complete directory's visibility and node-binding rules.
    let rows = sqlx::query(&format!("SELECT r.*,(SELECT n.id FROM nodes n WHERE EXISTS(SELECT 1 FROM dns_bindings b WHERE b.node_id=n.id AND b.record_ids ? r.id) OR n.published_connection->>'public_host'=r.name LIMIT 1) AS bound_node_id FROM dns_records r JOIN dns_zones z ON z.id=r.zone_id WHERE {filter} ORDER BY {order_by} LIMIT $4 OFFSET $5"))
        .bind(zone).bind(connection).bind(search).bind(page_size).bind((page - 1) * page_size)
        .fetch_all(&mut *tx).await?;
    tx.commit().await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut record = dns::record_json(row);
            record["bound_node_id"] = json!(row.get::<Option<String>, _>("bound_node_id"));
            record
        })
        .collect();
    Ok(Json(
        json!({"items":items,"total":total,"page":page,"page_size":page_size}),
    ))
}

#[cfg(test)]
#[path = "dns_pagination_tests.rs"]
mod pagination_tests;

pub async fn get_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row=sqlx::query("SELECT r.*,(SELECT n.id FROM nodes n WHERE EXISTS(SELECT 1 FROM dns_bindings b WHERE b.node_id=n.id AND b.record_ids ? r.id) OR n.published_connection->>'public_host'=r.name LIMIT 1) AS bound_node_id FROM dns_records r WHERE r.id=$1")
        .bind(id).fetch_optional(&state.pool).await?.ok_or_else(||ApiError::not_found("DNS record"))?;
    let mut v = dns::record_json(&row);
    v["bound_node_id"] = json!(row.get::<Option<String>, _>("bound_node_id"));
    Ok(Json(v))
}
pub async fn create_record(
    State(state): State<AppState>,
    Json(input): Json<CreateRecord>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let request = json!({"action":"record-create","input":input});
    let mut tx = db::begin_write(&state.pool).await?;
    if let Some(result) = dns::existing_operation(&mut tx, &input.idempotency_key, &request).await?
    {
        return Ok((StatusCode::ACCEPTED, Json(result)));
    }
    let zone = zone_for_write(&mut tx, &input.zone_id).await?;
    let record = input
        .record
        .normalize(&zone.get::<String, _>("name"))
        .map_err(bad)?;
    let result = create_record_in_tx(
        &mut tx,
        &zone,
        &input.idempotency_key,
        &request,
        &record,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
pub async fn create_record_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    zone: &sqlx::postgres::PgRow,
    key: &str,
    request: &Value,
    record: &RecordInput,
    node: Option<&str>,
) -> Result<Value, ApiError> {
    let zone_id: String = zone.get("id");
    let conflict:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dns_records WHERE zone_id=$1 AND name=$2 AND deleted_at IS NULL AND (record_type=$3 OR record_type='CNAME' OR $3='CNAME'))")
        .bind(&zone_id).bind(&record.name).bind(&record.record_type).fetch_one(&mut **tx).await?;
    if conflict {
        return Err(ApiError::conflict(
            "record already exists; select the existing record explicitly",
        ));
    }
    let id = Uuid::new_v4().to_string();
    let desired = record.remote();
    sqlx::query("INSERT INTO dns_records(id,zone_id,name,record_type,content,ttl,proxied,origin,desired) VALUES($1,$2,$3,$4,$5,$6,$7,'hysteriax',$8)")
        .bind(&id).bind(&zone_id).bind(&record.name).bind(&record.record_type).bind(&record.content).bind(record.ttl).bind(record.proxied).bind(&desired).execute(&mut **tx).await?;
    let connection: String = zone.get("connection_id");
    dns::enqueue(
        tx,
        Operation {
            key,
            request,
            connection: &connection,
            version: zone.get("credential_version"),
            resource_type: "dns_record",
            resource: &id,
            resource_name: &record.name,
            resource_key: format!("dns-zone:{zone_id}"),
            action: "record-create",
            payload: json!({"desired":desired}),
            node,
        },
    )
    .await
}
async fn record_action(
    state: &AppState,
    id: &str,
    input: ActionInput,
    record: Option<RecordInput>,
    action: &str,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let request = json!({"action":action,"record_id":id,"input":input,"record":record});
    if let Some(result) = dns::existing_operation(&mut tx, &input.idempotency_key, &request).await?
    {
        return Ok((StatusCode::ACCEPTED, Json(result)));
    }
    let row = sqlx::query("SELECT * FROM dns_records WHERE id=$1 AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(&mut *tx)
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
    let zone_id: String = row.get("zone_id");
    let zone = zone_for_write(&mut tx, &zone_id).await?;
    let bound:Option<String>=sqlx::query_scalar("SELECT n.id FROM nodes n WHERE EXISTS(SELECT 1 FROM dns_bindings b WHERE b.node_id=n.id AND b.record_ids ? $1) OR n.published_connection->>'public_host'=$2 LIMIT 1")
        .bind(id).bind(row.get::<String,_>("name")).fetch_optional(&mut *tx).await?;
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
            .bind(desired.clone().unwrap_or_else(||json!({"delete":true}))).bind(id).execute(&mut *tx).await?;
    }
    let connection: String = zone.get("connection_id");
    let result=dns::enqueue(&mut tx,Operation{key:&input.idempotency_key,request:&request,connection:&connection,version:zone.get("credential_version"),resource_type:"dns_record",resource:id,resource_name:&row.get::<String,_>("name"),resource_key:format!("dns-zone:{zone_id}"),action,payload:json!({"desired":desired,"baseline":row.get::<Option<Value>,_>("remote_snapshot")}),node:bound.as_deref()}).await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
pub async fn update_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateRecord>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    record_action(
        &state,
        &id,
        ActionInput {
            expected_revision: input.expected_revision,
            idempotency_key: input.idempotency_key,
        },
        Some(input.record),
        "record-update",
    )
    .await
}
pub async fn delete_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ActionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    record_action(&state, &id, input, None, "record-delete").await
}
pub async fn check_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ActionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    record_action(&state, &id, input, None, "record-check").await
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetBinding {
    pub expected_revision: i64,
    pub allocation: crate::dns::binding::Allocation,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Unbind {
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub public_host: String,
}

pub async fn binding_summary(pool: &sqlx::PgPool, node: &str) -> Result<Value, ApiError> {
    let row=sqlx::query("SELECT b.*,n.published_connection FROM dns_bindings b JOIN nodes n ON n.id=b.node_id WHERE b.node_id=$1").bind(node).fetch_optional(pool).await?;
    let Some(row) = row else {
        return Ok(Value::Null);
    };
    let ids: Value = row.get("record_ids");
    let mut records = Vec::new();
    for id in ids.as_array().ok_or_else(ApiError::internal)? {
        let record = sqlx::query("SELECT * FROM dns_records WHERE id=$1")
            .bind(id.as_str().ok_or_else(ApiError::internal)?)
            .fetch_one(pool)
            .await?;
        records.push(dns::record_json(&record));
    }
    Ok(
        json!({"node_id":node,"zone_id":row.get::<String,_>("zone_id"),"hostname":row.get::<String,_>("hostname"),"record_ids":ids,"records":records,"revision":row.get::<i64,_>("revision"),"published_connection":row.get::<Option<Value>,_>("published_connection")}),
    )
}
pub async fn get_binding(
    State(state): State<AppState>,
    Path(node): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM nodes WHERE id=$1)")
        .bind(&node)
        .fetch_one(&state.pool)
        .await?;
    if !exists {
        return Err(ApiError::not_found("node"));
    }
    Ok(Json(
        json!({"binding":binding_summary(&state.pool, &node).await?}),
    ))
}
pub async fn set_binding(
    State(state): State<AppState>,
    Path(node): Path<String>,
    Json(input): Json<SetBinding>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let request = json!({"node_id":node,"action":"binding-set","input":input});
    let mut tx = db::begin_write(&state.pool).await?;
    let previous =
        dns::existing_operation(&mut tx, &input.allocation.idempotency_key, &request).await?;
    tx.commit().await?;
    if let Some(previous) = previous {
        return Ok((StatusCode::ACCEPTED, Json(previous)));
    }
    let prepared = crate::dns::binding::prepare(&state, &node, &input.allocation).await?;
    let mut tx = db::begin_write(&state.pool).await?;
    if let Some(previous) =
        dns::existing_operation(&mut tx, &input.allocation.idempotency_key, &request).await?
    {
        return Ok((StatusCode::ACCEPTED, Json(previous)));
    }
    // Domain binding, node revision and follow-up sync are one database transaction.
    crate::dns::binding::change_hostname_in_tx(
        &state,
        &mut tx,
        &node,
        input.expected_revision,
        &prepared.hostname,
    )
    .await?;
    let result =
        crate::dns::binding::bind_in_tx(&mut tx, &node, &input.allocation, &prepared, &request)
            .await?;
    crate::dns::binding::queue_sync(&mut tx, &node).await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
pub async fn unbind(
    State(state): State<AppState>,
    Path(node): Path<String>,
    Json(input): Json<Unbind>,
) -> Result<Json<Value>, ApiError> {
    let request = json!({"node_id":node,"action":"binding-remove","input":input});
    if input.public_host.trim().is_empty()
        || input.public_host.len() > 253
        || input.public_host.chars().any(char::is_whitespace)
    {
        return Err(ApiError::bad_request(
            "enter a replacement public IP or hostname",
        ));
    }
    let mut tx = db::begin_write(&state.pool).await?;
    if let Some(previous) =
        dns::existing_operation(&mut tx, &input.idempotency_key, &request).await?
    {
        return Ok(Json(previous));
    }
    let binding=sqlx::query("SELECT b.*,c.credential_version FROM dns_bindings b JOIN dns_zones z ON z.id=b.zone_id JOIN dns_connections c ON c.id=z.connection_id WHERE node_id=$1")
        .bind(&node).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::not_found("DNS binding"))?;
    if input.public_host.trim() == binding.get::<String, _>("hostname") {
        return Err(ApiError::bad_request(
            "choose a different public address before unbinding",
        ));
    }
    crate::dns::binding::change_hostname_in_tx(
        &state,
        &mut tx,
        &node,
        input.expected_revision,
        input.public_host.trim(),
    )
    .await?;
    sqlx::query("DELETE FROM dns_bindings WHERE node_id=$1")
        .bind(&node)
        .execute(&mut *tx)
        .await?;
    let connection: String = sqlx::query_scalar("SELECT connection_id FROM dns_zones WHERE id=$1")
        .bind(binding.get::<String, _>("zone_id"))
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO dns_operations(id,idempotency_key,request_sha256,connection_id,credential_version,resource_type,resource_id,action,payload,result,applied_at) VALUES($1,$2,$3,$4,$5,'node',$6,'unbind',$7,$8,now())")
        .bind(Uuid::new_v4().to_string()).bind(&input.idempotency_key).bind(dns::request_hash(&request)).bind(connection).bind(binding.get::<i64,_>("credential_version")).bind(&node).bind(request).bind(json!({"node_id":node})).execute(&mut *tx).await?;
    crate::dns::binding::queue_sync(&mut tx, &node).await?;
    dns::audit(
        &mut tx,
        "node.unbound",
        &node,
        json!({"public_host":input.public_host,"records_retained":true}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"node_id":node,"records_retained":true})))
}
