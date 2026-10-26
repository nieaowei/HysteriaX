mod admin_tokens;
pub(crate) mod authorization_groups;
mod client_summary;
pub(crate) mod credentials;
pub(crate) mod dns;
mod event_stream_v2;
mod execution_rechecks;
mod idempotency;
pub(crate) mod job_retries;
pub(crate) mod managed_mtls;
pub(crate) mod nodes;
pub(crate) mod resources;
pub(crate) mod ssh_fingerprint;
pub(crate) mod subscriptions;
pub(crate) mod users;

use std::{convert::Infallible, time::Duration};

use async_stream::stream;
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Extension, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{db, error::ApiError, security::token_digest, state::AppState};

#[derive(Clone)]
pub(crate) struct AdminActor {
    pub id: String,
}

tokio::task_local! {static AUDIT_ACTOR_ID:String;}
pub(crate) fn audit_actor() -> String {
    AUDIT_ACTOR_ID
        .try_with(Clone::clone)
        .unwrap_or_else(|_| "admin".into())
}

pub fn router(state: AppState) -> Router {
    use subscriptions::templates as st;
    let admin = Router::new()
        .route("/api/v2/version", get(api_version))
        .route("/api/v2/client/summary", get(client_summary::get))
        .route("/api/v2/overview", get(crate::overview::get))
        .route("/api/v2/overview/nodes", get(crate::overview::list_nodes))
        .route("/api/v2/overview/history", get(crate::overview::history))
        .route("/api/v2/server/monitoring", get(crate::monitoring::get))
        .route("/api/v2/audit", get(list_audit_records))
        .route("/api/v2/cache", get(cache_stats))
        .route("/api/v2/events/head", get(event_stream_v2::head))
        .route("/api/v2/events", get(event_stream_v2::events))
        .route("/api/v2/jobs/{id}/events", get(event_stream_v2::job_events))
        .route(
            "/api/v2/execution-operations",
            get(execution_rechecks::list),
        )
        .route(
            "/api/v2/execution-operations/{id}/reverify",
            post(execution_rechecks::verify),
        )
        .route("/api/v2/jobs", get(list_jobs))
        .route("/api/v2/jobs/{id}", get(get_job))
        .route("/api/v2/jobs/{id}/retry", post(job_retries::v2::retry))
        .route(
            "/api/v2/jobs/{id}/confirm-fingerprint",
            post(job_retries::v2::confirm_fingerprint),
        )
        .route(
            "/api/v2/subscription-templates",
            get(st::list).post(st::v2::create),
        )
        .route("/api/v2/subscription-templates/fields", get(st::fields))
        .route(
            "/api/v2/subscription-templates/source",
            post(st::v2::compute_source),
        )
        .route(
            "/api/v2/subscription-templates/import",
            post(st::v2::compute_source),
        )
        .route(
            "/api/v2/subscription-templates/{id}",
            get(st::get).patch(st::v2::patch),
        )
        .route(
            "/api/v2/subscription-templates/{id}/draft",
            get(st::draft).put(st::v2::save),
        )
        .route(
            "/api/v2/subscription-templates/{id}/validate",
            post(st::v2::compute_template),
        )
        .route(
            "/api/v2/subscription-templates/{id}/refactor",
            post(st::v2::compute_template),
        )
        .route(
            "/api/v2/subscription-templates/{id}/preview",
            post(st::v2::compute_template),
        )
        .route(
            "/api/v2/subscription-templates/{id}/publish",
            post(st::v2::publication),
        )
        .route(
            "/api/v2/subscription-templates/{id}/rollback",
            post(st::v2::publication),
        )
        .route(
            "/api/v2/subscription-templates/{id}/versions",
            get(st::versions),
        )
        .route(
            "/api/v2/subscription-templates/{id}/versions/{version}",
            get(st::version),
        )
        .route(
            "/api/v2/subscription-templates/{id}/export",
            get(st::export),
        )
        .route(
            "/api/v2/authorization-groups",
            get(authorization_groups::list).post(authorization_groups::v2::create),
        )
        .route(
            "/api/v2/authorization-groups/preview",
            post(authorization_groups::v2::preview_create),
        )
        .route(
            "/api/v2/authorization-groups/{id}",
            get(authorization_groups::get)
                .put(authorization_groups::v2::change)
                .delete(authorization_groups::v2::change),
        )
        .route(
            "/api/v2/authorization-groups/{id}/preview",
            post(authorization_groups::v2::preview_group),
        )
        .route(
            "/api/v2/users/{id}/authorization-groups/preview",
            post(authorization_groups::v2::preview_memberships),
        )
        .route(
            "/api/v2/users/{id}/authorization-groups",
            axum::routing::put(authorization_groups::v2::update_memberships),
        )
        .route(
            "/api/v2/credentials/ca-identities",
            post(managed_mtls::v2::create),
        )
        .route(
            "/api/v2/credentials/{id}/ca-versions",
            post(managed_mtls::v2::publish),
        )
        .route(
            "/api/v2/credentials/{id}/ca-policy",
            axum::routing::patch(managed_mtls::v2::update_policy),
        )
        .route(
            "/api/v2/users/{id}/managed-certificates/reissue",
            post(managed_mtls::v2::reissue_user),
        )
        .route(
            "/api/v2/dns/records",
            get(dns::list_records).post(dns::v2::create_record),
        )
        .route(
            "/api/v2/dns/records/{id}",
            get(dns::get_record)
                .patch(dns::v2::record_action)
                .delete(dns::v2::record_action),
        )
        .route(
            "/api/v2/dns/records/{id}/check",
            post(dns::v2::record_action),
        )
        .route(
            "/api/v2/nodes/{id}/dns-binding",
            get(dns::get_binding)
                .put(dns::v2::set_binding)
                .delete(dns::v2::unbind),
        )
        .route(
            "/api/v2/dns/connections",
            get(dns::list_connections).post(dns::v2::create_connection),
        )
        .route(
            "/api/v2/dns/connections/{id}",
            get(dns::get_connection)
                .patch(dns::v2::patch_connection)
                .delete(dns::v2::delete_connection),
        )
        .route(
            "/api/v2/dns/connections/{id}/verify",
            post(dns::v2::connection_action),
        )
        .route(
            "/api/v2/dns/connections/{id}/refresh",
            post(dns::v2::connection_action),
        )
        .route("/api/v2/dns/zones", get(dns::list_zones))
        .route(
            "/api/v2/dns/zones/{id}",
            axum::routing::patch(dns::v2::patch_zone),
        )
        .route(
            "/api/v2/dns/zones/{id}/refresh",
            post(dns::v2::refresh_zone),
        )
        .route("/api/v2/nodes", get(nodes::list).post(nodes::create_v2))
        .route(
            "/api/v2/nodes/{id}",
            get(nodes::get)
                .patch(nodes::patch_v2)
                .delete(nodes::delete_v2),
        )
        .route("/api/v2/nodes/{id}/record", delete(nodes::remove_record_v2))
        .route("/api/v2/nodes/{id}/deploy", post(nodes::action_v2))
        .route("/api/v2/nodes/{id}/sync", post(nodes::action_v2))
        .route("/api/v2/nodes/{id}/rollback", post(nodes::action_v2))
        .route("/api/v2/nodes/{id}/ssh-test", post(nodes::action_v2))
        .route(
            "/api/v2/nodes/{id}/usage",
            axum::routing::put(nodes::update_usage_v2),
        )
        .route(
            "/api/v2/credentials",
            get(credentials::list).post(credentials::create_v2),
        )
        .route(
            "/api/v2/credentials/{id}",
            get(credentials::get)
                .patch(credentials::patch_v2)
                .delete(credentials::delete_v2),
        )
        .route(
            "/api/v2/credentials/{id}/versions",
            post(credentials::publish_v2),
        )
        .route(
            "/api/v2/credentials/{id}/references",
            get(credentials::references),
        )
        .route("/api/v2/credential-batches/{id}", get(credentials::batch))
        .route(
            "/api/v2/credential-batches/{id}/retry",
            post(credentials::retry_v2),
        )
        .route(
            "/api/v2/admin/tokens",
            get(list_admin_tokens).post(admin_tokens::create),
        )
        .route("/api/v2/admin/tokens/{id}", delete(admin_tokens::revoke))
        .route("/api/v2/users", get(users::list).post(users::create_v2))
        .route(
            "/api/v2/nodes/{id}/resources",
            get(resources::list).post(resources::create_v2),
        )
        .route(
            "/api/v2/nodes/{id}/resources/{resource_id}",
            delete(resources::delete_v2),
        )
        .route(
            "/api/v2/users/{id}",
            get(users::get)
                .patch(users::patch_v2)
                .delete(users::delete_v2),
        )
        .route("/api/v2/users/{id}/usage", get(users::usage))
        .route(
            "/api/v2/users/{id}/subscription",
            get(subscriptions::get_user).post(subscriptions::rotate_v2),
        )
        .route(
            "/api/v2/users/{id}/credentials/rotate",
            post(users::rotate_credentials_v2),
        )
        .route(
            "/api/v2/users/{id}/quota/reset",
            post(users::reset_quota_v2),
        )
        .layer(DefaultBodyLimit::max(30 * 1024 * 1024))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::cache::middleware,
        ))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin));

    Router::new()
        .merge(admin)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/openapi.yaml", get(openapi))
        .route("/sub/{token}", get(subscriptions::download_auto))
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
    mut request: axum::extract::Request,
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
    let actor = match sqlx::query(
        "SELECT id,(last_used_at IS NULL OR last_used_at<=clock_timestamp()-interval '60 seconds') AS usage_due FROM admin_tokens WHERE token_hash=$1 AND revoked_at IS NULL",
    )
    .bind(&digest)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            return ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "administrator token is invalid",
            )
            .into_response();
        }
        Err(error) => return ApiError::from(error).into_response(),
    };
    let id: String = actor.get("id");
    if actor.get::<bool, _>("usage_due") {
        // Optional metadata does not delay authentication or cancel a database
        // transaction on a deadline shorter than its network round trips.
        token_usage::schedule(&state, &id, &digest);
    }
    request
        .extensions_mut()
        .insert(AdminActor { id: id.clone() });
    AUDIT_ACTOR_ID.scope(id, next.run(request)).await
}

