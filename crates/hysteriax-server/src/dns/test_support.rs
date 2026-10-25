//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

/// Recover a receipt after rollback without rerunning business writes. A
/// concurrent commit may have invalidated our revision check or won the unique
/// idempotency key. Only a committed, matching request is eligible for replay.
pub(crate) async fn replay_after_error(
    pool: &PgPool,
    key: &str,
    request: &Value,
    error: ApiError,
) -> Result<Value, ApiError> {
    if error.status != axum::http::StatusCode::CONFLICT {
        return Err(error);
    }
    let mut tx = crate::read_snapshot::transaction(pool).await?;
    let previous = existing_operation(&mut tx, key, request).await?;
    tx.commit().await?;
    previous.ok_or(error)
}

pub(crate) async fn request_transaction<F>(
    pool: &PgPool,
    key: &str,
    request: &Value,
    operation: F,
) -> Result<Value, ApiError>
where
    F: for<'connection> FnMut(
        &'connection mut Transaction<'static, Postgres>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, ApiError>> + Send + 'connection>,
    >,
{
    match crate::db::serializable(pool, operation).await {
        Ok(value) => Ok(value),
        Err(error) => replay_after_error(pool, key, request, error).await,
    }
}
