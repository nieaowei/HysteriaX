use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    api::{enqueue_job_with_payload_in_tx, now},
    error::ApiError,
    state::AppState,
};

#[derive(Deserialize)]
pub struct CreateUser {
    name: String,
    #[serde(default = "default_true")]
    enabled: bool,
    expires_at: Option<String>,
    quota_bytes: Option<i64>,
}

#[derive(Clone, Deserialize)]
pub struct PatchUser {
    expected_revision: i64,
    name: Option<String>,
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    expires_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    quota_bytes: Option<Option<i64>>,
}

#[derive(Deserialize)]
pub struct RevisionRequest {
    expected_revision: i64,
}

#[derive(Default, Deserialize)]
pub(crate) struct UsersListQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
}

impl UsersListQuery {
    fn validate(&self) -> Result<(i64, i64, &str, &str), ApiError> {
        let page = self.page.unwrap_or(1);
        let size = self.page_size.unwrap_or(50);
        if page < 1 || !(1..=200).contains(&size) {
            return Err(ApiError::bad_request(
                "page must be positive and page_size must be between 1 and 200",
            ));
        }
        let sort = match self.sort.as_deref().unwrap_or("name") {
            "name" => "lower(u.name) COLLATE \"C\", u.name COLLATE \"C\"",
            "created_at" => "u.created_at",
            _ => return Err(ApiError::bad_request("invalid user sort field")),
        };
        let order = match self.order.as_deref().unwrap_or("asc") {
            "asc" => "ASC",
            "desc" => "DESC",
            _ => return Err(ApiError::bad_request("invalid user sort order")),
        };
        Ok((page, size, sort, order))
    }
}

pub(crate) async fn list(
    State(state): State<AppState>,
    Query(query): Query<UsersListQuery>,
) -> Result<Json<Value>, ApiError> {
    let (requested_page, page_size, sort, order) = query.validate()?;
    let search = query.q.as_deref().unwrap_or("").trim();
    let filter = "($1 = '' OR strpos(lower(u.name), lower($1)) > 0 OR strpos(lower(u.id), lower($1)) > 0 OR EXISTS (SELECT 1 FROM node_assignments na WHERE na.user_id=u.id AND strpos(lower(na.node_id), lower($1)) > 0))";
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM users u WHERE {filter}"
    )))
    .bind(search)
    .fetch_one(&mut *tx)
    .await?;
    let page = requested_page.min(((total + page_size - 1) / page_size).max(1));
    // Apply the direction to every name component, then use ID to break ties.
    let order_by = if query.sort.as_deref().unwrap_or("name") == "name" {
        format!("lower(u.name) COLLATE \"C\" {order}, u.name COLLATE \"C\" {order}, u.id {order}")
    } else {
        format!("{sort} {order}, u.id {order}")
    };
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT u.* FROM users u WHERE {filter} ORDER BY {order_by} LIMIT $2 OFFSET $3"
    )))
    .bind(search)
    .bind(page_size)
    .bind((page - 1) * page_size)
    .fetch_all(&mut *tx)
    .await?;
    let items = users_json_on(&mut tx, &rows).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"items": items, "total": total, "page": page, "page_size": page_size}),
    ))
}

#[cfg(test)]
#[path = "users_pagination_tests.rs"]
mod pagination_tests;

pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT * FROM users WHERE id = $1")
        .bind(&id)
        .fetch_optional(&mut *crate::read_snapshot::begin(&state.pool).await?)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    let mut result = user_json(&state, &row).await?;
    let identities = super::managed_mtls::user_identities(&state, &id).await?;
    if !identities.is_empty() {
        result["managed_certificates"] = json!(identities);
    }
    Ok(Json(result))
}

mod v2;
pub(super) use v2::{
    create as create_v2, delete as delete_v2, patch as patch_v2, reset_quota as reset_quota_v2,
    rotate_credentials as rotate_credentials_v2,
};

#[derive(Clone)]
struct PreparedUser {
    id: String,
    name: String,
    enabled: bool,
    expires_at: Option<DateTime<Utc>>,
    quota_bytes: Option<i64>,
    timestamp: DateTime<Utc>,
    credential: crate::connection_credentials::Credential,
}