mod token_usage;

async fn cache_stats(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"caches":state.caches.stats(),"events":state.events.stats()}))
}

async fn healthz() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn api_version(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
) -> Result<Json<Value>, ApiError> {
    let directory_version = crate::cache_domains::version(&state.pool, &["directory"]).await?;
    Ok(Json(json!({
        "api_version": "2.0.0",
        "management_protocol":crate::bootstrap::MANAGEMENT_PROTOCOL,
        "features": ["idempotent_management", "publication_cursors", "subscription_templates", "job_fingerprint_confirmation", "ssh_test_confirmation", "ssh_dns_sync", "client_summary", "node_packages", "overview_monitoring", "job_retry_links", "credentials", "node_record_removal", "dns_management", "authorization_groups", "managed_mtls"],
        "directory_version":directory_version,
        "current_admin_token_id": actor.id,
        "service_version": env!("CARGO_PKG_VERSION"),
        "hysteria_version": "app/v2.12.3",
        "mihomo_version": "v1.19.31"
    })))
}

async fn readyz(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let started = tokio::time::Instant::now();
    let check = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM runtime_contract WHERE id AND schema_version=$1 AND management_protocol=$2 AND master_key_fingerprint=$3)",
    )
    .bind(db::MIGRATOR.iter().last().unwrap().version)
    .bind(crate::bootstrap::MANAGEMENT_PROTOCOL)
    .bind(state.secrets.fingerprint())
    .fetch_one(&state.pool);
    let (ready, failure_reason) = match tokio::time::timeout(Duration::from_secs(5), check).await {
        Ok(Ok(true)) => (true, "ready"),
        Ok(Ok(false)) => (false, "runtime_contract_mismatch"),
        Ok(Err(_)) => (false, "query_failed"),
        Err(_) => (false, "timeout"),
    };
    if !ready {
        let pool_size = u64::from(state.pool.size());
        let pool_idle = state.pool.num_idle() as u64;
        tracing::warn!(
            reason = failure_reason,
            pool_size,
            pool_idle,
            pool_acquired = pool_size.saturating_sub(pool_idle),
            check_ms = started.elapsed().as_millis() as u64,
            "readiness database check failed"
        );
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_not_ready",
            "database or runtime protocol is not ready",
        ));
    }
    Ok(Json(
        json!({"status": "ready", "database": "ok", "management_protocol":crate::bootstrap::MANAGEMENT_PROTOCOL}),
    ))
}

