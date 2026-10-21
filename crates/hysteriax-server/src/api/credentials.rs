use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use super::{AdminActor, enqueue_job_with_payload_in_tx};
use crate::{credentials as vault, db, error::ApiError, state::AppState};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCredential {
    pub name: String,
    pub kind: String,
    pub reminder_at: Option<DateTime<Utc>>,
    pub payload: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchCredential {
    pub expected_revision: i64,
    pub name: String,
    pub archived: bool,
    pub reminder_at: Option<DateTime<Utc>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishVersion {
    pub expected_revision: i64,
    pub payload: Value,
}

fn invalid(error: anyhow::Error) -> ApiError {
    ApiError::bad_request(error.to_string())
}
fn name(value: &str) -> Result<&str, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 180 || value.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "name must contain 1–180 printable characters",
        ));
    }
    Ok(value)
}

pub(super) fn summary(row: &sqlx::postgres::PgRow) -> Value {
    let metadata: Value = row.get("metadata");
    let reminder: Option<DateTime<Utc>> = row.get("reminder_at");
    let expires = metadata
        .get("expires_at")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .or(reminder);
    let days = expires.map(|d| (d - Utc::now()).num_seconds().div_euclid(86400));
    let archived: bool = row.get("archived");
    let state = if archived {
        "archived"
    } else if days.is_some_and(|d| d < 0) {
        "expired"
    } else if days.is_some_and(|d| d <= 30) {
        "expiring"
    } else {
        "active"
    };
    json!({"id":row.get::<String,_>("id"),"name":row.get::<String,_>("name"),"kind":row.get::<String,_>("kind"),"owner_user_id":row.get::<Option<String>,_>("owner_user_id"),"revision":row.get::<i64,_>("revision"),"latest_version":row.get::<i64,_>("latest_version"),"archived":archived,"reminder_at":reminder,"expires_at":expires,"days_remaining":days,"status":state,"metadata":metadata,"created_at":row.get::<DateTime<Utc>,_>("created_at"),"updated_at":row.get::<DateTime<Utc>,_>("updated_at")})
}