impl PreparedUser {
    fn new(input: CreateUser, secrets: &crate::security::SecretBox) -> Result<Self, ApiError> {
        validate_name(&input.name)?;
        validate_quota(input.quota_bytes)?;
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            name: input.name.trim().into(),
            enabled: input.enabled,
            expires_at: normalize_expiry(input.expires_at)?,
            quota_bytes: input.quota_bytes,
            timestamp: now(),
            credential: crate::connection_credentials::Credential::new(secrets)?,
        })
    }

    async fn commit_on(&self, tx: &mut sqlx::PgConnection, actor: &str) -> Result<Value, ApiError> {
        sqlx::query("INSERT INTO users(id,name,enabled,expires_at,quota_bytes,usage_bytes,revision,created_at,updated_at,connection_credential_hash,connection_credential_enc) VALUES($1,$2,$3,$4,$5,0,1,$6,$6,$7,$8)")
            .bind(&self.id).bind(&self.name).bind(self.enabled).bind(self.expires_at)
            .bind(self.quota_bytes).bind(self.timestamp).bind(&self.credential.hash).bind(&self.credential.ciphertext).execute(&mut *tx).await?;
        super::audit_on(
            tx,
            actor,
            "user.created",
            "user",
            &self.id,
            json!({"name":self.name}),
        )
        .await?;
        Ok(json!({"id":self.id,"name":self.name,"enabled":self.enabled,"revision":1}))
    }
}

async fn patch_on(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    id: String,
    input: PatchUser,
    actor: String,
) -> Result<Value, ApiError> {
    if input.expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let current = lock_user_graph_on(tx, &id, false).await?;
    let revision: i64 = current.get("revision");
    if revision != input.expected_revision {
        return Err(ApiError::conflict(format!(
            "user revision is {revision}; reload before editing"
        )));
    }
    let name = input.name.unwrap_or_else(|| current.get("name"));
    let enabled = input
        .enabled
        .unwrap_or_else(|| current.get::<bool, _>("enabled"));
    let expires_at = match input.expires_at {
        Some(Some(value)) => normalize_expiry(Some(value))?,
        Some(None) => None,
        None => current.get("expires_at"),
    };
    let quota_bytes = input
        .quota_bytes
        .unwrap_or_else(|| current.get("quota_bytes"));
    validate_name(&name)?;
    validate_quota(quota_bytes)?;
    validate_expiry(expires_at.as_ref())?;
    let next_revision = revision + 1;
    let timestamp = now();
    let was_enabled = current.get::<bool, _>("enabled");
    let usage_bytes: i64 = current.get("usage_bytes");
    let expired_now = expires_at.is_some_and(|expiration| expiration <= Utc::now());
    let over_quota = quota_bytes.is_some_and(|limit| limit <= usage_bytes);
    let access_restricted = (was_enabled && !enabled) || expired_now || over_quota;
    let previous_marker: Option<DateTime<Utc>> = current.get("access_kick_enqueued_at");
    let kick_marker = if access_restricted {
        Some(timestamp)
    } else if enabled && !expired_now && !over_quota {
        None
    } else {
        previous_marker
    };
    let result = sqlx::query("UPDATE users SET name = $1, enabled = $2, expires_at = $3, quota_bytes = $4, access_kick_enqueued_at = $5, revision = $6, updated_at = $7 WHERE id = $8 AND revision = $9")
        .bind(name.trim()).bind(enabled).bind(expires_at).bind(quota_bytes)
        .bind(kick_marker).bind(next_revision).bind(timestamp).bind(&id).bind(revision).execute(&mut **tx).await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "user changed while saving; reload before editing",
        ));
    }
    if access_restricted {
        let node_ids = sqlx::query_scalar::<_, String>(
            "SELECT node_id FROM node_assignments WHERE user_id = $1 ORDER BY node_id",
        )
        .bind(&id)
        .fetch_all(&mut **tx)
        .await?;
        for node_id in node_ids {
            enqueue_job_with_payload_in_tx(
                tx,
                "kick",
                Some(&node_id),
                None,
                json!({"user_id": id,"kick_reason":"access_restricted"}),
            )
            .await?;
        }
    }
    if enabled && !expired_now && !over_quota {
        crate::kick_requests::clear_reason(tx, None, Some(&id), "access_restricted").await?;
    }
    super::audit_on(
        tx,
        &actor,
        "user.updated",
        "user",
        &id,
        json!({"revision":next_revision,"enabled":enabled}),
    )
    .await?;
    Ok(
        json!({"id": id, "name": name.trim(), "enabled": enabled, "expires_at": expires_at, "quota_bytes": quota_bytes, "revision": next_revision}),
    )
}