async fn openapi(headers: HeaderMap) -> Response {
    use sha2::Digest;
    const SPEC: &str = include_str!("../../../../openapi/openapi.yaml");
    let etag = format!("\"{}\"", hex::encode(sha2::Sha256::digest(SPEC.as_bytes())));
    let matched = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|values| {
            values
                .split(',')
                .any(|v| v.trim() == etag || v.trim() == "*")
        });
    let mut response = if matched {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Response::new(Body::from(SPEC))
    };
    response
        .headers_mut()
        .insert(header::ETAG, etag.parse().unwrap());
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "public, max-age=300, must-revalidate".parse().unwrap(),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "application/yaml; charset=utf-8".parse().unwrap(),
    );
    response
}

#[derive(Serialize)]
struct JobSummary {
    id: String,
    kind: String,
    node_id: Option<String>,
    node_name: Option<String>,
    resource_type: Option<String>,
    resource_id: Option<String>,
    resource_name: Option<String>,
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

#[derive(Default, Deserialize)]
struct JobsQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
}

#[derive(Serialize)]
struct JobsPage {
    items: Vec<JobSummary>,
    total: i64,
    page: i64,
    page_size: i64,
}

impl JobsQuery {
    fn validate(&self) -> Result<(i64, i64, &str, &str), ApiError> {
        let page = self.page.unwrap_or(1);
        let page_size = self.page_size.unwrap_or(50);
        if page < 1 || !(1..=200).contains(&page_size) {
            return Err(ApiError::bad_request(
                "page must be positive and page_size must be between 1 and 200",
            ));
        }
        let sort = match self.sort.as_deref().unwrap_or("created_at") {
            "created_at" => "j.created_at",
            "node" => "COALESCE(j.resource_name, j.node_name, j.node_id, '')",
            "kind" => "j.kind",
            "stage" => "j.stage",
            "status" => "j.status",
            _ => return Err(ApiError::bad_request("invalid job sort field")),
        };
        let order = match self.order.as_deref().unwrap_or("desc") {
            "asc" => "ASC",
            "desc" => "DESC",
            _ => return Err(ApiError::bad_request("invalid job sort order")),
        };
        Ok((page, page_size, sort, order))
    }
}

