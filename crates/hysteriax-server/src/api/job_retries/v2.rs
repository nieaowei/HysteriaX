use super::*;
use crate::api::{
    AdminActor,
    idempotency::{RequestIdentity, StoredResponse, execute, replay},
};
use axum::{Extension, extract::OriginalUri, http::HeaderMap, response::Response};

pub(crate) async fn retry(
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
        "jobs.retry",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: RetryRequest = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid job retry request"))?;
    execute(&state, identity, move |tx| {
        let (id, input, actor) = (id.clone(), input.clone(), actor.id.clone());
        Box::pin(async move {
            let (status, Json(value)) = super::retry_on(tx, id, input, actor).await?;
            Ok(StoredResponse::json(status, value))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn confirm_fingerprint(
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
        "jobs.confirm_fingerprint",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: crate::api::ssh_fingerprint::Confirmation = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid fingerprint confirmation request"))?;
    let history_id = uuid::Uuid::new_v4().to_string();
    let context = state.clone();
    execute(&state, identity, move |tx| {
        let (state, id, input, actor, history_id) = (
            context.clone(),
            id.clone(),
            input.clone(),
            actor.id.clone(),
            history_id.clone(),
        );
        Box::pin(async move {
            let (status, Json(value)) =
                crate::api::ssh_fingerprint::confirm_on(tx, &state, id, input, actor, history_id)
                    .await?;
            Ok(StoredResponse::json(status, value))
        })
    })
    .await?
    .into_http()
}

#[cfg(test)]
mod tests;