async fn delete_on(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    id: String,
    expected: i64,
    actor: String,
) -> Result<StatusCode, ApiError> {
    let row = lock_user_graph_on(tx, &id, true).await?;
    let revision: i64 = row.get("revision");
    if revision != expected {
        return Err(ApiError::conflict(
            "user revision changed; reload before deleting",
        ));
    }
    sqlx::query("UPDATE authorization_groups SET revision=revision+1,updated_at=$2 WHERE id IN (SELECT group_id FROM authorization_group_users WHERE user_id=$1)")
        .bind(&id)
        .bind(now())
        .execute(&mut **tx)
        .await?;
    let node_ids = sqlx::query_scalar::<_, String>(
        "SELECT node_id FROM node_assignments WHERE user_id = $1 ORDER BY node_id",
    )
    .bind(&id)
    .fetch_all(&mut **tx)
    .await?;
    for node_id in node_ids {
        enqueue_job_with_payload_in_tx(
            tx,
            "kick",
            Some(&node_id),
            None,
            json!({"user_id": id,"kick_reason":"user_deleted"}),
        )
        .await?;
    }
    sqlx::query("SELECT id FROM credentials WHERE owner_user_id=$1 ORDER BY id FOR UPDATE")
        .bind(&id)
        .fetch_all(&mut **tx)
        .await?;
    sqlx::query("SELECT id FROM mtls_user_identities WHERE user_id=$1 ORDER BY id FOR UPDATE")
        .bind(&id)
        .fetch_all(&mut **tx)
        .await?;
    sqlx::query("UPDATE credentials SET archived=TRUE,revision=revision+1,updated_at=$1 WHERE owner_user_id=$2 AND archived=FALSE").bind(now()).bind(&id).execute(&mut **tx).await?;
    sqlx::query("UPDATE mtls_user_identities SET archived=TRUE,next_action_at=NULL,revision=revision+1,authorization_revision=authorization_revision+1,updated_at=$2 WHERE user_id=$1").bind(&id).bind(now()).execute(&mut **tx).await?;
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(&id)
        .execute(&mut **tx)
        .await?;
    super::audit_on(
        tx,
        &actor,
        "user.deleted",
        "user",
        &id,
        json!({"revision":expected}),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

mod rotation;

/// Match billing's node-before-user order. Deletion additionally owns the
/// affected authorization groups before touching their membership cascades.
async fn lock_user_graph_on(
    tx: &mut sqlx::PgConnection,
    user: &str,
    groups: bool,
) -> Result<sqlx::postgres::PgRow, ApiError> {
    if groups {
        sqlx::query("SELECT g.id FROM authorization_groups g JOIN authorization_group_users m ON m.group_id=g.id WHERE m.user_id=$1 ORDER BY g.id FOR UPDATE OF g")
            .bind(user).fetch_all(&mut *tx).await?;
    }
    sqlx::query("SELECT n.id FROM nodes n JOIN node_assignments a ON a.node_id=n.id WHERE a.user_id=$1 ORDER BY n.id FOR NO KEY UPDATE OF n")
        .bind(user).fetch_all(&mut *tx).await?;
    sqlx::query("SELECT * FROM users WHERE id=$1 FOR NO KEY UPDATE")
        .bind(user)
        .fetch_optional(tx)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))
}

