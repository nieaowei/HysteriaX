use axum::{
    Json,
    body::Bytes,
    extract::{Extension, OriginalUri, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::{Value, json};

use super::{PatchUser, PreparedUser, RevisionRequest, delete_on, patch_on, reset_quota_on};
use crate::{
    api::{
        AdminActor,
        idempotency::{RequestIdentity, StoredResponse, execute},
        nodes::RevisionQuery,
    },
    error::ApiError,
    state::AppState,
};

pub(crate) async fn create(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "users.create",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"body":body}),
    )?;
    let input = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid user creation request"))?;
    let prepared = PreparedUser::new(input)?;
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

pub(crate) async fn patch(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "users.patch",
        &headers,
        json!({"method":"PATCH","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    let input: PatchUser = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid user update request"))?;
    execute(&state, identity, move |tx| {
        let (id, input, actor) = (id.clone(), input.clone(), actor.id.clone());
        Box::pin(async move {
            Ok(StoredResponse::json(
                StatusCode::OK,
                patch_on(tx, id, input, actor).await?,
            ))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    Query(query): Query<RevisionQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "users.delete",
        &headers,
        json!({"method":"DELETE","uri":uri.to_string(),"id":id,"body":body.as_ref()}),
    )?;
    let expected = query
        .expected_revision
        .ok_or_else(|| ApiError::bad_request("expected_revision query parameter is required"))?;
    execute(&state, identity, move |tx| {
        let (id, actor) = (id.clone(), actor.id.clone());
        Box::pin(async move {
            Ok(StoredResponse::empty(
                delete_on(tx, id, expected, actor).await?,
            ))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn reset_quota(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "users.quota_reset",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    let input: RevisionRequest = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid quota reset request"))?;
    execute(&state, identity, move |tx| {
        let (id, actor) = (id.clone(), actor.id.clone());
        Box::pin(async move {
            Ok(StoredResponse::json(
                StatusCode::OK,
                reset_quota_on(tx, id, input.expected_revision, actor).await?,
            ))
        })
    })
    .await?
    .into_http()
}

#[cfg(test)]
mod tests;
