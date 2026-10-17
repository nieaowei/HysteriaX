//! Global counters and notification candidates, independent of client list pages.
use super::*;

pub async fn get(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let now = Utc::now();
    let users = sqlx::query("SELECT count(*)::bigint total, count(*) FILTER (WHERE enabled)::bigint enabled, count(*) FILTER (WHERE expires_at <= $1)::bigint expired, count(*) FILTER (WHERE expires_at > $1 AND expires_at <= $1 + interval '7 days')::bigint expiring, count(*) FILTER (WHERE usage_bytes >= quota_bytes)::bigint depleted FROM users").bind(now).fetch_one(&mut *tx).await?;
    let node_count: i64 = sqlx::query_scalar("SELECT count(*) FROM nodes")
        .fetch_one(&mut *tx)
        .await?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT n.id FROM nodes n LEFT JOIN node_packages p ON p.node_id=n.id WHERE EXISTS(SELECT 1 FROM node_alerts a WHERE a.node_id=n.id AND a.active) OR p.config->>'expires_at' IS NOT NULL AND (p.config->>'expires_at')::timestamptz <= $1 OR (p.config->>'quota_bytes')::bigint IS NOT NULL AND (p.next_reset_at IS NULL OR p.next_reset_at > $1) AND p.usage_bytes >= (p.config->>'quota_bytes')::bigint ORDER BY n.id").bind(now).fetch_all(&mut *tx).await?;
    let nodes = crate::overview::summary::read_nodes_on(&mut tx, Some(&ids))
        .await?
        .iter()
        .map(|node| node.summary(now))
        .collect::<Result<Vec<_>, _>>()?;
    let rows = sqlx::query("SELECT c.*,v.metadata FROM credentials c LEFT JOIN mtls_authorities a ON a.credential_id=c.id AND c.kind='ca_identity' JOIN credential_versions v ON v.credential_id=c.id AND v.version=coalesce(a.published_version,c.latest_version) WHERE NOT c.archived AND coalesce(v.metadata->>'managed','false')<>'true' AND coalesce(hysteriax_rfc3339(v.metadata->>'expires_at'),c.reminder_at) < $1 + interval '31 days' ORDER BY c.id").bind(now).fetch_all(&mut *tx).await?;
    let mut credentials: Vec<_> = rows.iter().map(super::credentials::summary).collect();
    for warning in crate::credentials::rotations::maintenance_notifications_on(&mut tx).await? {
        credentials.retain(|entry| entry["id"] != warning["id"]);
        credentials.push(warning);
    }
    let rotation_warnings =
        crate::credentials::rotations::notification_credentials_on(&mut tx).await?;
    for warning in rotation_warnings {
        credentials.retain(|entry| entry["id"] != warning["id"]);
        credentials.push(warning);
    }
    tx.commit().await?;
    Ok(Json(
        json!({"node_count":node_count,"user_total":users.get::<i64,_>("total"),"user_enabled":users.get::<i64,_>("enabled"),"user_expired":users.get::<i64,_>("expired"),"user_expiring":users.get::<i64,_>("expiring"),"user_depleted":users.get::<i64,_>("depleted"),"notification_nodes":nodes,"notification_credentials":credentials}),
    ))
}
