pub(crate) mod job_retries;
pub(crate) mod nodes;
pub(crate) mod resources;
pub(crate) mod subscriptions;
pub(crate) mod users;

use std::{convert::Infallible, time::Duration};

use async_stream::stream;
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::{error::ApiError, security::token_digest, state::AppState};

pub fn router(state: AppState) -> Router {
    let admin = Router::new()
        .route("/api/v1/nodes", get(nodes::list).post(nodes::create))
        .route(
            "/api/v1/nodes/{id}",
            get(nodes::get).patch(nodes::patch).delete(nodes::delete),
        )
        .route(
            "/api/v1/nodes/{id}/usage",
            axum::routing::put(crate::node_limits::update_usage),
        )
        .route("/api/v1/nodes/{id}/deploy", post(nodes::deploy))
        .route("/api/v1/nodes/{id}/ssh-test", post(nodes::ssh_test))
        .route("/api/v1/nodes/{id}/sync", post(nodes::sync))
        .route("/api/v1/nodes/{id}/rollback", post(nodes::rollback))
        .route(
            "/api/v1/nodes/{id}/resources",
            get(resources::list).post(resources::create),
        )
        .route(
            "/api/v1/nodes/{id}/resources/{resource_id}",
            delete(resources::delete),
        )
        .route("/api/v1/users", get(users::list).post(users::create))
        .route(
            "/api/v1/users/{id}",
            get(users::get).patch(users::patch).delete(users::delete),
        )
        .route("/api/v1/users/{id}/assignments", post(users::assign))
        .route(
            "/api/v1/users/{id}/assignments/{node_id}",
            delete(users::unassign).put(users::update_assignment_client_certificate),
        )
        .route(
            "/api/v1/users/{id}/credentials/rotate",
            post(users::rotate_credentials),
        )
        .route(
            "/api/v1/users/{id}/subscription",
            get(subscriptions::get_user).post(subscriptions::rotate),
        )
        .route("/api/v1/users/{id}/usage", get(users::usage))
        .route("/api/v1/users/{id}/quota/reset", post(users::reset_quota))
        .route("/api/v1/jobs", get(list_jobs))
        .route("/api/v1/version", get(api_version))
        .route("/api/v1/overview", get(crate::overview::get))
        .route("/api/v1/overview/history", get(crate::overview::history))
        .route("/api/v1/server/monitoring", get(crate::monitoring::get))
        .route("/api/v1/jobs/{id}", get(get_job))
        .route("/api/v1/jobs/{id}/retry", post(job_retries::retry))
        .route("/api/v1/jobs/{id}/events", get(job_events))
        .route("/api/v1/events", get(events))
        .route("/api/v1/audit", get(list_audit_records))
        .route(
            "/api/v1/admin/tokens",
            get(list_admin_tokens).post(create_admin_token),
        )
        .route("/api/v1/admin/tokens/{id}", delete(revoke_admin_token))
        .layer(DefaultBodyLimit::max(30 * 1024 * 1024))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin));

    Router::new()
        .merge(admin)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/openapi.yaml", get(openapi))
        .route("/sub/{token}", get(subscriptions::download_auto))
        .route("/sub/{token}/clash.yaml", get(subscriptions::download))
        .route(
            "/hy2/auth/{node_id}/{node_token}",
            post(subscriptions::hy2_auth),
        )
        .with_state(state)
        .layer(
            tower_http::trace::TraceLayer::new_for_http().make_span_with(
                |request: &axum::http::Request<_>| {
                    let path = request.uri().path();
                    let path = if path.starts_with("/sub/") {
                        "/sub/:token"
                    } else if path.starts_with("/hy2/auth/") {
                        "/hy2/auth/:node_id/:node_token"
                    } else {
                        path
                    };
                    tracing::info_span!("http.request", method = %request.method(), %path)
                },
            ),
        )
}

async fn require_admin(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(token) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "administrator token required",
        )
        .into_response();
    };

    let digest = token_digest(token);
    let hashes = match sqlx::query_scalar::<_, String>(
        "SELECT token_hash FROM admin_tokens WHERE revoked_at IS NULL",
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(hashes) => hashes,
        Err(error) => {
            tracing::error!(%error, "failed to verify administrator token");
            return ApiError::internal().into_response();
        }
    };
    let authorized = hashes.iter().any(|known| {
        known.len() == digest.len() && bool::from(known.as_bytes().ct_eq(digest.as_bytes()))
    });
    if !authorized {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "administrator token is invalid",
        )
        .into_response();
    }
    next.run(request).await
}