// Managed and synthesized credentials share the same summaries and reference rules.
async fn page_values_on(
    connection: &mut sqlx::PgConnection,
    managed: &[String],
    admins: &[String],
    subscriptions: &[String],
    assignment_users: &[String],
    assignment_nodes: &[String],
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query("SELECT c.*,v.metadata FROM credentials c LEFT JOIN mtls_authorities a ON a.credential_id=c.id AND c.kind='ca_identity' JOIN credential_versions v ON v.credential_id=c.id AND v.version=coalesce(a.published_version,c.latest_version) WHERE c.id=ANY($1) ORDER BY lower(c.name) COLLATE \"C\",c.id").bind(managed).fetch_all(&mut *connection).await?;
    let mut counts =
        std::collections::BTreeMap::<String, std::collections::BTreeSet<String>>::new();
    let references=sqlx::query("SELECT DISTINCT credential_id,node_id FROM credential_reference_edges WHERE credential_id=ANY($1) AND source IN ('desired','deployed') UNION SELECT ssh_credential_id,id FROM nodes WHERE ssh_credential_id=ANY($1)").bind(managed).fetch_all(&mut *connection).await?;
    for row in references {
        counts
            .entry(row.get("credential_id"))
            .or_default()
            .insert(format!("node:{}", row.get::<String, _>("node_id")));
    }
    for row in sqlx::query("SELECT user_id,node_id,mtls_credential_id FROM node_assignments WHERE mtls_credential_id=ANY($1)").bind(managed).fetch_all(&mut *connection).await? { counts.entry(row.get("mtls_credential_id")).or_default().insert(format!("assignment:{}:{}",row.get::<String,_>("user_id"),row.get::<String,_>("node_id"))); }
    for connection in
        sqlx::query("SELECT id,credential_id FROM dns_connections WHERE credential_id=ANY($1)")
            .bind(managed)
            .fetch_all(&mut *connection)
            .await?
    {
        counts
            .entry(connection.get("credential_id"))
            .or_default()
            .insert(format!(
                "dns_connection:{}",
                connection.get::<String, _>("id")
            ));
    }
    let managed_nodes = sqlx::query("WITH contexts AS(SELECT ca_id,node_id FROM mtls_node_trust WHERE desired OR deployed UNION SELECT ca_id,node_id FROM mtls_live_activation_trust) SELECT ca_id AS credential_id,node_id FROM contexts WHERE ca_id=ANY($1) UNION SELECT i.credential_id,a.node_id FROM mtls_user_identities i JOIN node_assignments a ON a.user_id=i.user_id JOIN contexts c ON c.ca_id=i.ca_id AND c.node_id=a.node_id WHERE NOT i.archived AND i.credential_id=ANY($1) UNION SELECT p.credential_id,p.node_id FROM mtls_probe_versions p JOIN contexts c ON c.ca_id=p.ca_id AND c.node_id=p.node_id WHERE p.credential_id=ANY($1)").bind(managed).fetch_all(&mut *connection).await?;
    for row in managed_nodes {
        counts
            .entry(row.get("credential_id"))
            .or_default()
            .insert(format!("node:{}", row.get::<String, _>("node_id")));
    }
    let mut entries: Vec<Value> = rows
        .iter()
        .map(|row| {
            let mut entry = summary(row);
            entry["reference_count"] = json!(
                counts
                    .get(&row.get::<String, _>("id"))
                    .map_or(0, |set| set.len())
            );
            entry
        })
        .collect();
    for row in sqlx::query(
        "SELECT id,label,created_at,last_used_at,revoked_at FROM admin_tokens WHERE id=ANY($1) ORDER BY created_at",
    )
    .bind(admins)
    .fetch_all(&mut *connection)
    .await?
    {
        let id: String = row.get("id");
        entries.push(json!({"id":format!("admin:{id}"),"name":row.get::<String,_>("label"),"kind":"admin_token","status":if row.get::<Option<DateTime<Utc>>,_>("revoked_at").is_some(){"revoked"}else{"active"},"created_at":row.get::<DateTime<Utc>,_>("created_at"),"metadata":{"token_id":id,"last_used_at":row.get::<Option<DateTime<Utc>>,_>("last_used_at")},"revision":1,"latest_version":1,"archived":false}));
    }
    for row in sqlx::query("SELECT s.id,s.user_id,s.created_at,s.revoked_at,u.name,u.enabled,u.expires_at,u.quota_bytes,u.usage_bytes FROM subscription_credentials s JOIN users u ON u.id=s.user_id WHERE s.id=ANY($1) ORDER BY s.created_at").bind(subscriptions).fetch_all(&mut *connection).await? {
        entries.push(json!({"id":format!("subscription:{}",row.get::<String,_>("id")),"name":format!("{} · 订阅",row.get::<String,_>("name")),"kind":"subscription_token","owner_user_id":row.get::<String,_>("user_id"),"status":if row.get::<Option<DateTime<Utc>>,_>("revoked_at").is_some(){"revoked"}else{user_status(&row)},"expires_at":row.get::<Option<DateTime<Utc>>,_>("expires_at"),"created_at":row.get::<DateTime<Utc>,_>("created_at"),"metadata":{},"revision":1,"latest_version":1,"archived":false}));
    }
    for row in sqlx::query("SELECT a.user_id,a.node_id,a.created_at,u.name,n.name node_name,u.revision,u.enabled,u.expires_at,u.quota_bytes,u.usage_bytes FROM node_assignments a JOIN unnest($1::text[],$2::text[]) chosen(user_id,node_id) ON chosen.user_id=a.user_id AND chosen.node_id=a.node_id JOIN users u ON u.id=a.user_id JOIN nodes n ON n.id=a.node_id ORDER BY a.created_at").bind(assignment_users).bind(assignment_nodes).fetch_all(&mut *connection).await? {
        entries.push(json!({"id":format!("user:{}:{}",row.get::<String,_>("user_id"),row.get::<String,_>("node_id")),"name":format!("{} · {}",row.get::<String,_>("name"),row.get::<String,_>("node_name")),"kind":"user_credential","owner_user_id":row.get::<String,_>("user_id"),"status":user_status(&row),"expires_at":row.get::<Option<DateTime<Utc>>,_>("expires_at"),"created_at":row.get::<DateTime<Utc>,_>("created_at"),"metadata":{"node_id":row.get::<String,_>("node_id")},"revision":row.get::<i64,_>("revision"),"latest_version":1,"archived":false}));
    }
    Ok(entries)
}

#[derive(Default, Deserialize)]
pub(crate) struct CredentialsListQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    category: Option<String>,
    kind: Option<String>,
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    include_internal: Option<bool>,
}

impl CredentialsListQuery {
    fn validate(&self) -> Result<(i64, i64, &str, &str), ApiError> {
        let page = self.page.unwrap_or(1);
        let size = self.page_size.unwrap_or(50);
        if page < 1 || !(1..=200).contains(&size) {
            return Err(ApiError::bad_request(
                "page must be positive and page_size must be between 1 and 200",
            ));
        }
        match self.category.as_deref().unwrap_or("all") {
            "all" | "operations" | "user" => {}
            _ => return Err(ApiError::bad_request("invalid credential category")),
        }
        let sort = self.sort.as_deref().unwrap_or("name");
        if !["name", "kind", "status", "created_at"].contains(&sort) {
            return Err(ApiError::bad_request("invalid credential sort field"));
        }
        let order = self.order.as_deref().unwrap_or("asc");
        if !["asc", "desc"].contains(&order) {
            return Err(ApiError::bad_request("invalid credential sort order"));
        }
        Ok((page, size, sort, order))
    }
}

