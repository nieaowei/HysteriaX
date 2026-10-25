use super::{NodeAction, RevisionQuery};
use crate::{
    api::{
        AdminActor,
        idempotency::{RequestIdentity, StoredResponse, execute, replay},
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

pub(crate) async fn action(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let kind = uri.path().rsplit('/').next().unwrap_or_default().to_owned();
    if !matches!(kind.as_str(), "deploy" | "sync" | "rollback" | "ssh-test") {
        return Err(ApiError::not_found("node action"));
    }
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        &format!("nodes.{kind}"),
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: NodeAction = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid node action request"))?;
    let app = state.clone();
    execute(&state, identity, move |tx| {
        let (app, id, kind, actor) = (app.clone(), id.clone(), kind.clone(), actor.id.clone());
        Box::pin(async move {
            Ok(StoredResponse::json(
                StatusCode::ACCEPTED,
                super::action_on(tx, &app, &id, input.expected_revision, &kind, &actor).await?,
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
        "nodes.delete",
        &headers,
        json!({"method":"DELETE","uri":uri.to_string(),"id":id,"body":body.as_ref()}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let expected = query
        .expected_revision
        .ok_or_else(|| ApiError::bad_request("expected_revision query parameter is required"))?;
    let notify = id.clone();
    let response = execute(&state, identity, move |tx| {
        let id = id.clone();
        Box::pin(async move {
            let (status, Json(value)) = super::delete_on(tx, id, expected).await?;
            Ok(if status == StatusCode::NO_CONTENT {
                StoredResponse::empty(status)
            } else {
                StoredResponse::json(status, value)
            })
        })
    })
    .await?
    .into_http()?;
    if response.status() == StatusCode::NO_CONTENT {
        let _ = state.removed_nodes.send(notify);
    }
    Ok(response)
}

pub(crate) async fn remove_record(
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
        "nodes.remove_record",
        &headers,
        json!({"method":"DELETE","uri":uri.to_string(),"id":id,"body":body.as_ref()}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let expected = query.expected_revision.filter(|r| *r > 0).ok_or_else(|| {
        ApiError::bad_request("positive expected_revision query parameter is required")
    })?;
    let notify = id.clone();
    let response = execute(&state, identity, move |tx| {
        let id = id.clone();
        Box::pin(async move {
            Ok(StoredResponse::empty(
                super::remove_record_on(tx, id, expected).await?,
            ))
        })
    })
    .await?
    .into_http()?;
    if response.status() == StatusCode::NO_CONTENT {
        let _ = state.removed_nodes.send(notify);
    }
    Ok(response)
}

pub(crate) async fn update_usage(
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
        "nodes.update_usage",
        &headers,
        json!({"method":"PUT","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: crate::node_limits::UsageUpdate = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid node usage correction request"))?;
    let secrets = state.secrets.clone();
    execute(&state, identity, move |tx| {
        let (id, input, secrets, actor) =
            (id.clone(), input.clone(), secrets.clone(), actor.id.clone());
        Box::pin(async move {
            Ok(StoredResponse::json(
                StatusCode::OK,
                crate::node_limits::update_usage_on(tx, id, input, secrets, actor).await?,
            ))
        })
    })
    .await?
    .into_http()
}

#[cfg(test)]
mod creation_tests;
#[cfg(test)]
mod tests;

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
        "nodes.create",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let mut input_body = body;
    if let Some(allocation) = input_body
        .get_mut("dns_allocation")
        .and_then(Value::as_object_mut)
    {
        allocation.insert(
            "idempotency_key".into(),
            json!(identity.operation_key(&state.secrets)?),
        );
    }
    let input = serde_json::from_value(input_body)
        .map_err(|_| ApiError::bad_request("invalid node creation request"))?;
    let creation = match super::prepare_creation(&state, input).await {
        Ok(creation) => creation,
        Err(error) => {
            if let Some(response) = replay(&state, &identity).await? {
                return response.into_http();
            }
            return Err(error);
        }
    };
    let app = state.clone();
    execute(&state,identity,move |tx| {
        let (creation,app)=(creation.clone(),app.clone());
        Box::pin(async move {
            match super::insert_node_on(tx,&app,&creation).await? {
                super::NodeCreation::Created(value)=>Ok(StoredResponse::json(StatusCode::CREATED,value)),
                super::NodeCreation::Existing(previous)=>{
                    // A DNS receipt alone cannot authorize a management replay.
                    drop(previous);
                    Err(ApiError::conflict("DNS allocation has an existing operation without this management receipt; inspect its outcome before retrying"))
                },
            }
        })
    }).await?.into_http()
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
        "nodes.patch",
        &headers,
        json!({"method":"PATCH","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid node update request"))?;
    let prepared = match super::prepare_patch(&state, id, input).await {
        Ok(prepared) => prepared,
        Err(error) => {
            if let Some(response) = replay(&state, &identity).await? {
                return response.into_http();
            }
            return Err(error);
        }
    };
    let app = state.clone();
    execute(&state, identity, move |tx| {
        let (prepared, app) = (prepared.clone(), app.clone());
        Box::pin(async move {
            let Json(value) = super::patch_on(tx, &app, (*prepared).clone()).await?;
            Ok(StoredResponse::json(StatusCode::OK, value))
        })
    })
    .await?
    .into_http()
}
