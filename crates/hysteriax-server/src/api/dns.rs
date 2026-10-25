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
    dns::{self, Operation, RecordInput},
    error::ApiError,
    state::AppState,
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionInput {
    pub name: String,
    pub credential_id: String,
    pub credential_version: i64,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionPatch {
    pub expected_revision: i64,
    pub name: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActionInput {
    pub expected_revision: i64,
    pub idempotency_key: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZonePatch {
    pub expected_revision: i64,
    pub enabled: bool,
}
#[derive(Clone, Deserialize, Serialize)]
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
#[derive(Clone, Deserialize)]
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
        .fetch_all(&mut *crate::read_snapshot::begin(&state.pool).await?)
        .await?;
    Ok(Json(rows.iter().map(dns::connection_json).collect()))
}
pub async fn get_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT * FROM dns_connections WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut *crate::read_snapshot::begin(&state.pool).await?)
        .await?
        .ok_or_else(|| ApiError::not_found("DNS connection"))?;
    Ok(Json(dns::connection_json(&row)))
}
async fn prepare_connection(state: &AppState, input: &ConnectionInput) -> Result<String, ApiError> {
    let name = valid_name(&input.name)?.to_owned();
    dns::cloudflare_credential(
        &state.pool,
        &state.secrets,
        &input.credential_id,
        input.credential_version,
        false,
    )
    .await
    .map_err(|_| ApiError::bad_request("select an active Cloudflare DNS credential"))?;
    Ok(name)
}

pub async fn list_zones(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let rows = sqlx::query("SELECT * FROM dns_zones ORDER BY name,id")
        .fetch_all(&mut *crate::read_snapshot::begin(&state.pool).await?)
        .await?;
    Ok(Json(rows.iter().map(dns::zone_json).collect()))
}

async fn lock_zone_on(tx: &mut Transaction<'_, Postgres>, id: &str) -> Result<(), ApiError> {
    sqlx::query("SELECT c.id FROM dns_connections c JOIN dns_zones z ON z.connection_id=c.id WHERE z.id=$1 FOR KEY SHARE OF c").bind(id).fetch_optional(&mut **tx).await?;
    sqlx::query("SELECT id FROM dns_zones WHERE id=$1 FOR SHARE")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(())
}

pub async fn zone_for_write(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<sqlx::postgres::PgRow, ApiError> {
    lock_zone_on(tx, id).await?;
    let row=sqlx::query("SELECT z.*,c.credential_version,c.status,c.credential_id FROM dns_zones z JOIN dns_connections c ON c.id=z.connection_id JOIN credentials v ON v.id=c.credential_id WHERE z.id=$1 AND NOT v.archived FOR KEY SHARE OF v")
        .bind(id).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::bad_request("zone or active DNS credential not found"))?;
    if !row.get::<bool, _>("enabled") || row.get::<String, _>("status") != "verified" {
        return Err(ApiError::bad_request(
            "enable a zone from a verified DNS connection",
        ));
    }
    Ok(row)
}

#[derive(Default, Deserialize)]
pub(crate) struct RecordsListQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    zone_id: Option<String>,
    connection_id: Option<String>,
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
}