pub(crate) async fn list(
    State(state): State<AppState>,
    Query(query): Query<CredentialsListQuery>,
) -> Result<Json<Value>, ApiError> {
    let (requested_page, page_size, sort, order) = query.validate()?;
    let search = query.q.as_deref().unwrap_or("").trim();
    let pattern = super::literal_search_pattern(search);
    let kind = query.kind.as_deref().unwrap_or("");
    let category = query.category.as_deref().unwrap_or("all");
    let filter = "($1='all' OR (owner_user_id IS NOT NULL)=($1='user')) AND ($2='' OR kind=$2) AND ($3='' OR lower(name) LIKE lower($3) ESCAPE '\\') AND ($4 OR source<>'managed' OR NOT EXISTS(SELECT 1 FROM mtls_probe_versions p WHERE p.credential_id=credential_directory.entity_id))";
    let mut tx =
        crate::credentials::reference_index::begin_read(&state.pool, &state.secrets).await?;
    sqlx::query("SET LOCAL plan_cache_mode=force_custom_plan")
        .execute(&mut *tx)
        .await?;
    let total: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM credential_directory WHERE {filter}"
    ))
    .bind(category)
    .bind(kind)
    .bind(&pattern)
    .bind(query.include_internal.unwrap_or(false))
    .fetch_one(&mut *tx)
    .await?;
    let page = requested_page.min(((total + page_size - 1) / page_size).max(1));
    let direction = if order == "asc" { "ASC" } else { "DESC" };
    let leading = match sort {
        "name" => "lower(name) COLLATE \"C\"".to_owned(),
        "created_at" => "created_at".to_owned(),
        field => format!("lower({field}) COLLATE \"C\""),
    };
    let ordering = if sort == "name" {
        format!("{leading} {direction},name COLLATE \"C\" {direction},id COLLATE \"C\" {direction}")
    } else {
        format!(
            "{leading} {direction},lower(name) COLLATE \"C\" {direction},name COLLATE \"C\" {direction},id COLLATE \"C\" {direction}"
        )
    };
    let selected = sqlx::query(&format!("SELECT id,source,entity_id,node_id FROM credential_directory WHERE {filter} ORDER BY {ordering} LIMIT $5 OFFSET $6"))
        .bind(category).bind(kind).bind(&pattern).bind(query.include_internal.unwrap_or(false)).bind(page_size).bind((page-1)*page_size)
        .fetch_all(&mut *tx).await?;
    let mut managed = Vec::new();
    let mut admins = Vec::new();
    let mut subscriptions = Vec::new();
    let mut assignment_users = Vec::new();
    let mut assignment_nodes = Vec::new();
    for row in &selected {
        match row.get::<&str, _>("source") {
            "managed" => managed.push(row.get::<String, _>("entity_id")),
            "admin" => admins.push(row.get::<String, _>("entity_id")),
            "subscription" => subscriptions.push(row.get::<String, _>("entity_id")),
            "assignment" => {
                assignment_users.push(row.get::<String, _>("entity_id"));
                assignment_nodes.push(row.get::<String, _>("node_id"));
            }
            _ => return Err(ApiError::internal()),
        }
    }
    let entries = page_values_on(
        &mut tx,
        &managed,
        &admins,
        &subscriptions,
        &assignment_users,
        &assignment_nodes,
    )
    .await?;
    let mut entries: std::collections::BTreeMap<String, Value> = entries
        .into_iter()
        .map(|value| (value["id"].as_str().unwrap_or_default().to_owned(), value))
        .collect();
    let items: Vec<Value> = selected
        .iter()
        .map(|row| {
            entries
                .remove(row.get::<&str, _>("id"))
                .ok_or_else(ApiError::internal)
        })
        .collect::<Result<_, _>>()?;
    tx.commit().await?;
    Ok(Json(
        json!({"items":items,"total":total,"page":page,"page_size":page_size}),
    ))
}

#[cfg(test)]
#[path = "credentials_pagination_tests.rs"]
mod pagination_tests;