pub async fn usage(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = sqlx::query(
        "SELECT id, name, enabled, expires_at, usage_bytes, quota_bytes, quota_reset_at FROM users WHERE id = $1",
    )
    .bind(&user_id)
    .fetch_optional(&mut *crate::read_snapshot::begin(&state.pool).await?)
    .await?
    .ok_or_else(|| ApiError::not_found("user"))?;
    let rows = sqlx::query(
        "WITH user_nodes AS (
            SELECT node_id FROM node_assignments WHERE user_id=$1
            UNION SELECT node_id FROM user_node_usage WHERE user_id=$1
            UNION SELECT node_id FROM traffic_baselines WHERE user_id=$1
        ) SELECT n.node_id,coalesce(t.tx,0)::bigint tx,coalesce(t.rx,0)::bigint rx,
            coalesce(t.sampled_at,b.sampled_at) sampled_at,
            EXISTS(SELECT 1 FROM node_assignments a WHERE a.user_id=$1 AND a.node_id=n.node_id) assigned
        FROM user_nodes n LEFT JOIN user_node_usage t ON t.node_id=n.node_id AND t.user_id=$1
        LEFT JOIN traffic_baselines b ON b.node_id=n.node_id AND b.user_id=$1 ORDER BY n.node_id",
    ).bind(&user_id)
    .fetch_all(&mut *crate::read_snapshot::begin(&state.pool).await?)
    .await?;
    let by_node: Vec<Value> = rows.iter().map(|row| json!({
        "node_id": row.get::<String, _>("node_id"), "tx_bytes": row.get::<Option<i64>, _>("tx").unwrap_or(0),
        "rx_bytes": row.get::<Option<i64>, _>("rx").unwrap_or(0), "sampled_at": row.get::<Option<DateTime<Utc>>, _>("sampled_at"),
        "assigned": row.get::<bool, _>("assigned")
    })).collect();
    let assigned_samples: Vec<Option<DateTime<Utc>>> = rows
        .iter()
        .filter(|row| row.get::<bool, _>("assigned"))
        .map(|row| row.get("sampled_at"))
        .collect();
    let parsed_samples: Vec<DateTime<Utc>> = assigned_samples
        .iter()
        .filter_map(|sample| *sample)
        .collect();
    let freshness = parsed_samples
        .iter()
        .max()
        .map(|sample| sample.to_rfc3339());
    let now_utc = Utc::now();
    let freshness_status = if parsed_samples.is_empty() {
        "not_collected"
    } else if assigned_samples
        .iter()
        .any(|sample| sample.is_none_or(|sample| (now_utc - sample).num_seconds() > 30))
    {
        "stale"
    } else {
        "fresh"
    };
    let gap_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM data_gaps g JOIN node_assignments a ON a.node_id = g.node_id WHERE a.user_id = $1 AND g.resolved_at IS NULL")
        .bind(&user_id).fetch_one(&mut *crate::read_snapshot::begin(&state.pool).await?).await?;
    let is_restricted = !user.get::<bool, _>("enabled")
        || user
            .get::<Option<DateTime<Utc>>, _>("expires_at")
            .is_some_and(|expiration| expiration <= Utc::now())
        || user
            .get::<Option<i64>, _>("quota_bytes")
            .is_some_and(|quota| user.get::<i64, _>("usage_bytes") >= quota);
    let pending_rows = sqlx::query("SELECT j.id,j.node_id,j.status,j.stage,j.updated_at FROM kick_requests r JOIN jobs j ON j.id=r.latest_job_id WHERE r.user_id=$2 AND r.state NOT IN ('completed','cancelled') AND j.kind='kick' AND (j.status IN ('queued','running') OR (j.status='failed' AND $1)) ORDER BY j.created_at")
        .bind(is_restricted)
        .bind(&user_id)
        .fetch_all(&mut *crate::read_snapshot::begin(&state.pool).await?)
        .await?;
    let pending_revocations: Vec<Value> = pending_rows
        .iter()
        .map(|row| {
            json!({
                "job_id": row.get::<String, _>("id"),
                "node_id": row.get::<String, _>("node_id"),
                "status": row.get::<String, _>("status"),
                "stage": row.get::<String, _>("stage"),
                "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at")
            })
        })
        .collect();
    Ok(Json(json!({
        "user_id": user.get::<String, _>("id"), "name": user.get::<String, _>("name"),
        "usage_bytes": user.get::<i64, _>("usage_bytes"), "quota_bytes": user.get::<Option<i64>, _>("quota_bytes"),
        "quota_reset_at": user.get::<Option<chrono::DateTime<chrono::Utc>>, _>("quota_reset_at"), "by_node": by_node,
        "data_freshness": {"status": freshness_status, "last_sample_at": freshness, "open_gaps": gap_count},
        "pending_revocations": pending_revocations
    })))
}