async fn list_jobs(
    State(state): State<AppState>,
    Query(query): Query<JobsQuery>,
) -> Result<Json<JobsPage>, ApiError> {
    let (requested_page, page_size, sort, order) = query.validate()?;
    let search = query.q.as_deref().unwrap_or("").trim();
    let pattern = literal_search_pattern(search);
    // A literal substring search: '%' and '_' must not become SQL wildcards.
    let filter = "($1 = '' OR hysteriax_search_text(j.id,j.kind,j.stage,j.status,j.node_name,j.node_id,j.resource_name,j.resource_id,j.error_message) LIKE lower($1) ESCAPE '\\')";
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    sqlx::query("SET LOCAL plan_cache_mode=force_custom_plan")
        .execute(&mut *tx)
        .await?;
    let total: i64 = if search.is_empty() {
        sqlx::query_scalar("SELECT coalesce(sum(total),0)::bigint FROM job_status_totals")
            .fetch_one(&mut *tx)
            .await?
    } else {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM jobs j WHERE {filter}"
        )))
        .bind(&pattern)
        .fetch_one(&mut *tx)
        .await?
    };
    let page_count = ((total + page_size - 1) / page_size).max(1);
    let page = requested_page.min(page_count);
    // Only validated SQL fragments are interpolated; all user values are bound.
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT j.*, (SELECT child.id FROM jobs child WHERE child.retry_of_job_id=j.id) retry_job_id FROM jobs j WHERE {filter} ORDER BY {sort} {order}, j.id {order} LIMIT $2 OFFSET $3")))
        .bind(&pattern).bind(page_size).bind((page - 1) * page_size)
        .fetch_all(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(JobsPage {
        items: rows.iter().map(job_summary).collect(),
        total,
        page,
        page_size,
    }))
}

