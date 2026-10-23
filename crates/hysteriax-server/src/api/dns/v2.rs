use super::*;
use crate::api::{
    AdminActor,
    idempotency::{RequestIdentity, StoredResponse, execute, replay},
};
use axum::{Extension, body::Bytes, extract::OriginalUri, http::HeaderMap, response::Response};

fn parse<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, ApiError> {
    serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid DNS management request"))
}

pub(crate) async fn create_connection(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "dns.connections.create",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: ConnectionInput = parse(body)?;
    let name = match super::prepare_connection(&state, &input).await {
        Ok(name) => name,
        Err(error) => {
            if let Some(response) = replay(&state, &identity).await? {
                return response.into_http();
            }
            return Err(error);
        }
    };
    let id = Uuid::new_v4().to_string();
    execute(&state, identity, move |tx| {
        let (input, name, id, actor) = (input.clone(), name.clone(), id.clone(), actor.id.clone());
        Box::pin(async move {
            let (status, Json(body)) =
                super::create_connection_on(tx, input, name, id, actor).await?;
            Ok(StoredResponse::json(status, body))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn patch_connection(
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
        "dns.connections.patch",
        &headers,
        json!({"method":"PATCH","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: ConnectionPatch = parse(body)?;
    let name = valid_name(&input.name)?.to_owned();
    execute(&state, identity, move |tx| {
        let (id, input, name, actor) = (id.clone(), input.clone(), name.clone(), actor.id.clone());
        Box::pin(async move {
            let Json(body) = super::patch_connection_on(tx, id, input, name, actor).await?;
            Ok(StoredResponse::json(StatusCode::OK, body))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn delete_connection(
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
        "dns.connections.delete",
        &headers,
        json!({"method":"DELETE","uri":uri.to_string(),"id":id,"body":body.as_ref()}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    execute(&state, identity, move |tx| {
        let (id, query, actor) = (id.clone(), query.clone(), actor.id.clone());
        Box::pin(async move {
            Ok(StoredResponse::empty(
                super::delete_connection_on(tx, id, query, actor).await?,
            ))
        })
    })
    .await?
    .into_http()
}

async fn reject_orphan_on(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
    request: &Value,
) -> Result<(), ApiError> {
    if dns::existing_operation(tx, key, request).await?.is_some() {
        return Err(ApiError::conflict(
            "DNS operation exists without this management receipt; inspect its outcome before retrying",
        ));
    }
    Ok(())
}

pub(crate) async fn connection_action(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let action = match uri.path().rsplit('/').next() {
        Some("verify") => "verify",
        Some("refresh") => "connection-refresh",
        _ => return Err(ApiError::not_found("DNS connection action")),
    };
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        &format!("dns.connections.{action}"),
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let mut body = body;
    let object = body
        .as_object_mut()
        .ok_or_else(|| ApiError::bad_request("invalid DNS action request"))?;
    object.insert(
        "idempotency_key".into(),
        json!(identity.operation_key(&state.secrets)?),
    );
    let input: ActionInput = parse(body)?;
    let request = json!({"connection_id":id,"action":action,"input":input});
    execute(&state, identity, move |tx| {
        let (id, input, request) = (id.clone(), input.clone(), request.clone());
        Box::pin(async move {
            reject_orphan_on(tx, &input.idempotency_key, &request).await?;
            Ok(StoredResponse::json(
                StatusCode::ACCEPTED,
                super::connection_action_on(tx, id, input, action.into(), request).await?,
            ))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn patch_zone(
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
        "dns.zones.patch",
        &headers,
        json!({"method":"PATCH","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: ZonePatch = parse(body)?;
    execute(&state, identity, move |tx| {
        let (id, input, actor) = (id.clone(), input.clone(), actor.id.clone());
        Box::pin(async move {
            let Json(body) = super::patch_zone_on(tx, id, input, actor).await?;
            Ok(StoredResponse::json(StatusCode::OK, body))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn refresh_zone(
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
        "dns.zones.refresh",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let mut body = body;
    let object = body
        .as_object_mut()
        .ok_or_else(|| ApiError::bad_request("invalid DNS action request"))?;
    object.insert(
        "idempotency_key".into(),
        json!(identity.operation_key(&state.secrets)?),
    );
    let input: ActionInput = parse(body)?;
    let request = json!({"zone_id":id,"action":"zone-refresh","input":input});
    execute(&state, identity, move |tx| {
        let (id, input, request) = (id.clone(), input.clone(), request.clone());
        Box::pin(async move {
            reject_orphan_on(tx, &input.idempotency_key, &request).await?;
            Ok(StoredResponse::json(
                StatusCode::ACCEPTED,
                super::refresh_zone_on(tx, id, input, request).await?,
            ))
        })
    })
    .await?
    .into_http()
}

#[cfg(test)]
mod tests;