fn user_status(row: &sqlx::postgres::PgRow) -> &'static str {
    if !row.get::<bool, _>("enabled") {
        "disabled"
    } else if row
        .get::<Option<DateTime<Utc>>, _>("expires_at")
        .is_some_and(|t| t <= Utc::now())
    {
        "expired"
    } else if row
        .get::<Option<i64>, _>("quota_bytes")
        .is_some_and(|q| row.get::<i64, _>("usage_bytes") >= q)
    {
        "quota_exhausted"
    } else {
        "active"
    }
}

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mut tx =
        crate::credentials::reference_index::begin_read(&state.pool, &state.secrets).await?;
    let row = sqlx::query("SELECT c.*,v.metadata FROM credentials c LEFT JOIN mtls_authorities a ON a.credential_id=c.id AND c.kind='ca_identity' JOIN credential_versions v ON v.credential_id=c.id AND v.version=coalesce(a.published_version,c.latest_version) WHERE c.id=$1").bind(&id).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::not_found("credential"))?;
    let mut result = summary(&row);
    let versions = sqlx::query("SELECT version,metadata,created_at FROM credential_versions WHERE credential_id=$1 ORDER BY version DESC").bind(&id).fetch_all(&mut *tx).await?;
    result["versions"] = json!(versions.iter().map(|v|json!({"version":v.get::<i64,_>("version"),"metadata":v.get::<Value,_>("metadata"),"created_at":v.get::<DateTime<Utc>,_>("created_at")})).collect::<Vec<_>>());
    result["references"] = json!(read_references_on(&mut tx, &id).await?);
    if let Some(ca) = sqlx::query("SELECT * FROM mtls_authorities WHERE credential_id=$1")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
    {
        result["ca"] = json!({"source":ca.get::<String,_>("source"),"policy_revision":ca.get::<i64,_>("policy_revision"),"published_version":ca.get::<i64,_>("published_version"),"client_validity_days":ca.get::<i32,_>("client_validity_days"),"renew_before_hours":ca.get::<i32,_>("renew_before_hours"),"maintenance_error":ca.get::<Option<String>,_>("maintenance_error")});
    }
    if result.get("ca").is_some() {
        let cleanup = sqlx::query("SELECT c.ca_version,c.next_action_at,c.completed_at,coalesce(jsonb_agg(jsonb_build_object('node_id',p.node_id,'name',n.name,'job_id',p.job_id,'completed',p.completed,'status',j.status,'stage',j.stage,'error_message',coalesce(p.error_message,j.error_message)) ORDER BY p.node_id) FILTER(WHERE p.node_id IS NOT NULL),'[]'::jsonb) AS nodes FROM mtls_trust_cleanup c LEFT JOIN mtls_cleanup_nodes p ON p.ca_id=c.ca_id AND p.ca_version=c.ca_version LEFT JOIN nodes n ON n.id=p.node_id LEFT JOIN jobs j ON j.id=p.job_id WHERE c.ca_id=$1 GROUP BY c.ca_id,c.ca_version ORDER BY c.ca_version DESC").bind(&id).fetch_all(&mut *tx).await?;
        result["ca"]["cleanup"] = json!(cleanup.iter().map(|r|json!({"version":r.get::<i64,_>("ca_version"),"next_action_at":r.get::<DateTime<Utc>,_>("next_action_at"),"completed_at":r.get::<Option<DateTime<Utc>>,_>("completed_at"),"nodes":r.get::<Value,_>("nodes")})).collect::<Vec<_>>());
    }
    let batches = sqlx::query(
        "SELECT id FROM credential_batches WHERE credential_id=$1 ORDER BY created_at DESC",
    )
    .bind(&id)
    .fetch_all(&mut *tx)
    .await?;
    let ids: Vec<String> = batches.iter().map(|row| row.get("id")).collect();
    result["batches"] = json!(batch_values_on(&mut tx, &ids).await?);
    tx.commit().await?;
    Ok(Json(result))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Json(input): Json<CreateCredential>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let name = name(&input.name)?;
    if input.kind == "ca_identity" {
        return Err(ApiError::bad_request(
            "use the CA identity creation endpoint",
        ));
    }
    let metadata = vault::validate_payload(&input.kind, &input.payload).map_err(invalid)?;
    let mut tx = db::begin_write(&state.pool).await?;
    let id = vault::insert(
        &mut tx,
        &state.secrets,
        name,
        &input.kind,
        None,
        &input.payload,
        &metadata,
        &json!({}),
    )
    .await?;
    sqlx::query("UPDATE credentials SET reminder_at=$1 WHERE id=$2")
        .bind(input.reminder_at)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    record(
        &mut tx,
        &actor,
        "credential.created",
        &id,
        json!({"kind":input.kind,"version":1}),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":id,"revision":1,"version":1})),
    ))
}

pub async fn patch(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    Json(input): Json<PatchCredential>,
) -> Result<Json<Value>, ApiError> {
    let name = name(&input.name)?;
    let mut tx = db::begin_write(&state.pool).await?;
    let managed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credential_versions WHERE credential_id=$1 AND metadata->>'managed'='true' UNION ALL SELECT 1 FROM credentials WHERE id=$1 AND owner_user_id IS NOT NULL)").bind(&id).fetch_one(&mut *tx).await?;
    if managed {
        return Err(ApiError::bad_request(
            "system-issued identities cannot be edited",
        ));
    }
    if input.archived {
        let active_ca: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_node_trust WHERE ca_id=$1 AND (desired OR deployed) UNION ALL SELECT 1 FROM mtls_user_identities WHERE ca_id=$1 AND NOT archived UNION ALL SELECT 1 FROM mtls_rotations WHERE ca_id=$1 AND phase<>'complete' UNION ALL SELECT 1 FROM mtls_live_activation_trust WHERE ca_id=$1)").bind(&id).fetch_one(&mut *tx).await?;
        if active_ca {
            return Err(ApiError::conflict("CA identity is in use"));
        }
    }
    let rows = sqlx::query("UPDATE credentials SET name=$1,archived=$2,reminder_at=$3,revision=revision+1,updated_at=$4 WHERE id=$5 AND revision=$6").bind(name).bind(input.archived).bind(input.reminder_at).bind(Utc::now()).bind(&id).bind(input.expected_revision).execute(&mut *tx).await?;
    if rows.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "credential changed or was deleted; reload before editing",
        ));
    }
    record(
        &mut tx,
        &actor,
        "credential.updated",
        &id,
        json!({"archived":input.archived}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"id":id,"revision":input.expected_revision+1})))
}