async fn healthz() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn api_version() -> Json<Value> {
    Json(json!({
        "api_version": "1.0.0",
        "features": ["node_packages", "overview_monitoring", "job_retry_links"],
        "service_version": env!("CARGO_PKG_VERSION"),
        "hysteria_version": "app/v2.12.3",
        "mihomo_version": "v1.19.31"
    }))
}

async fn readyz(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    sqlx::query_scalar::<_, i64>("SELECT 1::BIGINT")
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(json!({"status": "ready", "database": "ok"})))
}

async fn openapi() -> Response {
    let mut response = Response::new(Body::from(include_str!("../../../../openapi/openapi.yaml")));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/yaml; charset=utf-8".parse().unwrap(),
    );
    response
}

#[derive(Deserialize, Default)]
struct EventsQuery {
    after: Option<i64>,
    job_id: Option<String>,
}

async fn events(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    stream_events(state, event_cursor(query.after, &headers), query.job_id)
}

async fn job_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE id = $1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;
    if exists == 0 {
        return Err(ApiError::not_found("job"));
    }
    Ok(stream_events(
        state,
        event_cursor(query.after, &headers),
        Some(id),
    ))
}

fn event_cursor(after: Option<i64>, headers: &HeaderMap) -> i64 {
    after
        .or_else(|| {
            headers
                .get("last-event-id")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<i64>().ok())
        })
        .filter(|cursor| *cursor >= 0)
        .unwrap_or(0)
}

