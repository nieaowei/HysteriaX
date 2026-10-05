use axum::{
    Json,
    extract::{Extension, Path, State},
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
    pub owner_user_id: Option<String>,
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

fn summary(row: &sqlx::postgres::PgRow) -> Value {
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

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let rows = sqlx::query("SELECT c.*,v.metadata FROM credentials c JOIN credential_versions v ON v.credential_id=c.id AND v.version=c.latest_version ORDER BY lower(c.name) COLLATE \"C\",c.id").fetch_all(&state.pool).await?;
    let mut counts =
        std::collections::BTreeMap::<String, std::collections::BTreeSet<String>>::new();
    for node in
        sqlx::query("SELECT id,ssh_credential_id,desired_config_enc,deployed_config_enc FROM nodes")
            .fetch_all(&state.pool)
            .await?
    {
        let node_id: String = node.get("id");
        if let Some(id) = node.get::<Option<String>, _>("ssh_credential_id") {
            counts
                .entry(id)
                .or_default()
                .insert(format!("node:{node_id}"));
        }
        for field in ["desired_config_enc", "deployed_config_enc"] {
            if let Some(cipher) = node.get::<Option<String>, _>(field) {
                let config: Value = serde_json::from_str(&state.secrets.decrypt(&cipher)?)
                    .map_err(|_| ApiError::internal())?;
                for (_, r) in vault::references(&config).map_err(invalid)? {
                    counts
                        .entry(r.id)
                        .or_default()
                        .insert(format!("node:{node_id}"));
                }
            }
        }
    }
    for row in sqlx::query("SELECT user_id,node_id,mtls_credential_id FROM node_assignments WHERE mtls_credential_id IS NOT NULL").fetch_all(&state.pool).await? { counts.entry(row.get("mtls_credential_id")).or_default().insert(format!("assignment:{}:{}",row.get::<String,_>("user_id"),row.get::<String,_>("node_id"))); }
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
        "SELECT id,label,created_at,last_used_at,revoked_at FROM admin_tokens ORDER BY created_at",
    )
    .fetch_all(&state.pool)
    .await?
    {
        let id: String = row.get("id");
        entries.push(json!({"id":format!("admin:{id}"),"name":row.get::<String,_>("label"),"kind":"admin_token","status":if row.get::<Option<DateTime<Utc>>,_>("revoked_at").is_some(){"revoked"}else{"active"},"created_at":row.get::<DateTime<Utc>,_>("created_at"),"metadata":{"token_id":id,"last_used_at":row.get::<Option<DateTime<Utc>>,_>("last_used_at")},"revision":1,"latest_version":1,"archived":false}));
    }
    for row in sqlx::query("SELECT s.id,s.user_id,s.created_at,s.revoked_at,u.name,u.enabled,u.expires_at,u.quota_bytes,u.usage_bytes FROM subscription_credentials s JOIN users u ON u.id=s.user_id ORDER BY s.created_at").fetch_all(&state.pool).await? {
        entries.push(json!({"id":format!("subscription:{}",row.get::<String,_>("id")),"name":format!("{} · 订阅",row.get::<String,_>("name")),"kind":"subscription_token","owner_user_id":row.get::<String,_>("user_id"),"status":if row.get::<Option<DateTime<Utc>>,_>("revoked_at").is_some(){"revoked"}else{user_status(&row)},"expires_at":row.get::<Option<DateTime<Utc>>,_>("expires_at"),"created_at":row.get::<DateTime<Utc>,_>("created_at"),"metadata":{},"revision":1,"latest_version":1,"archived":false}));
    }
    for row in sqlx::query("SELECT a.user_id,a.node_id,a.created_at,u.name,n.name node_name,u.revision,u.enabled,u.expires_at,u.quota_bytes,u.usage_bytes FROM node_assignments a JOIN users u ON u.id=a.user_id JOIN nodes n ON n.id=a.node_id ORDER BY a.created_at").fetch_all(&state.pool).await? {
        entries.push(json!({"id":format!("user:{}:{}",row.get::<String,_>("user_id"),row.get::<String,_>("node_id")),"name":format!("{} · {}",row.get::<String,_>("name"),row.get::<String,_>("node_name")),"kind":"user_credential","owner_user_id":row.get::<String,_>("user_id"),"status":user_status(&row),"expires_at":row.get::<Option<DateTime<Utc>>,_>("expires_at"),"created_at":row.get::<DateTime<Utc>,_>("created_at"),"metadata":{"node_id":row.get::<String,_>("node_id")},"revision":row.get::<i64,_>("revision"),"latest_version":1,"archived":false}));
    }
    Ok(Json(entries))
}

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
    let row = sqlx::query("SELECT c.*,v.metadata FROM credentials c JOIN credential_versions v ON v.credential_id=c.id AND v.version=c.latest_version WHERE c.id=$1").bind(&id).fetch_optional(&state.pool).await?.ok_or_else(||ApiError::not_found("credential"))?;
    let mut result = summary(&row);
    let versions = sqlx::query("SELECT version,metadata,created_at FROM credential_versions WHERE credential_id=$1 ORDER BY version DESC").bind(&id).fetch_all(&state.pool).await?;
    result["versions"] = json!(versions.iter().map(|v|json!({"version":v.get::<i64,_>("version"),"metadata":v.get::<Value,_>("metadata"),"created_at":v.get::<DateTime<Utc>,_>("created_at")})).collect::<Vec<_>>());
    result["references"] = json!(find_references(&state, &id).await?);
    let batches = sqlx::query(
        "SELECT id FROM credential_batches WHERE credential_id=$1 ORDER BY created_at DESC",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await?;
    let mut output = Vec::new();
    for batch in batches {
        output.push(batch_value(&state, &batch.get::<String, _>("id")).await?);
    }
    result["batches"] = json!(output);
    Ok(Json(result))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Json(input): Json<CreateCredential>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let name = name(&input.name)?;
    let metadata = vault::validate_payload(&input.kind, &input.payload).map_err(invalid)?;
    if input.owner_user_id.is_some() && input.kind != "tls_identity" {
        return Err(ApiError::bad_request(
            "only mTLS identities can be user-owned",
        ));
    }
    let mut tx = db::begin_write(&state.pool).await?;
    if let Some(user) = &input.owner_user_id {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1)")
            .bind(user)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            return Err(ApiError::not_found("user"));
        }
    }
    let id = vault::insert(
        &mut tx,
        &state.secrets,
        name,
        &input.kind,
        input.owner_user_id.as_deref(),
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
    let assignments = sqlx::query("SELECT a.node_id,a.user_id,u.revision FROM node_assignments a JOIN users u ON u.id=a.user_id WHERE a.mtls_credential_id=$1 ORDER BY a.user_id,a.node_id").bind(&id).fetch_all(&mut *tx).await?;
    for a in assignments {
        let node: String = a.get("node_id");
        let user: String = a.get("user_id");
        let job = enqueue_job_with_payload_in_tx(&mut tx,"credential-apply",Some(&node),None,json!({"batch_id":batch,"credential_id":id,"version":version,"mode":"mtls","user_id":user})).await?;
        sqlx::query("INSERT INTO credential_batch_items(batch_id,node_id,user_id,expected_revision,job_id) VALUES($1,$2,$3,$4,$5)").bind(&batch).bind(node).bind(user).bind(a.get::<i64,_>("revision")).bind(job).execute(&mut *tx).await?;
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
    let mut connection = state.pool.acquire().await?;
    find_references_on(&mut connection, &state.secrets, id).await
}

async fn find_references_on(
    connection: &mut sqlx::PgConnection,
    secrets: &crate::security::SecretBox,
    id: &str,
) -> Result<Vec<Value>, ApiError> {
    let mut out = Vec::new();
    for row in sqlx::query("SELECT id,name,desired_config_enc,deployed_config_enc,ssh_credential_id,ssh_credential_version FROM nodes").fetch_all(&mut *connection).await? {
        let node:String = row.get("id"); let name:String = row.get("name");
        if row.get::<Option<String>,_>("ssh_credential_id").as_deref()==Some(id) { out.push(json!({"entity_type":"node","entity_id":node,"name":name,"source":"ssh","version":row.get::<Option<i64>,_>("ssh_credential_version")})); }
        for (field,source) in [("desired_config_enc","desired"),("deployed_config_enc","deployed")] {
            if let Some(cipher)=row.get::<Option<String>,_>(field) {
                let value:Value=serde_json::from_str(&secrets.decrypt(&cipher)?).map_err(|_|ApiError::internal())?;
                for (path,r) in vault::references(&value).map_err(invalid)?.into_iter().filter(|(_,r)|r.id==id) { out.push(json!({"entity_type":"node","entity_id":node,"name":name,"source":source,"version":r.version,"field":path})); }
            }
        }
    }
    for row in sqlx::query("SELECT node_id,revision,config_enc FROM config_versions")
        .fetch_all(&mut *connection)
        .await?
    {
        let value: Value =
            serde_json::from_str(&secrets.decrypt(&row.get::<String, _>("config_enc"))?)
                .map_err(|_| ApiError::internal())?;
        for (path, r) in vault::references(&value)
            .map_err(invalid)?
            .into_iter()
            .filter(|(_, r)| r.id == id)
        {
            out.push(json!({"entity_type":"node","entity_id":row.get::<String,_>("node_id"),"source":"history","config_revision":row.get::<i64,_>("revision"),"version":r.version,"field":path}));
        }
    }
    for row in sqlx::query("SELECT user_id,node_id,mtls_credential_version FROM node_assignments WHERE mtls_credential_id=$1").bind(id).fetch_all(&mut *connection).await? { out.push(json!({"entity_type":"user","entity_id":row.get::<String,_>("user_id"),"node_id":row.get::<String,_>("node_id"),"source":"mtls","version":row.get::<i64,_>("mtls_credential_version")})); }
    for row in
        sqlx::query("SELECT DISTINCT b.id,b.version FROM credential_batches b JOIN credential_batch_items i ON i.batch_id=b.id JOIN jobs j ON j.id=i.job_id WHERE b.credential_id=$1 AND j.status IN ('queued','running')")
            .bind(id)
            .fetch_all(&mut *connection)
            .await?
    {
        out.push(json!({"entity_type":"batch","entity_id":row.get::<String,_>("id"),"source":"batch","version":row.get::<i64,_>("version")}));
    }
    Ok(out)
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<super::nodes::RevisionQuery>,
) -> Result<StatusCode, ApiError> {
    // Check references on the same write-locked connection as deletion.
    let mut tx = db::begin_write(&state.pool).await?;
    if !find_references_on(&mut tx, &state.secrets, &id)
        .await?
        .is_empty()
    {
        return Err(ApiError::conflict(
            "credential is referenced; archive it instead",
        ));
    }
    // Terminal batch metadata can be removed once no business/history or active
    // task references remain; the jobs and audit log still retain their IDs.
    sqlx::query("DELETE FROM credential_batches WHERE credential_id=$1")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    let result = sqlx::query("DELETE FROM credentials WHERE id=$1 AND revision=$2")
        .bind(&id)
        .bind(
            query
                .expected_revision
                .ok_or_else(|| ApiError::bad_request("expected_revision is required"))?,
        )
        .execute(&mut *tx)
        .await?;
    if result.rows_affected() != 1 {
        return Err(ApiError::conflict("credential changed or was deleted"));
    }
    record(&mut tx, &actor, "credential.deleted", &id, json!({})).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn batch_value(state: &AppState, id: &str) -> Result<Value, ApiError> {
    let batch = sqlx::query("SELECT * FROM credential_batches WHERE id=$1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| ApiError::not_found("credential batch"))?;
    let items=sqlx::query("SELECT i.node_id,i.user_id,i.job_id,j.status,j.stage,j.error_message,j.result_json,n.name FROM credential_batch_items i JOIN jobs j ON j.id=i.job_id LEFT JOIN nodes n ON n.id=i.node_id WHERE i.batch_id=$1 ORDER BY i.node_id,i.user_id").bind(id).fetch_all(&state.pool).await?;
    Ok(
        json!({"id":id,"credential_id":batch.get::<String,_>("credential_id"),"version":batch.get::<i64,_>("version"),"created_at":batch.get::<DateTime<Utc>,_>("created_at"),"items":items.iter().map(|i|json!({"node_id":i.get::<String,_>("node_id"),"user_id":i.get::<String,_>("user_id"),"job_id":i.get::<String,_>("job_id"),"status":i.get::<String,_>("status"),"stage":i.get::<String,_>("stage"),"error_message":i.get::<Option<String>,_>("error_message"),"name":i.get::<Option<String>,_>("name")})).collect::<Vec<_>>()}),
    )
}
pub async fn batch(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(batch_value(&state, &id).await?))
}

pub async fn retry(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let batch=sqlx::query("SELECT b.*,c.latest_version,c.archived FROM credential_batches b JOIN credentials c ON c.id=b.credential_id WHERE b.id=$1").bind(&id).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::not_found("credential batch"))?;
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
        let mode = if !user.is_empty() {
            "mtls"
        } else if current
            .get::<Option<String>, _>("ssh_credential_id")
            .as_deref()
            == Some(&credential)
        {
            "ssh"
        } else {
            "config"
        };
        let revision = if mode == "mtls" {
            sqlx::query_scalar::<_, i64>("SELECT revision FROM users WHERE id=$1")
                .bind(&user)
                .fetch_one(&mut *tx)
                .await?
        } else {
            current.get("desired_revision")
        };
        let payload = json!({"batch_id":id,"credential_id":credential,"version":batch.get::<i64,_>("version"),"mode":mode,"user_id":user});
        let job =
            enqueue_job_with_payload_in_tx(&mut tx, "credential-apply", Some(&node), None, payload)
                .await?;
        sqlx::query("UPDATE credential_batch_items SET expected_revision=$1,job_id=$2,applied_at=NULL,apply_stage=NULL WHERE batch_id=$3 AND node_id=$4 AND user_id=$5").bind(revision).bind(job).bind(&id).bind(node).bind(user).execute(&mut *tx).await?;
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

async fn record(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    actor: &AdminActor,
    action: &str,
    id: &str,
    detail: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,$2,$3,'credential',$4,$5,$6)").bind(Uuid::new_v4().to_string()).bind(&actor.id).bind(action).bind(id).bind(detail).bind(Utc::now()).execute(&mut **tx).await?;
    Ok(())
}