async fn reset_quota_on(
    tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
    user_id: String,
    expected: i64,
    actor: String,
) -> Result<Value, ApiError> {
    if expected < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let current = lock_user_graph_on(tx, &user_id, false).await?;
    if current.get::<i64, _>("revision") != expected {
        return Err(ApiError::conflict(
            "user revision changed; reload before resetting quota",
        ));
    }
    let timestamp: DateTime<Utc> = sqlx::query_scalar("UPDATE users SET usage_bytes=0,quota_reset_at=clock_timestamp(),access_kick_enqueued_at=NULL,revision=revision+1,updated_at=clock_timestamp() WHERE id=$1 AND revision=$2 RETURNING quota_reset_at")
        .bind(&user_id).bind(expected).fetch_one(&mut **tx).await?;
    let unrestricted: bool = sqlx::query_scalar("SELECT enabled AND (expires_at IS NULL OR expires_at>clock_timestamp()) AND (quota_bytes IS NULL OR usage_bytes<quota_bytes) FROM users WHERE id=$1")
        .bind(&user_id).fetch_one(&mut **tx).await?;
    if unrestricted {
        crate::kick_requests::clear_reason(tx, None, Some(&user_id), "access_restricted").await?;
    }
    super::audit_on(
        tx,
        &actor,
        "user.quota_reset",
        "user",
        &user_id,
        json!({"quota_reset_at":timestamp}),
    )
    .await?;
    Ok(json!({"user_id":user_id,"usage_bytes":0,"quota_reset_at":timestamp,"revision":expected+1}))
}

async fn user_json(state: &AppState, row: &sqlx::postgres::PgRow) -> Result<Value, ApiError> {
    let mut connection = crate::read_snapshot::begin(&state.pool).await?;
    user_json_on(&mut connection, row).await
}

async fn user_json_on(
    connection: &mut sqlx::PgConnection,
    row: &sqlx::postgres::PgRow,
) -> Result<Value, ApiError> {
    users_json_on(connection, std::slice::from_ref(row))
        .await?
        .into_iter()
        .next()
        .ok_or_else(ApiError::internal)
}
async fn users_json_on(
    connection: &mut sqlx::PgConnection,
    rows: &[sqlx::postgres::PgRow],
) -> Result<Vec<Value>, ApiError> {
    use std::collections::BTreeMap;
    if rows.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<String> = rows.iter().map(|row| row.get("id")).collect();
    let assignments=sqlx::query("SELECT user_id,node_id,created_at FROM node_assignments WHERE user_id=ANY($1) ORDER BY user_id,node_id").bind(&ids).fetch_all(&mut *connection).await?;
    let sources=sqlx::query("SELECT gu.user_id,gn.node_id,g.id,g.name FROM authorization_group_users gu JOIN authorization_group_nodes gn ON gn.group_id=gu.group_id JOIN authorization_groups g ON g.id=gu.group_id WHERE gu.user_id=ANY($1) ORDER BY lower(g.name) COLLATE \"C\",g.name COLLATE \"C\",g.id").bind(&ids).fetch_all(&mut *connection).await?;
    let memberships=sqlx::query("SELECT gu.user_id,g.id,g.name,g.revision FROM authorization_group_users gu JOIN authorization_groups g ON g.id=gu.group_id WHERE gu.user_id=ANY($1) ORDER BY lower(g.name) COLLATE \"C\",g.name COLLATE \"C\",g.id").bind(&ids).fetch_all(&mut *connection).await?;
    let mut source_map = BTreeMap::<(String, String), Vec<Value>>::new();
    for row in sources {
        source_map
            .entry((row.get("user_id"), row.get("node_id")))
            .or_default()
            .push(json!({"id":row.get::<String,_>("id"),"name":row.get::<String,_>("name")}));
    }
    let mut identity_refs =
        crate::credentials::authorization::assignment_refs_on(connection, &ids).await?;
    let mut assigned = BTreeMap::<String, Vec<Value>>::new();
    for row in assignments {
        let user: String = row.get("user_id");
        let node: String = row.get("node_id");
        let sources = source_map
            .remove(&(user.clone(), node.clone()))
            .unwrap_or_default();
        let identities = identity_refs
            .remove(&(user.clone(), node.clone()))
            .unwrap_or_default();
        assigned.entry(user).or_default().push(json!({"node_id":node,"created_at":row.get::<DateTime<Utc>,_>("created_at"),"managed_identity_ids":identities,"source_groups":sources}));
    }
    let mut groups = BTreeMap::<String, Vec<Value>>::new();
    for row in memberships {
        groups.entry(row.get("user_id")).or_default().push(json!({"id":row.get::<String,_>("id"),"name":row.get::<String,_>("name"),"revision":row.get::<i64,_>("revision")}));
    }
    Ok(rows.iter().map(|row|{
        let id:String=row.get("id");let assignments=assigned.remove(&id).unwrap_or_default();let groups=groups.remove(&id).unwrap_or_default();
        json!({"id":id,"name":row.get::<String,_>("name"),"enabled":row.get::<bool,_>("enabled"),
            "expires_at":row.get::<Option<DateTime<Utc>>,_>("expires_at"),"quota_bytes":row.get::<Option<i64>,_>("quota_bytes"),"usage_bytes":row.get::<i64,_>("usage_bytes"),
            "revision":row.get::<i64,_>("revision"),"quota_reset_at":row.get::<Option<DateTime<Utc>>,_>("quota_reset_at"),
            "assignments":assignments,"authorization_groups":groups,"created_at":row.get::<DateTime<Utc>,_>("created_at"),"updated_at":row.get::<DateTime<Utc>,_>("updated_at")})
    }).collect())
}