#[cfg(test)]
mod jobs_pagination_tests;

pub(crate) async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT j.*, (SELECT child.id FROM jobs child WHERE child.retry_of_job_id=j.id) retry_job_id FROM jobs j WHERE j.id = $1")
        .bind(&id).fetch_optional(&mut *crate::read_snapshot::begin(&state.pool).await?).await?.ok_or_else(|| ApiError::not_found("job"))?;
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
        resource_type: row.get("resource_type"),
        resource_id: row.get("resource_id"),
        resource_name: row.get("resource_name"),
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

#[derive(Default, Deserialize)]
struct AuditQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    q: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    action_matches: Option<String>,
    entity_type_matches: Option<String>,
    actor_matches: Option<String>,
}

impl AuditQuery {
    fn validate(&self) -> Result<(i64, i64, &str, &str), ApiError> {
        let page = self.page.unwrap_or(1);
        let size = self.page_size.unwrap_or(50);
        if page < 1 || !(1..=200).contains(&size) {
            return Err(ApiError::bad_request(
                "page must be positive and page_size must be between 1 and 200",
            ));
        }
        let sort = match self.sort.as_deref().unwrap_or("created_at") {
            "created_at" => "a.created_at",
            "action" => "a.action",
            "entity_type" => "a.entity_type",
            "entity_id" => "a.entity_id",
            "actor" => "a.actor",
            _ => return Err(ApiError::bad_request("invalid audit sort field")),
        };
        let order = match self.order.as_deref().unwrap_or("desc") {
            "asc" => "ASC",
            "desc" => "DESC",
            _ => return Err(ApiError::bad_request("invalid audit sort order")),
        };
        Ok((page, size, sort, order))
    }
}

