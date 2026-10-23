use super::*;
use axum::http::Method;

fn child_body(
    identity: &RequestIdentity,
    state: &AppState,
    mut body: Value,
) -> Result<Value, ApiError> {
    body.as_object_mut()
        .ok_or_else(|| ApiError::bad_request("invalid DNS management request"))?
        .insert(
            "idempotency_key".into(),
            json!(identity.operation_key(&state.secrets)?),
        );
    Ok(body)
}

pub(crate) async fn create_record(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "dns.records.create",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: CreateRecord = parse(child_body(&identity, &state, body)?)?;
    let request = json!({"action":"record-create","input":input});
    execute(&state, identity, move |tx| {
        let (input, request) = (input.clone(), request.clone());
        Box::pin(async move {
            reject_orphan_on(tx, &input.idempotency_key, &request).await?;
            Ok(StoredResponse::json(
                StatusCode::ACCEPTED,
                super::super::create_record_on(tx, input, request).await?,
            ))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn record_action(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let action = match (method.as_str(), uri.path().rsplit('/').next()) {
        ("POST", Some("check")) => "record-check",
        ("PATCH", _) => "record-update",
        ("DELETE", _) => "record-delete",
        _ => return Err(ApiError::not_found("DNS record action")),
    };
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        &format!("dns.records.{action}"),
        &headers,
        json!({"method":method.as_str(),"uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let body = child_body(&identity, &state, body)?;
    let (input, record) = if action == "record-update" {
        let update: UpdateRecord = parse(body)?;
        (
            ActionInput {
                expected_revision: update.expected_revision,
                idempotency_key: update.idempotency_key,
            },
            Some(update.record),
        )
    } else {
        (parse::<ActionInput>(body)?, None)
    };
    let request = json!({"action":action,"record_id":id,"input":input,"record":record});
    execute(&state, identity, move |tx| {
        let (id, input, record, request) =
            (id.clone(), input.clone(), record.clone(), request.clone());
        Box::pin(async move {
            reject_orphan_on(tx, &input.idempotency_key, &request).await?;
            Ok(StoredResponse::json(
                StatusCode::ACCEPTED,
                super::super::record_action_on(tx, id, input, record, action.into(), request)
                    .await?,
            ))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn set_binding(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(node): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "nodes.dns_binding.set",
        &headers,
        json!({"method":"PUT","uri":uri.to_string(),"id":node,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let mut body = body;
    body.get_mut("allocation")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| ApiError::bad_request("invalid DNS allocation request"))?
        .insert(
            "idempotency_key".into(),
            json!(identity.operation_key(&state.secrets)?),
        );
    let input: SetBinding = parse(body)?;
    let prepared = match crate::dns::binding::prepare(&state, &node, &input.allocation).await {
        Ok(prepared) => std::sync::Arc::new(prepared),
        Err(error) => {
            if let Some(response) = replay(&state, &identity).await? {
                return response.into_http();
            }
            return Err(error);
        }
    };
    let request = json!({"node_id":node,"action":"binding-set","input":input});
    let app = state.clone();
    execute(&state, identity, move |tx| {
        let (node, input, prepared, app, request) = (
            node.clone(),
            input.clone(),
            prepared.clone(),
            app.clone(),
            request.clone(),
        );
        Box::pin(async move {
            reject_orphan_on(tx, &input.allocation.idempotency_key, &request).await?;
            Ok(StoredResponse::json(
                StatusCode::ACCEPTED,
                super::super::set_binding_on(tx, node, input, prepared, app, request).await?,
            ))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn unbind(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(node): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "nodes.dns_binding.remove",
        &headers,
        json!({"method":"DELETE","uri":uri.to_string(),"id":node,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: Unbind = parse(child_body(&identity, &state, body)?)?;
    super::super::validate_unbind(&input)?;
    let request = json!({"node_id":node,"action":"binding-remove","input":input});
    let app = state.clone();
    execute(&state, identity, move |tx| {
        let (node, input, app, request, actor) = (
            node.clone(),
            input.clone(),
            app.clone(),
            request.clone(),
            actor.id.clone(),
        );
        Box::pin(async move {
            reject_orphan_on(tx, &input.idempotency_key, &request).await?;
            Ok(StoredResponse::json(
                StatusCode::OK,
                super::super::unbind_on(tx, node, input, request, app, actor).await?,
            ))
        })
    })
    .await?
    .into_http()
}

#[cfg(test)]
mod tests;
