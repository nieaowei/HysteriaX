//! Pure database mutation boundary. Prepare secrets and external results before
//! entering it. Never call another transaction boundary or do external I/O here.
use std::{future::Future, pin::Pin, sync::Arc};

use axum::{
    body::Body,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::Response,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};

use crate::{db, error::ApiError, security::SecretBox, state::AppState};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RequestIdentity {
    actor: String,
    operation: String,
    key: String,
    digest: String,
}

impl RequestIdentity {
    pub(super) fn from_headers(
        secrets: &SecretBox,
        actor: &str,
        operation: &str,
        headers: &HeaderMap,
        request: Value,
    ) -> Result<Self, ApiError> {
        let mut keys = headers.get_all("idempotency-key").iter();
        let key = keys.next().and_then(|key| key.to_str().ok());
        let key = key.filter(|key| {
            (1..=200).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_graphic())
        });
        let Some(key) = key.filter(|_| keys.next().is_none()) else {
            return Err(ApiError::bad_request(
                "exactly one Idempotency-Key of 1 to 200 visible ASCII characters is required",
            ));
        };
        let digest = secrets.request_digest(
            "hysteriax.management.v2.request",
            &serde_json::to_vec(&json!({"operation":operation,"request":request}))
                .map_err(|_| ApiError::internal())?,
        );
        Ok(Self {
            actor: actor.into(),
            operation: operation.into(),
            key: key.into(),
            digest,
        })
    }
}

/// The successful HTTP result, including secret fields, is encrypted as a whole.
/// Preserve the serialized body so replays return exactly the committed bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StoredResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl StoredResponse {
    pub(super) fn empty(status: StatusCode) -> Self {
        Self {
            status: status.as_u16(),
            headers: vec![("cache-control".into(), "no-store".into())],
            body: String::new(),
        }
    }

    pub(super) fn json(status: StatusCode, body: Value) -> Self {
        Self {
            status: status.as_u16(),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("cache-control".into(), "no-store".into()),
            ],
            body: body.to_string(),
        }
    }

    pub(super) fn into_http(self) -> Result<Response, ApiError> {
        let status = StatusCode::from_u16(self.status).map_err(|_| ApiError::internal())?;
        if !status.is_success() || (matches!(status.as_u16(), 204 | 205) && !self.body.is_empty()) {
            return Err(ApiError::internal());
        }
        let mut response = Response::new(Body::from(self.body));
        *response.status_mut() = status;
        for (name, value) in self.headers {
            response.headers_mut().append(
                HeaderName::try_from(name).map_err(|_| ApiError::internal())?,
                HeaderValue::try_from(value).map_err(|_| ApiError::internal())?,
            );
        }
        Ok(response)
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    identity: RequestIdentity,
    response: StoredResponse,
}

async fn authorize_on(tx: &mut sqlx::PgConnection, actor: &str) -> Result<(), ApiError> {
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM admin_tokens WHERE id=$1 AND revoked_at IS NULL)",
    )
    .bind(actor)
    .fetch_one(tx)
    .await?;
    if !active {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "administrator token is invalid",
        ));
    }
    Ok(())
}

fn restore(
    secrets: &SecretBox,
    identity: &RequestIdentity,
    row: &sqlx::postgres::PgRow,
) -> Result<StoredResponse, ApiError> {
    if row.get::<String, _>("request_digest") != identity.digest {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "Idempotency-Key was already used for a different request",
        ));
    }
    let saved: String = row.get("response_enc");
    if saved.is_empty() {
        return Err(ApiError::internal());
    }
    let envelope: Envelope = serde_json::from_slice(&secrets.decrypt_bytes(&saved)?)
        .map_err(|_| ApiError::internal())?;
    if envelope.identity != *identity {
        return Err(ApiError::internal());
    }
    envelope.response.clone().into_http()?;
    Ok(envelope.response)
}

/// Read committed proof before mutable preparation. A miss grants no ownership:
/// execute must still reserve/recheck the key atomically with the business work.
pub(super) async fn replay(
    state: &AppState,
    identity: &RequestIdentity,
) -> Result<Option<StoredResponse>, ApiError> {
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    authorize_on(&mut tx, &identity.actor).await?;
    let row = sqlx::query("SELECT request_digest,response_enc FROM management_receipts WHERE actor_id=$1 AND operation=$2 AND idempotency_key=$3")
        .bind(&identity.actor).bind(&identity.operation).bind(&identity.key).fetch_optional(&mut *tx).await?;
    let response = row
        .map(|row| restore(&state.secrets, identity, &row))
        .transpose()?;
    tx.commit().await?;
    Ok(response)
}

/// Only native serialization/deadlock failures retry. Reservation, business
/// changes, audit/outbox and encrypted response all commit or roll back together.
pub(super) async fn execute<F>(
    state: &AppState,
    identity: RequestIdentity,
    mutation: F,
) -> Result<StoredResponse, ApiError>
where
    F: for<'c> Fn(
            &'c mut Transaction<'static, Postgres>,
        )
            -> Pin<Box<dyn Future<Output = Result<StoredResponse, ApiError>> + Send + 'c>>
        + Send
        + Sync
        + 'static,
{
    let secrets = state.secrets.clone();
    let mutation = Arc::new(mutation);
    db::serializable(&state.pool, move |tx| {
        let (identity, secrets, mutation) = (identity.clone(), secrets.clone(), mutation.clone());
        Box::pin(async move {
            authorize_on(tx,&identity.actor).await?;
            let inserted = sqlx::query("INSERT INTO management_receipts(actor_id,operation,idempotency_key,request_digest,response_enc) VALUES($1,$2,$3,$4,'') ON CONFLICT DO NOTHING")
                .bind(&identity.actor).bind(&identity.operation).bind(&identity.key).bind(&identity.digest)
                .execute(&mut **tx).await?.rows_affected() == 1;
            let row = sqlx::query("SELECT request_digest,response_enc FROM management_receipts WHERE actor_id=$1 AND operation=$2 AND idempotency_key=$3 FOR UPDATE")
                .bind(&identity.actor).bind(&identity.operation).bind(&identity.key)
                .fetch_one(&mut **tx).await?;
            if !inserted {
                return restore(&secrets,&identity,&row);
            }
            if row.get::<String,_>("request_digest") != identity.digest || !row.get::<String,_>("response_enc").is_empty() {
                return Err(ApiError::internal());
            }
            let response = mutation(tx).await?;
            // Validate before commit so an invalid response cannot strand a mutation.
            response.clone().into_http()?;
            let ciphertext = secrets.encrypt_bytes(
                &serde_json::to_vec(&Envelope {
                    identity: identity.clone(),
                    response: response.clone(),
                })
                .map_err(|_| ApiError::internal())?,
            )?;
            sqlx::query("UPDATE management_receipts SET response_enc=$4 WHERE actor_id=$1 AND operation=$2 AND idempotency_key=$3")
                .bind(&identity.actor).bind(&identity.operation).bind(&identity.key).bind(ciphertext)
                .execute(&mut **tx).await?;
            Ok(response)
        })
    })
    .await
}

#[cfg(test)]
mod tests;