/// Escape LIKE metacharacters while preserving the existing literal substring search.
pub(crate) fn literal_search_pattern(search: &str) -> String {
    if search.is_empty() {
        return String::new();
    }
    format!(
        "%{}%",
        search
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

fn audit_matches(value: Option<&str>) -> Vec<&str> {
    value
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect()
}

async fn list_audit_records(
    State(state): State<AppState>,
    Query(query): Query<AuditQuery>,
) -> Result<Json<Value>, ApiError> {
    let (requested_page, page_size, sort, order) = query.validate()?;
    let search = query.q.as_deref().unwrap_or("").trim();
    let pattern = literal_search_pattern(search);
    let actions = audit_matches(query.action_matches.as_deref());
    let entity_types = audit_matches(query.entity_type_matches.as_deref());
    let actors = audit_matches(query.actor_matches.as_deref());
    // The client resolves localized display labels to raw codes. All values remain bound.
    let filter = "($1 = '' OR hysteriax_search_text(a.id,a.action,a.actor,a.entity_type,a.entity_id) LIKE lower($1) ESCAPE '\\' OR a.action = ANY($2) OR a.entity_type = ANY($3) OR a.actor = ANY($4))";
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    sqlx::query("SET LOCAL plan_cache_mode=force_custom_plan")
        .execute(&mut *tx)
        .await?;
    let total: i64 = if search.is_empty() {
        sqlx::query_scalar("SELECT total FROM audit_totals WHERE id")
            .fetch_one(&mut *tx)
            .await?
    } else {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM audit_records a WHERE {filter}"
        )))
        .bind(&pattern)
        .bind(&actions)
        .bind(&entity_types)
        .bind(&actors)
        .fetch_one(&mut *tx)
        .await?
    };
    let page = requested_page.min(((total + page_size - 1) / page_size).max(1));
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT a.* FROM audit_records a WHERE {filter} ORDER BY {sort} {order}, a.id {order} LIMIT $5 OFFSET $6")))
        .bind(&pattern).bind(&actions).bind(&entity_types).bind(&actors).bind(page_size).bind((page - 1) * page_size)
        .fetch_all(&mut *tx).await?;
    tx.commit().await?;
    let items: Vec<Value> = rows.iter().map(|row| json!({
        "id": row.get::<String, _>("id"), "actor": row.get::<String, _>("actor"),
        "action": row.get::<String, _>("action"), "entity_type": row.get::<String, _>("entity_type"),
        "entity_id": row.get::<String, _>("entity_id"),
        "detail": row.get::<Value, _>("detail_json"),
        "created_at": row.get::<DateTime<Utc>, _>("created_at")
    })).collect();
    Ok(Json(
        json!({ "items": items, "total": total, "page": page, "page_size": page_size }),
    ))
}

#[cfg(test)]
mod audit_pagination_tests;

