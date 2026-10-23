use super::{
    AdminActor, CreateCredential, PatchCredential, PublishVersion, mutations, preparation,
};
use crate::{
    api::{
        idempotency::{RequestIdentity, StoredResponse, execute, replay},
        nodes::RevisionQuery,
    },
    error::ApiError,
    state::AppState,
};
use axum::{
    Json,
    body::Bytes,
    extract::{Extension, OriginalUri, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::{Value, json};

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
        "credentials.create",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: CreateCredential = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid credential creation request"))?;
    let prepared = preparation::Creation::new(&state.secrets, input)?;
    execute(&state, identity, move |tx| {
        let (prepared, actor) = (prepared.clone(), actor.clone());
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
        "credentials.patch",
        &headers,
        json!({"method":"PATCH","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: PatchCredential = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid credential update request"))?;
    execute(&state, identity, move |tx| {
        let (id, input, actor) = (id.clone(), input.clone(), actor.clone());
        Box::pin(async move {
            Ok(StoredResponse::json(
                StatusCode::OK,
                mutations::patch_on(tx, id, input, actor).await?,
            ))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn publish(
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
        "credentials.publish",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: PublishVersion = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid credential publication request"))?;
    let prepared = match preparation::Publication::new(&state, &id, &input).await {
        Ok(prepared) => prepared,
        Err(error) => {
            // Another same-key request may commit while preparation reads the
            // newer revision or a now-deleted source. Committed proof wins.
            if let Some(response) = replay(&state, &identity).await? {
                return response.into_http();
            }
            return Err(error);
        }
    };
    execute(&state, identity, move |tx| {
        let (id, prepared, actor) = (id.clone(), prepared.clone(), actor.clone());
        Box::pin(async move {
            Ok(StoredResponse::json(
                StatusCode::ACCEPTED,
                mutations::publish_on(tx, id, input.expected_revision, prepared, actor).await?,
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
        "credentials.delete",
        &headers,
        json!({"method":"DELETE","uri":uri.to_string(),"id":id,"body":body.as_ref()}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let expected = query
        .expected_revision
        .ok_or_else(|| ApiError::bad_request("expected_revision is required"))?;
    let secrets = state.secrets.clone();
    execute(&state, identity, move |tx| {
        let (id, secrets, actor) = (id.clone(), secrets.clone(), actor.clone());
        Box::pin(async move {
            Ok(StoredResponse::empty(
                mutations::delete_on(tx, id, expected, secrets, actor).await?,
            ))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn retry(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "credentials.batch_retry",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body.as_ref()}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    execute(&state, identity, move |tx| {
        let (id, actor) = (id.clone(), actor.clone());
        Box::pin(async move {
            Ok(StoredResponse::json(
                StatusCode::ACCEPTED,
                mutations::retry_on(tx, id, actor).await?,
            ))
        })
    })
    .await?
    .into_http()
}

#[cfg(test)]
mod tests;