pub async fn publish(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    Json(input): Json<PublishVersion>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let row = sqlx::query("SELECT * FROM credentials WHERE id=$1")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ApiError::not_found("credential"))?;
    if row.get::<i64, _>("revision") != input.expected_revision || row.get::<bool, _>("archived") {
        return Err(ApiError::conflict("credential changed or is archived"));
    }
    let kind: String = row.get("kind");
    if kind == "ca_identity" {
        return Err(ApiError::bad_request(
            "CA versions require managed rotation",
        ));
    }
    let managed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credential_versions WHERE credential_id=$1 AND metadata->>'managed'='true' UNION ALL SELECT 1 FROM credentials WHERE id=$1 AND owner_user_id IS NOT NULL)").bind(&id).fetch_one(&mut *tx).await?;
    if managed {
        return Err(ApiError::bad_request(
            "system-issued identities cannot be edited",
        ));
    }
    let metadata = vault::validate_payload(&kind, &input.payload).map_err(invalid)?;
    if kind == "dns" {
        let prior: Value = sqlx::query_scalar(
            "SELECT metadata FROM credential_versions WHERE credential_id=$1 AND version=$2",
        )
        .bind(&id)
        .bind(row.get::<i64, _>("latest_version"))
        .fetch_one(&mut *tx)
        .await?;
        if prior["provider"] != metadata["provider"] {
            return Err(ApiError::bad_request(
                "DNS provider cannot change between versions; create a new credential instead",
            ));
        }
    }
    let version = row.get::<i64, _>("latest_version") + 1;
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES($1,$2,$3,$4,$5)").bind(&id).bind(version).bind(state.secrets.encrypt(&input.payload.to_string())?).bind(metadata).bind(Utc::now()).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE credentials SET latest_version=$1,revision=revision+1,updated_at=$2 WHERE id=$3",
    )
    .bind(version)
    .bind(Utc::now())
    .bind(&id)
    .execute(&mut *tx)
    .await?;
    let batch = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO credential_batches(id,credential_id,version,created_at) VALUES($1,$2,$3,$4)",
    )
    .bind(&batch)
    .bind(&id)
    .bind(version)
    .bind(Utc::now())
    .execute(&mut *tx)
    .await?;
    let nodes = sqlx::query("SELECT id,desired_revision,desired_config_enc,ssh_credential_id FROM nodes WHERE state NOT IN ('deleting','delete_failed') ORDER BY id").fetch_all(&mut *tx).await?;
    let mut count = 0;
    for node in nodes {
        let config: Value = serde_json::from_str(
            &state
                .secrets
                .decrypt(&node.get::<String, _>("desired_config_enc"))?,
        )
        .map_err(|_| ApiError::internal())?;
        let mode = if node
            .get::<Option<String>, _>("ssh_credential_id")
            .as_deref()
            == Some(&id)
        {
            Some("ssh")
        } else if vault::references(&config)
            .map_err(invalid)?
            .iter()
            .any(|(_, r)| r.id == id)
        {
            Some("config")
        } else {
            None
        };
        if let Some(mode) = mode {
            let node_id: String = node.get("id");
            let revision: i64 = node.get("desired_revision");
            let job = enqueue_job_with_payload_in_tx(
                &mut tx,
                "credential-apply",
                Some(&node_id),
                None,
                json!({"batch_id":batch,"credential_id":id,"version":version,"mode":mode}),
            )
            .await?;
            sqlx::query("INSERT INTO credential_batch_items(batch_id,node_id,expected_revision,job_id) VALUES($1,$2,$3,$4)").bind(&batch).bind(&node_id).bind(revision).bind(job).execute(&mut *tx).await?;
            count += 1;
        }
    }
    let dns_connections = sqlx::query("SELECT id FROM dns_connections WHERE credential_id=$1")
        .bind(&id)
        .fetch_all(&mut *tx)
        .await?;
    for connection in dns_connections {
        crate::dns::rotation::enqueue(&mut tx, &connection.get::<String, _>("id"), version, &batch)
            .await?;
        count += 1;
    }
    record(
        &mut tx,
        &actor,
        "credential.version_published",
        &id,
        json!({"version":version,"batch_id":batch,"affected_count":count}),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(
            json!({"id":id,"revision":input.expected_revision+1,"version":version,"batch_id":batch,"affected_count":count}),
        ),
    ))
}

