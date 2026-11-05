use axum::{
    Json,
    extract::{Extension, OriginalUri, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    AdminActor, audit_on,
    idempotency::{RequestIdentity, StoredResponse, execute},
};
use crate::{error::ApiError, security::token_digest, state::AppState};

#[derive(Clone)]
pub(super) struct PreparedToken {
    id: String,
    label: String,
    token: String,
    hash: String,
    timestamp: DateTime<Utc>,
}

impl PreparedToken {
    pub(super) fn new(label: &str) -> Result<Self, ApiError> {
        let label = super::validate_admin_token_label(label)?.to_owned();
        let token = super::generate_token();
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            label,
            hash: token_digest(&token),
            token,
            timestamp: super::now(),
        })
    }

    pub(super) async fn commit_on(
        &self,
        tx: &mut PgConnection,
        actor: &str,
    ) -> Result<Value, ApiError> {
        sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES($1,$2,$3,$4)")
            .bind(&self.id)
            .bind(&self.hash)
            .bind(&self.label)
            .bind(self.timestamp)
            .execute(&mut *tx)
            .await?;
        audit_on(
            tx,
            actor,
            "admin_token.created",
            "admin_token",
            &self.id,
            json!({"label":self.label}),
        )
        .await?;
        Ok(json!({"id":self.id,"label":self.label,"token":self.token}))
    }
}

pub(super) async fn create(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "admin_tokens.create",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"body":body}),
    )?;
    let input: super::CreateAdminToken = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid administrator token request"))?;
    let prepared = PreparedToken::new(&input.label)?;
    execute(&state, identity, move |tx| {
        let (prepared, actor) = (prepared.clone(), actor.id.clone());
        Box::pin(async move {
            Ok(StoredResponse::json(
                StatusCode::CREATED,
                prepared.commit_on(tx, &actor).await?,
            ))
        })
    })
    .await?
    .into_http()
}

pub(super) async fn revoke_on(
    tx: &mut PgConnection,
    actor: &str,
    id: &str,
) -> Result<(), ApiError> {
    if actor == id {
        return Err(ApiError::conflict(
            "cannot revoke the token used by this request",
        ));
    }
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM admin_tokens WHERE revoked_at IS NULL")
            .fetch_one(&mut *tx)
            .await?;
    if count <= 1 {
        return Err(ApiError::conflict(
            "cannot revoke the last active administrator token",
        ));
    }
    let changed =
        sqlx::query("UPDATE admin_tokens SET revoked_at=$1 WHERE id=$2 AND revoked_at IS NULL")
            .bind(super::now())
            .bind(id)
            .execute(&mut *tx)
            .await?;
    if changed.rows_affected() != 1 {
        return Err(ApiError::not_found("active administrator token"));
    }
    audit_on(
        tx,
        actor,
        "admin_token.revoked",
        "admin_token",
        id,
        json!({}),
    )
    .await
}

pub(super) async fn revoke(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "admin_tokens.revoke",
        &headers,
        json!({"method":"DELETE","uri":uri.to_string(),"id":id,"body":body.as_ref()}),
    )?;
    let response = execute(&state, identity, move |tx| {
        let (actor, id) = (actor.id.clone(), id.clone());
        Box::pin(async move {
            revoke_on(tx, &actor, &id).await?;
            Ok(StoredResponse::empty(StatusCode::NO_CONTENT))
        })
    })
    .await?;
    state
        .admin_tokens_changed
        .send_modify(|version| *version = version.wrapping_add(1));
    response.into_http()
}

#[cfg(test)]
mod tests;
