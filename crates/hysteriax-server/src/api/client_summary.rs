//! Global counters and notification candidates, independent of client list pages.
use super::*;

pub async fn get(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let now = Utc::now();
    let users = sqlx::query("SELECT count(*)::bigint total, count(*) FILTER (WHERE enabled)::bigint enabled, count(*) FILTER (WHERE expires_at <= $1)::bigint expired, count(*) FILTER (WHERE expires_at > $1 AND expires_at <= $1 + interval '7 days')::bigint expiring, count(*) FILTER (WHERE usage_bytes >= quota_bytes)::bigint depleted FROM users").bind(now).fetch_one(&mut *tx).await?;
    let node_count: i64 = sqlx::query_scalar("SELECT count(*) FROM nodes")
        .fetch_one(&mut *tx)
        .await?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT n.id FROM nodes n LEFT JOIN node_packages p ON p.node_id=n.id WHERE EXISTS(SELECT 1 FROM node_alerts a WHERE a.node_id=n.id AND a.active) OR p.config->>'expires_at' IS NOT NULL AND (p.config->>'expires_at')::timestamptz <= $1 OR (p.config->>'quota_bytes')::bigint IS NOT NULL AND (p.next_reset_at IS NULL OR p.next_reset_at > $1) AND p.usage_bytes >= (p.config->>'quota_bytes')::bigint ORDER BY n.id LIMIT 500").bind(now).fetch_all(&mut *tx).await?;
    let nodes = crate::overview::summary::read_nodes_on(&mut tx, Some(&ids))
        .await?
        .iter()
        .map(|node| node.summary(now))
        .collect::<Result<Vec<_>, _>>()?;
    let rows = sqlx::query("SELECT c.*,v.metadata FROM credentials c LEFT JOIN mtls_authorities a ON a.credential_id=c.id AND c.kind='ca_identity' JOIN credential_versions v ON v.credential_id=c.id AND v.version=coalesce(a.published_version,c.latest_version) WHERE NOT c.archived AND coalesce(v.metadata->>'managed','false')<>'true' AND coalesce(hysteriax_rfc3339(v.metadata->>'expires_at'),c.reminder_at) < $1 + interval '31 days' ORDER BY c.id LIMIT 500").bind(now).fetch_all(&mut *tx).await?;
    let mut by_id = std::collections::HashMap::new();
    let mut order = Vec::new();
    for entry in rows.iter().map(super::credentials::summary) {
        let id = entry["id"].as_str().unwrap_or_default().to_owned();
        if !by_id.contains_key(&id) {
            order.push(id.clone());
        }
        by_id.insert(id, entry);
    }
    for warning in crate::credentials::rotations::maintenance_notifications_on(&mut tx).await? {
        let id = warning["id"].as_str().unwrap_or_default().to_owned();
        if !by_id.contains_key(&id) {
            order.push(id.clone());
        }
        by_id.insert(id, warning);
    }
    for warning in crate::credentials::rotations::notification_credentials_on(&mut tx).await? {
        let id = warning["id"].as_str().unwrap_or_default().to_owned();
        if !by_id.contains_key(&id) {
            order.push(id.clone());
        }
        by_id.insert(id, warning);
    }
    let credentials: Vec<_> = order.into_iter().filter_map(|id| by_id.remove(&id)).collect();
    tx.commit().await?;
    Ok(Json(
        json!({"node_count":node_count,"user_total":users.get::<i64,_>("total"),"user_enabled":users.get::<i64,_>("enabled"),"user_expired":users.get::<i64,_>("expired"),"user_expiring":users.get::<i64,_>("expiring"),"user_depleted":users.get::<i64,_>("depleted"),"notification_nodes":nodes,"notification_credentials":credentials}),
    ))
}