pub async fn references(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Value>>, ApiError> {
    Ok(Json(find_references(&state, &id).await?))
}

pub(crate) async fn find_references(state: &AppState, id: &str) -> Result<Vec<Value>, ApiError> {
    let mut tx =
        crate::credentials::reference_index::begin_read(&state.pool, &state.secrets).await?;
    let references = read_references_on(&mut tx, id).await?;
    tx.commit().await?;
    Ok(references)
}

async fn find_references_on(
    connection: &mut sqlx::PgConnection,
    _secrets: &crate::security::SecretBox,
    id: &str,
) -> Result<Vec<Value>, ApiError> {
    crate::credentials::reference_index::assert_clean_on(connection).await?;
    read_references_on(connection, id).await
}

async fn read_references_on(
    connection: &mut sqlx::PgConnection,
    id: &str,
) -> Result<Vec<Value>, ApiError> {
    let mut out = Vec::new();
    for row in
        sqlx::query("SELECT id,name,credential_version FROM dns_connections WHERE credential_id=$1")
            .bind(id)
            .fetch_all(&mut *connection)
            .await?
    {
        out.push(json!({"entity_type":"dns_connection","entity_id":row.get::<String,_>("id"),"name":row.get::<String,_>("name"),"source":"dns","version":row.get::<i64,_>("credential_version")}));
    }
    for row in
        sqlx::query("SELECT id,name,ssh_credential_version FROM nodes WHERE ssh_credential_id=$1")
            .bind(id)
            .fetch_all(&mut *connection)
            .await?
    {
        out.push(json!({"entity_type":"node","entity_id":row.get::<String,_>("id"),"name":row.get::<String,_>("name"),"source":"ssh","version":row.get::<Option<i64>,_>("ssh_credential_version")}));
    }
    for row in sqlx::query("SELECT e.node_id,n.name,e.source,e.config_revision,e.version,e.field FROM credential_reference_edges e JOIN nodes n ON n.id=e.node_id WHERE e.credential_id=$1 ORDER BY e.source,e.node_id,e.config_revision,e.field").bind(id).fetch_all(&mut *connection).await? {
        let source:String=row.get("source");
        let mut reference=json!({"entity_type":"node","entity_id":row.get::<String,_>("node_id"),"source":source,"version":row.get::<i64,_>("version"),"field":row.get::<String,_>("field")});
        if source=="history" { reference["config_revision"]=json!(row.get::<i64,_>("config_revision")); }
        else { reference["name"]=json!(row.get::<String,_>("name")); }
        out.push(reference);
    }
    for row in
        sqlx::query("SELECT DISTINCT b.id,b.version FROM credential_batches b JOIN credential_batch_items i ON i.batch_id=b.id JOIN jobs j ON j.id=i.job_id WHERE b.credential_id=$1 AND j.status IN ('queued','running')")
            .bind(id)
            .fetch_all(&mut *connection)
            .await?
    {
        out.push(json!({"entity_type":"batch","entity_id":row.get::<String,_>("id"),"source":"batch","version":row.get::<i64,_>("version")}));
    }
    for row in sqlx::query("SELECT i.id,i.user_id,i.ca_id,v.credential_version,v.ca_version FROM mtls_user_identities i LEFT JOIN mtls_user_versions v ON v.identity_id=i.id WHERE i.ca_id=$1 OR i.credential_id=$1 ORDER BY i.id,v.credential_version").bind(id).fetch_all(&mut *connection).await? {
        out.push(json!({"entity_type":"user","entity_id":row.get::<String,_>("user_id"),"source":"managed_identity","identity_id":row.get::<String,_>("id"),"ca_id":row.get::<String,_>("ca_id"),"version":row.get::<Option<i64>,_>("credential_version"),"ca_version":row.get::<Option<i64>,_>("ca_version")}));
    }
    for row in sqlx::query("SELECT node_id,ca_version,desired,deployed FROM mtls_node_trust WHERE ca_id=$1 ORDER BY node_id,ca_version").bind(id).fetch_all(&mut *connection).await? {
        out.push(json!({"entity_type":"node","entity_id":row.get::<String,_>("node_id"),"source":"managed_trust","version":row.get::<i64,_>("ca_version"),"desired":row.get::<bool,_>("desired"),"deployed":row.get::<bool,_>("deployed")}));
    }
    for row in sqlx::query("SELECT node_id,credential_version,ca_version FROM mtls_probe_versions WHERE ca_id=$1 OR credential_id=$1 ORDER BY node_id,credential_version").bind(id).fetch_all(&mut *connection).await? {
        out.push(json!({"entity_type":"node","entity_id":row.get::<String,_>("node_id"),"source":"managed_probe","version":row.get::<i64,_>("credential_version"),"ca_version":row.get::<i64,_>("ca_version")}));
    }
    for row in sqlx::query(
        "SELECT id,ca_version,phase FROM mtls_rotations WHERE ca_id=$1 AND phase<>'complete'",
    )
    .bind(id)
    .fetch_all(&mut *connection)
    .await?
    {
        out.push(json!({"entity_type":"rotation","entity_id":row.get::<String,_>("id"),"source":"managed_rotation","version":row.get::<i64,_>("ca_version"),"phase":row.get::<String,_>("phase")}));
    }
    for row in sqlx::query(
        "SELECT ca_version FROM mtls_trust_cleanup WHERE ca_id=$1 AND completed_at IS NULL",
    )
    .bind(id)
    .fetch_all(&mut *connection)
    .await?
    {
        out.push(json!({"entity_type":"ca_cleanup","entity_id":id,"source":"managed_cleanup","version":row.get::<i64,_>("ca_version")}));
    }
    for row in sqlx::query("SELECT a.id,a.node_id,a.target_revision,a.phase,t.ca_version FROM mtls_activation_trust t JOIN mtls_activations a ON a.id=t.activation_id WHERE t.ca_id=$1").bind(id).fetch_all(&mut *connection).await? {
        out.push(json!({"entity_type":"activation","entity_id":row.get::<String,_>("id"),"node_id":row.get::<String,_>("node_id"),"source":"managed_activation","version":row.get::<i64,_>("ca_version"),"config_revision":row.get::<i64,_>("target_revision"),"phase":row.get::<String,_>("phase")}));
    }
    for row in sqlx::query("SELECT id,node_id,probe_credential_version,phase FROM mtls_activations WHERE probe_credential_id=$1").bind(id).fetch_all(&mut *connection).await? {
        out.push(json!({"entity_type":"activation","entity_id":row.get::<String,_>("id"),"node_id":row.get::<String,_>("node_id"),"source":"managed_activation_probe","version":row.get::<i64,_>("probe_credential_version"),"phase":row.get::<String,_>("phase")}));
    }
    Ok(out)
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<super::nodes::RevisionQuery>,
) -> Result<StatusCode, ApiError> {
    let expected = query
        .expected_revision
        .ok_or_else(|| ApiError::bad_request("expected_revision is required"))?;
    let secrets = state.secrets.clone();
    db::serializable(&state.pool, move |tx| {
        let (id, actor, secrets) = (id.clone(), actor.clone(), secrets.clone());
        Box::pin(async move {
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
            sqlx::query(
                "DELETE FROM mtls_trust_cleanup WHERE ca_id=$1 AND completed_at IS NOT NULL",
            )
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
        })
    })
    .await
}

async fn batch_values_on(
    connection: &mut sqlx::PgConnection,
    ids: &[String],
) -> Result<Vec<Value>, ApiError> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let batches = sqlx::query("SELECT * FROM credential_batches WHERE id=ANY($1)")
        .bind(ids)
        .fetch_all(&mut *connection)
        .await?;
    let items=sqlx::query("SELECT i.batch_id,i.node_id,i.user_id,i.job_id,j.status,j.stage,j.error_message,n.name FROM credential_batch_items i JOIN jobs j ON j.id=i.job_id LEFT JOIN nodes n ON n.id=i.node_id WHERE i.batch_id=ANY($1) ORDER BY i.node_id,i.user_id").bind(ids).fetch_all(&mut *connection).await?;
    let dns=sqlx::query("SELECT i.batch_id,i.connection_id,i.job_id,c.name,j.status,j.stage,j.error_message FROM dns_credential_batch_items i JOIN dns_connections c ON c.id=i.connection_id JOIN jobs j ON j.id=i.job_id WHERE i.batch_id=ANY($1) ORDER BY c.name").bind(ids).fetch_all(&mut *connection).await?;
    let rotations=sqlx::query("SELECT r.id,r.batch_id,r.ca_version,r.phase,r.outcome,r.published_at,r.error_message,r.next_action_at,coalesce(jsonb_agg(jsonb_build_object('node_id',p.node_id,'name',n.name,'trust_prepared',p.trust_prepared,'probe_switched',p.probe_switched,'job_id',p.job_id,'error_message',coalesce(p.error_message,j.error_message),'status',j.status,'stage',j.stage) ORDER BY p.node_id) FILTER(WHERE p.node_id IS NOT NULL),'[]'::jsonb) AS nodes FROM mtls_rotations r LEFT JOIN mtls_rotation_nodes p ON p.rotation_id=r.id LEFT JOIN nodes n ON n.id=p.node_id LEFT JOIN jobs j ON j.id=p.job_id WHERE r.batch_id=ANY($1) GROUP BY r.id").bind(ids).fetch_all(&mut *connection).await?;
    let mut rotation_map = std::collections::BTreeMap::<String, Value>::new();
    for row in rotations {
        let rotation_status =
            crate::credentials::rotations::status_on(connection, &row.get::<String, _>("id"))
                .await?;
        rotation_map.insert(row.get("batch_id"),json!({"id":row.get::<String,_>("id"),"version":row.get::<i64,_>("ca_version"),"phase":row.get::<String,_>("phase"),"error_message":row.get::<Option<String>,_>("error_message"),"next_action_at":row.get::<DateTime<Utc>,_>("next_action_at"),"nodes":row.get::<Value,_>("nodes"),"status":rotation_status,"outcome":row.get::<Option<String>,_>("outcome"),"published_at":row.get::<Option<DateTime<Utc>>,_>("published_at")}));
    }
    let mut item_map = std::collections::BTreeMap::<String, Vec<Value>>::new();
    let mut dns_map = std::collections::BTreeMap::<String, Vec<Value>>::new();
    for row in items {
        item_map.entry(row.get("batch_id")).or_default().push(json!({"node_id":row.get::<String,_>("node_id"),"user_id":row.get::<String,_>("user_id"),"job_id":row.get::<String,_>("job_id"),
        "status":row.get::<String,_>("status"),"stage":row.get::<String,_>("stage"),"error_message":row.get::<Option<String>,_>("error_message"),"name":row.get::<Option<String>,_>("name")}));
    }
    for row in dns {
        dns_map.entry(row.get("batch_id")).or_default().push(json!({"connection_id":row.get::<String,_>("connection_id"),"job_id":row.get::<String,_>("job_id"),"name":row.get::<String,_>("name"),
        "status":row.get::<String,_>("status"),"stage":row.get::<String,_>("stage"),"error_message":row.get::<Option<String>,_>("error_message")}));
    }
    let mut values = std::collections::BTreeMap::new();
    for row in batches {
        let id: String = row.get("id");
        let items = item_map.remove(&id).unwrap_or_default();
        let dns = dns_map.remove(&id).unwrap_or_default();
        let mut value = json!({"id":id,"credential_id":row.get::<String,_>("credential_id"),"version":row.get::<i64,_>("version"),"created_at":row.get::<DateTime<Utc>,_>("created_at"),"items":items,"dns_items":dns});
        if let Some(rotation) = rotation_map.remove(&id) {
            value["ca_rotation"] = rotation;
        }
        values.insert(id, value);
    }
    ids.iter()
        .map(|id| {
            values
                .remove(id)
                .ok_or_else(|| ApiError::not_found("credential batch"))
        })
        .collect()
}
pub async fn batch(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let value = batch_values_on(&mut tx, &[id])
        .await?
        .into_iter()
        .next()
        .ok_or_else(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(value))
}