fn validate_name(value: &str) -> Result<(), ApiError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 100 || value.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "name must contain 1 to 100 printable characters",
        ));
    }
    Ok(())
}

fn validate_quota(value: Option<i64>) -> Result<(), ApiError> {
    if value.is_some_and(|quota| quota < 0) {
        return Err(ApiError::bad_request("quota_bytes cannot be negative"));
    }
    Ok(())
}

fn validate_expiry(value: Option<&DateTime<Utc>>) -> Result<(), ApiError> {
    let _ = value;
    Ok(())
}

fn normalize_expiry(value: Option<String>) -> Result<Option<DateTime<Utc>>, ApiError> {
    value
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|parsed| parsed.with_timezone(&Utc))
                .map_err(|_| ApiError::bad_request("expires_at must be an RFC 3339 timestamp"))
        })
        .transpose()
}

fn default_true() -> bool {
    true
}

fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use axum::{Json, extract::Path};
    use serde_json::Value;

    use crate::{security::SecretBox, state::AppState};

    use super::{RevisionRequest, reset_quota};

    #[tokio::test]
    async fn quota_reset_starts_a_new_usage_period_and_checks_revision() {
        let pool = crate::db::test_pool().await;
        sqlx::query("INSERT INTO users (id, name, enabled, quota_bytes, usage_bytes, revision, access_kick_enqueued_at, created_at, updated_at) VALUES ('user-id', 'Quota user', TRUE, 1000, 750, 2, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();

        let state = AppState::new(
            pool.clone(),
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
        );
        let Json(response): Json<Value> = reset_quota(
            axum::extract::State(state.clone()),
            Path("user-id".to_owned()),
            Json(RevisionRequest {
                expected_revision: 2,
            }),
        )
        .await
        .unwrap();
        assert_eq!(response["usage_bytes"], 0);
        assert_eq!(response["revision"], 3);

        let (usage, revision, marker): (i64, i64, Option<String>) = sqlx::query_as(
            "SELECT usage_bytes, revision, access_kick_enqueued_at FROM users WHERE id = 'user-id'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((usage, revision, marker), (0, 3, None));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM audit_records WHERE action = 'user.quota_reset'",
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
        assert!(
            reset_quota(
                axum::extract::State(state),
                Path("user-id".to_owned()),
                Json(RevisionRequest {
                    expected_revision: 2,
                }),
            )
            .await
            .is_err()
        );
    }
}

#[cfg(test)]
mod transaction_tests;

// Direct domain fixtures are test-only; production writes use protocol-2 handlers.
#[cfg(test)]
#[path = "users_test_support.rs"]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::*;

#[cfg(test)]
use crate::db;
