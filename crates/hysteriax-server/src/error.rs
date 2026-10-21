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