pub async fn retry(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let batch=sqlx::query("SELECT b.*,c.latest_version,c.archived FROM credential_batches b JOIN credentials c ON c.id=b.credential_id WHERE b.id=$1").bind(&id).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::not_found("credential batch"))?;
    if let Some(rotation) =
        sqlx::query("SELECT id,phase,outcome FROM mtls_rotations WHERE batch_id=$1")
            .bind(&id)
            .fetch_optional(&mut *tx)
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
        sqlx::query("UPDATE mtls_rotations SET retry_requested=TRUE,next_action_at=$2,error_message=NULL WHERE id=$1 AND phase<>'complete'").bind(&rotation_id).bind(Utc::now()).execute(&mut *tx).await?;
        let count=sqlx::query("UPDATE mtls_rotation_nodes p SET job_id=NULL,error_message=NULL FROM jobs j WHERE p.rotation_id=$1 AND p.job_id=j.id AND j.status IN ('failed','rolled_back','cancelled')").bind(&rotation_id).execute(&mut *tx).await?.rows_affected();
        record(&mut tx,&actor,"ca.rotation_retried",&batch.get::<String,_>("credential_id"),json!({"rotation_id":rotation_id,"version":batch.get::<i64,_>("version"),"affected_count":count})).await?;
        tx.commit().await?;
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({"batch_id":id,"affected_count":count})),
        ));
    }
    if batch.get::<bool, _>("archived")
        || batch.get::<i64, _>("version") != batch.get::<i64, _>("latest_version")
    {
        return Err(ApiError::conflict(
            "cannot retry archived or superseded credential version",
        ));
    }
    let items=sqlx::query("SELECT i.*,j.payload_json FROM credential_batch_items i JOIN jobs j ON j.id=i.job_id WHERE i.batch_id=$1 AND j.status IN ('failed','rolled_back','cancelled')").bind(&id).fetch_all(&mut *tx).await?;
    let mut count = 0;
    for item in items {
        let node: String = item.get("node_id");
        let user: String = item.get("user_id");
        let credential: String = batch.get("credential_id");
        let current=sqlx::query("SELECT desired_revision,ssh_credential_id,desired_config_enc FROM nodes WHERE id=$1 AND state NOT IN ('deleting','delete_failed')").bind(&node).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::conflict("referenced node was deleted"))?;
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
            enqueue_job_with_payload_in_tx(&mut tx, "credential-apply", Some(&node), None, payload)
                .await?;
        sqlx::query("UPDATE credential_batch_items SET expected_revision=$1,job_id=$2,applied_at=NULL,apply_stage=NULL WHERE batch_id=$3 AND node_id=$4 AND user_id=$5").bind(revision).bind(job).bind(&id).bind(node).bind(user).execute(&mut *tx).await?;
        count += 1;
    }
    let dns_items=sqlx::query("SELECT i.connection_id FROM dns_credential_batch_items i JOIN jobs j ON j.id=i.job_id WHERE i.batch_id=$1 AND j.status IN ('failed','cancelled')").bind(&id).fetch_all(&mut *tx).await?;
    for item in dns_items {
        crate::dns::rotation::enqueue(
            &mut tx,
            &item.get::<String, _>("connection_id"),
            batch.get("version"),
            &id,
        )
        .await?;
        count += 1;
    }
    record(
        &mut tx,
        &actor,
        "credential.batch_retried",
        &batch.get::<String, _>("credential_id"),
        json!({"batch_id":id,"affected_count":count}),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"batch_id":id,"affected_count":count})),
    ))
}

pub(super) async fn record(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &AdminActor,
    action: &str,
    id: &str,
    detail: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,$2,$3,'credential',$4,$5,$6)").bind(Uuid::new_v4().to_string()).bind(&actor.id).bind(action).bind(id).bind(detail).bind(Utc::now()).execute(&mut **tx).await?;
    Ok(())
}