fn stream_events(
    state: AppState,
    mut cursor: i64,
    job_id: Option<String>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let output = stream! {
        loop {
            let rows = if let Some(job_id) = &job_id {
                sqlx::query("SELECT id, event_type, payload_json FROM job_events WHERE id > $1 AND job_id = $2 ORDER BY id LIMIT 100")
                    .bind(cursor).bind(job_id).fetch_all(&state.pool).await
            } else {
                sqlx::query("SELECT id, event_type, payload_json FROM job_events WHERE id > $1 ORDER BY id LIMIT 100")
                    .bind(cursor).fetch_all(&state.pool).await
            };
            match rows {
                Ok(rows) => {
                    for row in rows {
                        let id: i64 = row.get("id");
                        let event_type: String = row.get("event_type");
                        let payload: Value = row.get("payload_json");
                        cursor = id;
                        yield Ok(Event::default().id(id.to_string()).event(event_type).data(payload.to_string()));
                    }
                }
                Err(error) => tracing::error!(%error, "failed to load persisted SSE events"),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    Sse::new(output).keep_alive(axum::response::sse::KeepAlive::default())
}

#[derive(Serialize)]
struct JobSummary {
    id: String,
    kind: String,
    node_id: Option<String>,
    node_name: Option<String>,
    target_revision: Option<i64>,
    status: String,
    stage: String,
    error_message: Option<String>,
    result: Option<Value>,
    attempts: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    retry_of_job_id: Option<String>,
    retry_job_id: Option<String>,
}

async fn list_jobs(State(state): State<AppState>) -> Result<Json<Vec<JobSummary>>, ApiError> {
    let rows = sqlx::query("SELECT j.*, (SELECT child.id FROM jobs child WHERE child.retry_of_job_id=j.id) retry_job_id FROM jobs j ORDER BY created_at DESC LIMIT 200")
        .fetch_all(&state.pool)
        .await?;
    Ok(Json(rows.iter().map(job_summary).collect()))
}

pub(crate) async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT j.*, (SELECT child.id FROM jobs child WHERE child.retry_of_job_id=j.id) retry_job_id FROM jobs j WHERE j.id = $1")
        .bind(&id).fetch_optional(&state.pool).await?.ok_or_else(|| ApiError::not_found("job"))?;
    let summary = job_summary(&row);
    let result: Option<Value> = row.get("result_json");
    let logs: Value = row.get("logs_json");
    Ok(Json(
        json!({"job": summary, "result": result, "logs": logs, "started_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("started_at")}),
    ))
}

fn job_summary(row: &sqlx::postgres::PgRow) -> JobSummary {
    JobSummary {
        id: row.get("id"),
        kind: row.get("kind"),
        node_id: row.get("node_id"),
        node_name: row.get("node_name"),
        target_revision: row.get("target_revision"),
        status: row.get("status"),
        stage: row.get("stage"),
        error_message: row.get("error_message"),
        result: row.get::<Option<Value>, _>("result_json"),
        attempts: row.get("attempts"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        finished_at: row.get("finished_at"),
        retry_of_job_id: row.get("retry_of_job_id"),
        retry_job_id: row.try_get("retry_job_id").unwrap_or(None),
    }
}

async fn list_admin_tokens(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let rows = sqlx::query("SELECT id, label, created_at, last_used_at, revoked_at FROM admin_tokens ORDER BY created_at")
        .fetch_all(&state.pool).await?;
    Ok(Json(rows.iter().map(|row| json!({
        "id": row.get::<String, _>("id"), "label": row.get::<String, _>("label"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"), "last_used_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_used_at"),
        "revoked_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("revoked_at")
    })).collect()))
}

async fn list_audit_records(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let rows = sqlx::query("SELECT id, actor, action, entity_type, entity_id, detail_json, created_at FROM audit_records ORDER BY created_at DESC LIMIT 500")
        .fetch_all(&state.pool).await?;
    Ok(Json(rows.iter().map(|row| json!({
        "id": row.get::<String, _>("id"), "actor": row.get::<String, _>("actor"),
        "action": row.get::<String, _>("action"), "entity_type": row.get::<String, _>("entity_type"),
        "entity_id": row.get::<String, _>("entity_id"),
        "detail": row.get::<Value, _>("detail_json"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at")
    })).collect()))
}

#[derive(Deserialize)]
struct CreateAdminToken {
    label: String,
}

async fn create_admin_token(
    State(state): State<AppState>,
    Json(input): Json<CreateAdminToken>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let label = validate_admin_token_label(&input.label)?;
    let token = generate_token();
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO admin_tokens (id, token_hash, label, created_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(&id)
    .bind(token_digest(&token))
    .bind(label)
    .bind(now())
    .execute(&state.pool)
    .await?;
    audit(
        &state.pool,
        "admin_token.created",
        "admin_token",
        &id,
        json!({"label": label}),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id": id, "label": label, "token": token})),
    ))
}

fn validate_admin_token_label(label: &str) -> Result<&str, ApiError> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > 100 {
        return Err(ApiError::bad_request(
            "label must contain 1 to 100 characters",
        ));
    }
    Ok(label)
}

async fn revoke_admin_token(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let result =
        sqlx::query("UPDATE admin_tokens SET revoked_at = $1 WHERE id = $2 AND revoked_at IS NULL")
            .bind(now())
            .bind(&id)
            .execute(&state.pool)
            .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("active administrator token"));
    }
    audit(
        &state.pool,
        "admin_token.revoked",
        "admin_token",
        &id,
        json!({}),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn audit(
    pool: &PgPool,
    action: &str,
    entity_type: &str,
    entity_id: &str,
    detail: Value,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO audit_records (id, actor, action, entity_type, entity_id, detail_json, created_at) VALUES ($1, 'admin', $2, $3, $4, $5, $6)")
        .bind(Uuid::new_v4().to_string()).bind(action).bind(entity_type).bind(entity_id)
        .bind(detail).bind(now()).execute(pool).await?;
    Ok(())
}

pub(crate) async fn enqueue_job_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    node_id: Option<&str>,
    revision: Option<i64>,
) -> Result<String, ApiError> {
    enqueue_job_with_payload_in_tx(tx, kind, node_id, revision, json!({})).await
}

pub(crate) async fn enqueue_job_with_payload_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    kind: &str,
    node_id: Option<&str>,
    revision: Option<i64>,
    job_payload: Value,
) -> Result<String, ApiError> {
    let id = Uuid::new_v4().to_string();
    let timestamp = now();
    let node_name: Option<String> = if let Some(node_id) = node_id {
        Some(
            sqlx::query_scalar("SELECT name FROM nodes WHERE id = $1")
                .bind(node_id)
                .fetch_optional(&mut **tx)
                .await?
                .ok_or_else(|| ApiError::not_found("node"))?,
        )
    } else {
        None
    };
    let payload = json!({"id": id, "kind": kind, "node_id": node_id, "node_name": node_name, "target_revision": revision, "status": "queued", "stage": "queued"});
    sqlx::query("INSERT INTO jobs (id, kind, node_id, node_name, target_revision, payload_json, status, stage, available_at, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, 'queued', 'queued', $7, $8, $9)")
        .bind(&id).bind(kind).bind(node_id).bind(node_name).bind(revision).bind(job_payload).bind(timestamp).bind(timestamp).bind(timestamp).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ($1, 'job.queued', $2, $3)")
        .bind(&id).bind(payload).bind(timestamp).execute(&mut **tx).await?;
    Ok(id)
}

pub(crate) async fn supersede_queued_syncs_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<(), ApiError> {
    let rows = sqlx::query(
        "SELECT id FROM jobs WHERE node_id = $1 AND kind = 'sync' AND status = 'queued'",
    )
    .bind(node_id)
    .fetch_all(&mut **tx)
    .await?;
    for row in rows {
        let id: String = row.get("id");
        let timestamp = now();
        sqlx::query("UPDATE jobs SET status = 'cancelled', stage = 'superseded', updated_at = $1, finished_at = $2 WHERE id = $3 AND status = 'queued'")
            .bind(timestamp).bind(timestamp).bind(&id).execute(&mut **tx).await?;
        let payload =
            json!({"id": id, "status": "cancelled", "stage": "superseded", "node_id": node_id});
        sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ($1, 'job.superseded', $2, $3)")
            .bind(&id).bind(payload).bind(timestamp).execute(&mut **tx).await?;
    }
    Ok(())
}

pub(crate) async fn cancel_queued_node_jobs_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<(), ApiError> {
    let rows = sqlx::query("SELECT id, kind FROM jobs WHERE node_id = $1 AND status = 'queued'")
        .bind(node_id)
        .fetch_all(&mut **tx)
        .await?;
    for row in rows {
        let id: String = row.get("id");
        let kind: String = row.get("kind");
        let timestamp = now();
        sqlx::query("UPDATE jobs SET status = 'cancelled', stage = 'node_deleting', updated_at = $1, finished_at = $2 WHERE id = $3 AND status = 'queued'")
            .bind(timestamp).bind(timestamp).bind(&id).execute(&mut **tx).await?;
        let payload = json!({"id": id, "kind": kind, "node_id": node_id, "status": "cancelled", "stage": "node_deleting"});
        sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ($1, 'job.cancelled', $2, $3)")
            .bind(&id).bind(payload).bind(timestamp).execute(&mut **tx).await?;
    }
    Ok(())
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn generate_token() -> String {
    format!(
        "hx_{}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use sqlx::Row;

    use super::{enqueue_job_in_tx, event_cursor, job_summary, validate_admin_token_label};

    #[tokio::test]
    async fn job_node_snapshot_survives_node_rename_and_deletion() {
        let pool = crate::db::test_pool().await;
        sqlx::query("INSERT INTO nodes (id, name, ssh_host, ssh_port, ssh_username, ssh_auth_type, ssh_secret_enc, public_host, public_port, listen_addr, node_token_hash, node_token_enc, traffic_stats_secret_enc, desired_config_enc, created_at, updated_at) VALUES ('snapshot-node', 'Original node', '127.0.0.1', 22, 'root', 'private_key', 'ssh-secret', 'node.example.test', 443, ':443', 'node-hash', 'node-token', 'stats-secret', '{}', now(), now())")
            .execute(&pool).await.unwrap();

        let mut tx = crate::db::begin_write(&pool).await.unwrap();
        let id = enqueue_job_in_tx(&mut tx, "sync", Some("snapshot-node"), Some(1))
            .await
            .unwrap();
        let global_id = enqueue_job_in_tx(&mut tx, "global", None, None)
            .await
            .unwrap();
        assert!(
            enqueue_job_in_tx(&mut tx, "sync", Some("missing-node"), Some(1))
                .await
                .is_err()
        );
        tx.commit().await.unwrap();

        sqlx::query("UPDATE nodes SET name = 'Renamed node' WHERE id = 'snapshot-node'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM nodes WHERE id = 'snapshot-node'")
            .execute(&pool)
            .await
            .unwrap();

        let row = sqlx::query("SELECT * FROM jobs WHERE id = $1")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        let summary = serde_json::to_value(job_summary(&row)).unwrap();
        assert_eq!(summary["node_id"], "snapshot-node");
        assert_eq!(summary["node_name"], "Original node");
        let payload: serde_json::Value =
            sqlx::query_scalar("SELECT payload_json FROM job_events WHERE job_id = $1")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(payload["node_name"], "Original node");

        let global = sqlx::query("SELECT node_id, node_name FROM jobs WHERE id = $1")
            .bind(&global_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(global.get::<Option<String>, _>("node_id").is_none());
        assert!(global.get::<Option<String>, _>("node_name").is_none());
    }

    #[test]
    fn admin_token_labels_count_unicode_characters_not_utf8_bytes() {
        let hundred_characters = "令牌".repeat(50);
        assert_eq!(
            validate_admin_token_label(&hundred_characters).unwrap(),
            hundred_characters
        );
        assert!(validate_admin_token_label(&format!("{hundred_characters}令牌")).is_err());
        assert!(validate_admin_token_label("   ").is_err());
    }

    #[test]
    fn sse_reconnect_uses_last_event_id_and_query_cursor_takes_precedence() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", HeaderValue::from_static("42"));
        assert_eq!(event_cursor(None, &headers), 42);
        assert_eq!(event_cursor(Some(7), &headers), 7);

        headers.insert("last-event-id", HeaderValue::from_static("invalid"));
        assert_eq!(event_cursor(None, &headers), 0);
        assert_eq!(event_cursor(Some(-1), &headers), 0);
    }
}
