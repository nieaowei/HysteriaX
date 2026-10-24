use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    pub fn not_found(resource: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("{resource} not found"),
        )
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "revision_conflict", message)
    }

    fn pool_busy() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "database_pool_busy",
            "database capacity is temporarily unavailable; retry the request",
        )
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "request failed",
        )
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: ErrorDetail<'a>,
}

#[derive(Serialize)]
struct ErrorDetail<'a> {
    code: &'a str,
    message: &'a str,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: ErrorDetail {
                    code: self.code,
                    message: &self.message,
                },
            }),
        )
            .into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        if matches!(error, sqlx::Error::PoolTimedOut) {
            tracing::warn!("database pool capacity unavailable");
            return Self::pool_busy();
        }
        if error.as_database_error().is_some_and(|e| {
            e.code().as_deref() == Some("23505") && e.constraint() == Some("mtls_operations_pkey")
        }) {
            return Self::new(
                StatusCode::CONFLICT,
                "mtls_operation_committed",
                "a concurrent request committed this managed identity operation",
            );
        }
        if error.as_database_error().is_some_and(|e| {
            e.code().as_deref() == Some("23505")
                && e.constraint() == Some("dns_bindings_hostname_key")
        }) {
            return Self::conflict("hostname is already bound to another node");
        }
        if error.as_database_error().is_some_and(|e| {
            e.code().as_deref() == Some("23505")
                && e.constraint() == Some("dns_operations_idempotency_key_key")
        }) {
            return Self::new(
                StatusCode::CONFLICT,
                "dns_operation_committed",
                "a concurrent request already committed this DNS operation",
            );
        }
        if error.as_database_error().is_some_and(|e| {
            e.code().as_deref() == Some("23503")
                && e.constraint() == Some("resource_reference_resource")
        }) {
            return Self::conflict(
                "configuration resource is referenced or no longer available; reload before retrying",
            );
        }
        if error.as_database_error().is_some_and(|e| {
            e.code().as_deref() == Some("23503")
                && e.constraint()
                    .is_some_and(|name| name.starts_with("reference_edge_"))
        }) {
            return Self::conflict(
                "credential version is referenced or no longer available; reload before retrying",
            );
        }
        tracing::error!(error = %error, "database operation failed");
        if crate::db::RetryableTransactionError::retryable_transaction(&error) {
            return Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "transaction_retry_exhausted",
                "concurrent database changes; retry the request",
            );
        }
        Self::internal()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        if error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<sqlx::Error>(),
                Some(sqlx::Error::PoolTimedOut)
            ) || cause
                .downcast_ref::<ApiError>()
                .is_some_and(|error| error.code == "database_pool_busy")
        }) {
            tracing::warn!("database pool capacity unavailable");
            return Self::pool_busy();
        }
        tracing::error!(error = %error, "request operation failed");
        if crate::db::RetryableTransactionError::retryable_transaction(&error) {
            return Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "transaction_retry_exhausted",
                "concurrent database changes; retry the request",
            );
        }
        Self::internal()
    }
}

impl crate::db::RetryableTransactionError for ApiError {
    fn retryable_transaction(&self) -> bool {
        self.code == "transaction_retry_exhausted"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::RetryableTransactionError;

    #[test]
    fn pool_pressure_is_not_a_serialization_retry_for_a_database_closure() {
        for cause in [
            anyhow::Error::new(sqlx::Error::PoolTimedOut),
            anyhow::Error::new(ApiError::from(sqlx::Error::PoolTimedOut)),
        ] {
            let error = ApiError::from(cause.context("load current state"));
            assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(error.code, "database_pool_busy");
            assert!(
                !error.retryable_transaction(),
                "only native transaction conflicts may rerun a closure"
            );
        }
    }

    #[tokio::test]
    async fn actual_http_pool_pressure_returns_retryable_status_without_consuming_write_identity() {
        let pool = crate::db::test_pool_with_max_connections(1).await;
        sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('pool-admin',$1,'fixture',clock_timestamp())").bind(crate::security::token_digest("pool-admin")).execute(&pool).await.unwrap();
        let state = crate::state::AppState::new(
            pool.clone(),
            crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .unwrap(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/v2/users", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, crate::api::router(state))
                .await
                .unwrap()
        });
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();
        let held = pool.acquire().await.unwrap();
        let busy = client
            .post(&url)
            .bearer_auth("pool-admin")
            .header("idempotency-key", "same-write")
            .json(&serde_json::json!({"name":"pool recovery"}))
            .send()
            .await
            .unwrap();
        assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            busy.json::<serde_json::Value>().await.unwrap()["error"]["code"],
            "database_pool_busy"
        );
        drop(held);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM management_receipts")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
        let committed = client
            .post(&url)
            .bearer_auth("pool-admin")
            .header("idempotency-key", "same-write")
            .json(&serde_json::json!({"name":"pool recovery"}))
            .send()
            .await
            .unwrap();
        assert_eq!(committed.status(), StatusCode::CREATED);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM management_receipts")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
        server.abort();
    }
}