#[derive(Deserialize)]
struct CreateAdminToken {
    label: String,
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

pub(crate) async fn audit_on(
    connection: &mut sqlx::PgConnection,
    actor: &str,
    action: &str,
    entity_type: &str,
    entity_id: &str,
    detail: Value,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(Uuid::new_v4().to_string()).bind(actor).bind(action).bind(entity_type).bind(entity_id).bind(detail).bind(now()).execute(connection).await?;
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
    if ssh_fingerprint::supports_confirmation(kind)
        && let Some(node) = node_id
    {
        ssh_fingerprint::cancel_waiting(tx, node).await?;
    }
    let job_payload = if kind == "kick" {
        let node = node_id.ok_or_else(|| ApiError::bad_request("kick requires node_id"))?;
        let (existing, payload) = crate::kick_requests::prepare(tx, node, &job_payload).await?;
        if let Some(id) = existing {
            return Ok(id);
        }
        payload
    } else {
        job_payload
    };
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
        .bind(&id).bind(kind).bind(node_id).bind(node_name).bind(revision).bind(&job_payload).bind(timestamp).bind(timestamp).bind(timestamp).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO job_events (job_id, event_type, payload_json, created_at) VALUES ($1, 'job.queued', $2, $3)")
        .bind(&id).bind(payload).bind(timestamp).execute(&mut **tx).await?;
    if kind == "kick" {
        crate::kick_requests::attach(
            tx,
            node_id.unwrap(),
            job_payload["user_id"].as_str().unwrap(),
            &id,
        )
        .await?;
    }
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
    let rows = sqlx::query("SELECT id, kind FROM jobs WHERE node_id = $1 AND status IN ('queued', 'waiting_confirmation')")
        .bind(node_id)
        .fetch_all(&mut **tx)
        .await?;
    for row in rows {
        let id: String = row.get("id");
        let kind: String = row.get("kind");
        let timestamp = now();
        sqlx::query("UPDATE jobs SET status = 'cancelled', stage = 'node_deleting', updated_at = $1, finished_at = $2 WHERE id = $3 AND status IN ('queued', 'waiting_confirmation')")
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
    use sqlx::Row;

    use super::{enqueue_job_in_tx, job_summary, validate_admin_token_label};

    #[tokio::test]
    async fn job_node_snapshot_survives_node_rename_and_deletion() {
        let pool = crate::db::test_pool().await;
        sqlx::query("INSERT INTO nodes (id, name, ssh_host, ssh_port, ssh_username, ssh_auth_type, ssh_secret_enc, public_host, public_port, listen_addr, node_token_hash, node_token_enc, traffic_stats_secret_enc, desired_config_enc, created_at, updated_at) VALUES ('snapshot-node', 'Original node', '127.0.0.1', 22, 'root', 'private_key', 'ssh-secret', 'node.example.test', 443, ':443', 'node-hash', 'node-token', 'stats-secret', '{}', now(), now())")
            .execute(&pool).await.unwrap();

        let mut tx = pool.begin().await.unwrap();
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

    #[tokio::test]
    async fn collection_routes_always_paginate_without_page_suffix() {
        use super::{AppState, Duration, StatusCode, Value, json, router, token_digest};

        let pool = crate::db::test_pool().await;
        let state = AppState::new(
            pool,
            crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .unwrap(),
        );
        let token = "collection-route-test";
        sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('admin',$1,'Test',now())")
            .bind(token_digest(token)).execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) SELECT 'user-'||i,'User '||i,true,now(),now() FROM generate_series(1,65) i")
            .execute(&state.pool).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(state);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        for path in ["nodes", "users", "dns/records", "credentials"] {
            let url = format!("{base}/api/v2/{path}");
            let all = client.get(&url).bearer_auth(token).send().await.unwrap();
            assert_eq!(all.status(), StatusCode::OK);
            let all: Value = all.json().await.unwrap();
            assert!(
                all.is_object(),
                "collection responses always use the page envelope"
            );
            assert_eq!(all["page"], 1);
            assert_eq!(all["page_size"], 50);
            let total = all["total"].as_u64().unwrap() as usize;
            assert!(all["items"].as_array().unwrap().len() <= 50);
            if path == "users" {
                assert_eq!(total, 65);
                assert_eq!(all["items"].as_array().unwrap().len(), 50);
            }
            for query in ["page=1", "page_size=25", "page=2&page_size=25"] {
                let response = client
                    .get(format!("{url}?{query}"))
                    .bearer_auth(token)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let page: Value = response.json().await.unwrap();
                assert_eq!(page["total"], total);
                let size = if query == "page=1" { 50 } else { 25 };
                assert_eq!(page["page_size"], size);
                assert!(page["items"].as_array().unwrap().len() <= size);
                if path == "users" && query == "page=2&page_size=25" {
                    assert_eq!(page["page"], 2);
                    assert_eq!(page["items"].as_array().unwrap().len(), 25);
                }
            }
            for query in ["page=0", "page_size=201", "sort=invalid"] {
                assert_eq!(
                    client
                        .get(format!("{url}?{query}"))
                        .bearer_auth(token)
                        .send()
                        .await
                        .unwrap()
                        .status(),
                    StatusCode::BAD_REQUEST
                );
            }
            assert_eq!(
                client
                    .get(format!("{url}/page?page=1"))
                    .bearer_auth(token)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                client
                    .get(format!("{url}?page=1"))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        let filtered = client
            .get(format!("{base}/api/v2/dns/records?zone_id=missing"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(filtered.status(), StatusCode::OK);
        let filtered: Value = filtered.json().await.unwrap();
        assert_eq!(filtered["items"], json!([]));
        assert_eq!(filtered["total"], 0);
        let searched: Value = client
            .get(format!("{base}/api/v2/users?q=User%2065"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(searched["total"], 1);
        assert_eq!(searched["items"][0]["id"], "user-65");
        server.abort();
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
}

#[cfg(test)]
mod backend_cache_tests;

#[cfg(test)]
mod backend_cache_v2_tests;

#[cfg(test)]
mod query_optimizer_tests;

#[cfg(test)]
mod auth_contention_tests;

#[cfg(test)]
mod management_transaction_tests;

// Direct domain fixtures are test-only; production writes use protocol-2 handlers.
#[cfg(test)]
#[path = "test_support.rs"]
mod test_support;
#[cfg(test)]
use test_support::*;
