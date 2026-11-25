//! Global counters and notification candidates, independent of client list pages.
use super::*;
use axum::extract::Query;
use serde::Deserialize;

const DEFAULT_NOTIFICATION_PAGE_SIZE: i64 = 500;
const MAX_NOTIFICATION_PAGE_SIZE: i64 = 500;

#[derive(Deserialize)]
pub struct NotificationPageQuery {
    cursor: Option<String>,
    page_size: Option<i64>,
}

fn parse_page_params(query: &NotificationPageQuery) -> Result<(Option<&str>, i64), ApiError> {
    let page_size = query.page_size.unwrap_or(DEFAULT_NOTIFICATION_PAGE_SIZE);
    if !(1..=MAX_NOTIFICATION_PAGE_SIZE).contains(&page_size) {
        return Err(ApiError::bad_request("page_size must be between 1 and 500"));
    }
    let cursor = query
        .cursor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    Ok((cursor, page_size))
}

async fn notification_nodes_page_on(
    tx: &mut sqlx::PgConnection,
    now: DateTime<Utc>,
    cursor: Option<&str>,
    page_size: i64,
) -> Result<(Vec<Value>, Option<String>), ApiError> {
    let mut ids: Vec<String> = sqlx::query_scalar(
        "SELECT n.id FROM nodes n \
         LEFT JOIN node_packages p ON p.node_id = n.id \
         WHERE ($2::text IS NULL OR n.id > $2) \
           AND (EXISTS(SELECT 1 FROM node_alerts a WHERE a.node_id = n.id AND a.active) \
                OR (p.config->>'expires_at' IS NOT NULL AND (p.config->>'expires_at')::timestamptz <= $1) \
                OR ((p.config->>'quota_bytes')::bigint IS NOT NULL AND (p.next_reset_at IS NULL OR p.next_reset_at > $1) AND p.usage_bytes >= (p.config->>'quota_bytes')::bigint)) \
         ORDER BY n.id LIMIT $3",
    )
    .bind(now)
    .bind(cursor)
    .bind(page_size + 1)
    .fetch_all(&mut *tx)
    .await?;
    let next_cursor = if ids.len() > page_size as usize {
        ids.truncate(page_size as usize);
        ids.last().cloned()
    } else {
        None
    };
    let nodes = crate::overview::summary::read_nodes_on(tx, Some(&ids))
        .await?
        .iter()
        .map(|node| node.summary(now))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((nodes, next_cursor))
}

async fn notification_credentials_page_on(
    tx: &mut sqlx::PgConnection,
    now: DateTime<Utc>,
    cursor: Option<&str>,
    page_size: i64,
) -> Result<(Vec<Value>, Option<String>), ApiError> {
    let mut rows = sqlx::query(
        "SELECT c.*, v.metadata FROM credentials c \
         LEFT JOIN mtls_authorities a ON a.credential_id = c.id AND c.kind = 'ca_identity' \
         JOIN credential_versions v ON v.credential_id = c.id AND v.version = coalesce(a.published_version, c.latest_version) \
         WHERE NOT c.archived \
           AND coalesce(v.metadata->>'managed', 'false') <> 'true' \
           AND coalesce(hysteriax_rfc3339(v.metadata->>'expires_at'), c.reminder_at) < $1 + interval '31 days' \
           AND ($2::text IS NULL OR c.id > $2) \
         ORDER BY c.id LIMIT $3",
    )
    .bind(now)
    .bind(cursor)
    .bind(page_size + 1)
    .fetch_all(&mut *tx)
    .await?;
    let next_cursor = if rows.len() > page_size as usize {
        rows.truncate(page_size as usize);
        rows.last().map(|row| row.get::<String, _>("id"))
    } else {
        None
    };
    let maintenance = crate::credentials::rotations::maintenance_notifications_on(&mut *tx).await?;
    let rotation = crate::credentials::rotations::notification_credentials_on(tx).await?;
    let mut override_ids = std::collections::HashSet::new();
    for warning in maintenance.iter().chain(rotation.iter()) {
        if let Some(id) = warning["id"].as_str() {
            override_ids.insert(id.to_owned());
        }
    }
    let mut by_id = std::collections::HashMap::new();
    let mut order = Vec::new();
    for entry in rows.iter().map(super::credentials::summary) {
        let id = entry["id"].as_str().unwrap_or_default().to_owned();
        if cursor.is_some() && override_ids.contains(&id) {
            continue;
        }
        if !by_id.contains_key(&id) {
            order.push(id.clone());
        }
        by_id.insert(id, entry);
    }
    if cursor.is_none() {
        for warning in maintenance.into_iter().chain(rotation) {
            let id = warning["id"].as_str().unwrap_or_default().to_owned();
            if !by_id.contains_key(&id) {
                order.push(id.clone());
            }
            by_id.insert(id, warning);
        }
    }
    let credentials: Vec<_> = order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect();
    Ok((credentials, next_cursor))
}

pub async fn get(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let now = Utc::now();
    let users = sqlx::query("SELECT count(*)::bigint total, count(*) FILTER (WHERE enabled)::bigint enabled, count(*) FILTER (WHERE expires_at <= $1)::bigint expired, count(*) FILTER (WHERE expires_at > $1 AND expires_at <= $1 + interval '7 days')::bigint expiring, count(*) FILTER (WHERE usage_bytes >= quota_bytes)::bigint depleted FROM users").bind(now).fetch_one(&mut *tx).await?;
    let node_count: i64 = sqlx::query_scalar("SELECT count(*) FROM nodes")
        .fetch_one(&mut *tx)
        .await?;
    let (nodes, nodes_next_cursor) =
        notification_nodes_page_on(&mut tx, now, None, DEFAULT_NOTIFICATION_PAGE_SIZE).await?;
    let (credentials, credentials_next_cursor) =
        notification_credentials_page_on(&mut tx, now, None, DEFAULT_NOTIFICATION_PAGE_SIZE)
            .await?;
    tx.commit().await?;
    Ok(Json(json!({
        "node_count": node_count,
        "user_total": users.get::<i64, _>("total"),
        "user_enabled": users.get::<i64, _>("enabled"),
        "user_expired": users.get::<i64, _>("expired"),
        "user_expiring": users.get::<i64, _>("expiring"),
        "user_depleted": users.get::<i64, _>("depleted"),
        "notification_nodes": nodes,
        "notification_nodes_next_cursor": nodes_next_cursor,
        "notification_credentials": credentials,
        "notification_credentials_next_cursor": credentials_next_cursor,
    })))
}

pub async fn notification_nodes(
    State(state): State<AppState>,
    Query(query): Query<NotificationPageQuery>,
) -> Result<Json<Value>, ApiError> {
    let (cursor, page_size) = parse_page_params(&query)?;
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let now = Utc::now();
    let (items, next_cursor) = notification_nodes_page_on(&mut tx, now, cursor, page_size).await?;
    tx.commit().await?;
    Ok(Json(json!({"items": items, "next_cursor": next_cursor})))
}

pub async fn notification_credentials(
    State(state): State<AppState>,
    Query(query): Query<NotificationPageQuery>,
) -> Result<Json<Value>, ApiError> {
    let (cursor, page_size) = parse_page_params(&query)?;
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let now = Utc::now();
    let (items, next_cursor) =
        notification_credentials_page_on(&mut tx, now, cursor, page_size).await?;
    tx.commit().await?;
    Ok(Json(json!({"items": items, "next_cursor": next_cursor})))
}