impl RecordsListQuery {
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

pub(crate) async fn list_records(
    State(state): State<AppState>,
    Query(query): Query<RecordsListQuery>,
) -> Result<Json<Value>, ApiError> {
    let (requested_page, page_size, sort, order) = query.validate()?;
    let zone = query.zone_id.as_deref().filter(|value| !value.is_empty());
    let connection = query
        .connection_id
        .as_deref()
        .filter(|value| !value.is_empty());
    let search = query.q.as_deref().unwrap_or("").trim();
    let filter = "($1::text IS NULL OR r.zone_id=$1) AND ($2::text IS NULL OR z.connection_id=$2) AND ($3 = '' OR strpos(lower(r.name),lower($3)) > 0 OR strpos(lower(r.content),lower($3)) > 0 OR strpos(lower(r.id),lower($3)) > 0) AND (r.deleted_at IS NULL OR EXISTS(SELECT 1 FROM dns_bindings b WHERE b.record_ids ? r.id))";
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
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
        .bind(id).fetch_optional(&mut *crate::read_snapshot::begin(&state.pool).await?).await?.ok_or_else(||ApiError::not_found("DNS record"))?;
    let mut v = dns::record_json(&row);
    v["bound_node_id"] = json!(row.get::<Option<String>, _>("bound_node_id"));
    Ok(Json(v))
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetBinding {
    pub expected_revision: i64,
    pub allocation: crate::dns::binding::Allocation,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Unbind {
    pub expected_revision: i64,
    pub idempotency_key: String,
    pub public_host: String,
}

pub async fn binding_summary(pool: &sqlx::PgPool, node: &str) -> Result<Value, ApiError> {
    let mut connection = crate::read_snapshot::begin(pool).await?;
    Ok(bindings_on(&mut connection, &[node.to_owned()])
        .await?
        .remove(node)
        .unwrap_or(Value::Null))
}
pub(crate) async fn bindings_on(
    connection: &mut sqlx::PgConnection,
    nodes: &[String],
) -> Result<std::collections::BTreeMap<String, Value>, ApiError> {
    let bindings=sqlx::query("SELECT b.*,n.published_connection FROM dns_bindings b JOIN nodes n ON n.id=b.node_id WHERE b.node_id=ANY($1)").bind(nodes).fetch_all(&mut *connection).await?;
    let mut ids = std::collections::BTreeSet::new();
    for binding in &bindings {
        let values: Value = binding.get("record_ids");
        for id in values.as_array().ok_or_else(ApiError::internal)? {
            ids.insert(id.as_str().ok_or_else(ApiError::internal)?.to_owned());
        }
    }
    let ids: Vec<String> = ids.into_iter().collect();
    let rows = sqlx::query("SELECT * FROM dns_records WHERE id=ANY($1)")
        .bind(&ids)
        .fetch_all(connection)
        .await?;
    let records: std::collections::BTreeMap<String, Value> = rows
        .iter()
        .map(|row| (row.get("id"), dns::record_json(row)))
        .collect();
    let mut values = std::collections::BTreeMap::new();
    for row in bindings {
        let id: String = row.get("node_id");
        let ids: Value = row.get("record_ids");
        let selected: Vec<Value> = ids
            .as_array()
            .ok_or_else(ApiError::internal)?
            .iter()
            .map(|id| {
                records
                    .get(id.as_str().unwrap())
                    .cloned()
                    .ok_or_else(ApiError::internal)
            })
            .collect::<Result<_, _>>()?;
        values.insert(id.clone(),json!({"node_id":id,"zone_id":row.get::<String,_>("zone_id"),"hostname":row.get::<String,_>("hostname"),"record_ids":ids,"records":selected,
            "revision":row.get::<i64,_>("revision"),"published_connection":row.get::<Option<Value>,_>("published_connection")}));
    }
    Ok(values)
}

pub async fn get_binding(
    State(state): State<AppState>,
    Path(node): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM nodes WHERE id=$1)")
        .bind(&node)
        .fetch_one(&mut *crate::read_snapshot::begin(&state.pool).await?)
        .await?;
    if !exists {
        return Err(ApiError::not_found("node"));
    }
    Ok(Json(
        json!({"binding":binding_summary(&state.pool, &node).await?}),
    ))
}

fn validate_unbind(input: &Unbind) -> Result<(), ApiError> {
    if input.public_host.trim().is_empty()
        || input.public_host.len() > 253
        || input.public_host.chars().any(char::is_whitespace)
    {
        return Err(ApiError::bad_request(
            "enter a replacement public IP or hostname",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod transaction_tests;

#[cfg(test)]
mod queue_tests;

mod mutations;
use mutations::*;

pub(super) mod v2;

// Direct domain fixtures are test-only; production writes use protocol-2 handlers.
#[cfg(test)]
#[path = "dns_test_support.rs"]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::*;

#[cfg(test)]
use crate::db;
