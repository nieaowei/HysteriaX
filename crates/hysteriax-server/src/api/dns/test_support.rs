//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn create_connection(
    State(state): State<AppState>,
    Json(input): Json<ConnectionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let name = prepare_connection(&state, &input).await?;
    let id = Uuid::new_v4().to_string();
    let actor = super::super::audit_actor();
    db::serializable::<_, ApiError, _>(&state.pool, move |tx| {
        let (input, name, id) = (input.clone(), name.clone(), id.clone());
        let actor = actor.clone();
        Box::pin(async move { create_connection_on(tx, input, name, id, actor).await })
    })
    .await
}

pub(crate) async fn patch_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ConnectionPatch>,
) -> Result<Json<Value>, ApiError> {
    let name = valid_name(&input.name)?.to_owned();
    let actor = super::super::audit_actor();
    db::serializable::<_, ApiError, _>(&state.pool, move |tx| {
        let (id, input, name) = (id.clone(), input.clone(), name.clone());
        let actor = actor.clone();
        Box::pin(async move { patch_connection_on(tx, id, input, name, actor).await })
    })
    .await
}

pub(crate) async fn delete_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(input): Query<RevisionQuery>,
) -> Result<StatusCode, ApiError> {
    let actor = super::super::audit_actor();
    db::serializable::<_, ApiError, _>(&state.pool, move |tx| {
        let (id, input) = (id.clone(), input.clone());
        let actor = actor.clone();
        Box::pin(async move { delete_connection_on(tx, id, input, actor).await })
    })
    .await
}

pub(crate) async fn connection_action(
    state: &AppState,
    id: &str,
    input: ActionInput,
    action: &str,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let request = json!({"connection_id":id,"action":action,"input":input});
    let id = id.to_owned();
    let action = action.to_owned();
    let key = input.idempotency_key.clone();
    let callback_request = request.clone();
    let result = dns::request_transaction(&state.pool, &key, &request, move |tx| {
        let (id, input, action, request) = (
            id.clone(),
            input.clone(),
            action.clone(),
            callback_request.clone(),
        );
        Box::pin(async move { connection_action_on(tx, id, input, action, request).await })
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

pub(crate) async fn verify_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ActionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    connection_action(&state, &id, input, "verify").await
}

pub(crate) async fn refresh_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ActionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    connection_action(&state, &id, input, "connection-refresh").await
}

pub(crate) async fn patch_zone(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ZonePatch>,
) -> Result<Json<Value>, ApiError> {
    let actor = super::super::audit_actor();
    db::serializable::<_, ApiError, _>(&state.pool, move |tx| {
        let (id, input, actor) = (id.clone(), input.clone(), actor.clone());
        Box::pin(async move { patch_zone_on(tx, id, input, actor).await })
    })
    .await
}

pub(crate) async fn refresh_zone(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ActionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let request = json!({"zone_id":id,"action":"zone-refresh","input":input});
    let key = input.idempotency_key.clone();
    let callback_request = request.clone();
    let result = dns::request_transaction(&state.pool, &key, &request, move |tx| {
        let (id, input, request) = (id.clone(), input.clone(), callback_request.clone());
        Box::pin(async move { refresh_zone_on(tx, id, input, request).await })
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

pub(crate) async fn create_record(
    State(state): State<AppState>,
    Json(input): Json<CreateRecord>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let request = json!({"action":"record-create","input":input});
    let key = input.idempotency_key.clone();
    let callback_request = request.clone();
    let result = dns::request_transaction(&state.pool, &key, &request, move |tx| {
        let (input, request) = (input.clone(), callback_request.clone());
        Box::pin(async move { create_record_on(tx, input, request).await })
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

pub(crate) async fn record_action(
    state: &AppState,
    id: &str,
    input: ActionInput,
    record: Option<RecordInput>,
    action: &str,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let request = json!({"action":action,"record_id":id,"input":input,"record":record});
    let id = id.to_owned();
    let action = action.to_owned();
    let key = input.idempotency_key.clone();
    let callback_request = request.clone();
    let result = dns::request_transaction(&state.pool, &key, &request, move |tx| {
        let (id, input, record, action, request) = (
            id.clone(),
            input.clone(),
            record.clone(),
            action.clone(),
            callback_request.clone(),
        );
        Box::pin(async move { record_action_on(tx, id, input, record, action, request).await })
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

pub(crate) async fn update_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateRecord>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    record_action(
        &state,
        &id,
        ActionInput {
            expected_revision: input.expected_revision,
            idempotency_key: input.idempotency_key,
        },
        Some(input.record),
        "record-update",
    )
    .await
}

pub(crate) async fn delete_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ActionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    record_action(&state, &id, input, None, "record-delete").await
}

pub(crate) async fn check_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ActionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    record_action(&state, &id, input, None, "record-check").await
}

pub(crate) async fn set_binding(
    State(state): State<AppState>,
    Path(node): Path<String>,
    Json(input): Json<SetBinding>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let request = json!({"node_id":node,"action":"binding-set","input":input});
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    let previous =
        dns::existing_operation(&mut tx, &input.allocation.idempotency_key, &request).await?;
    tx.commit().await?;
    if let Some(previous) = previous {
        return Ok((StatusCode::ACCEPTED, Json(previous)));
    }
    let prepared = crate::dns::binding::prepare(&state, &node, &input.allocation).await?;
    let key = input.allocation.idempotency_key.clone();
    let prepared = std::sync::Arc::new(prepared);
    let app = state.clone();
    let callback_request = request.clone();
    let result = dns::request_transaction(&state.pool, &key, &request, move |tx| {
        let (node, input, prepared, app, request) = (
            node.clone(),
            input.clone(),
            prepared.clone(),
            app.clone(),
            callback_request.clone(),
        );
        Box::pin(async move { set_binding_on(tx, node, input, prepared, app, request).await })
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

pub(crate) async fn unbind(
    State(state): State<AppState>,
    Path(node): Path<String>,
    Json(input): Json<Unbind>,
) -> Result<Json<Value>, ApiError> {
    let request = json!({"node_id":node,"action":"binding-remove","input":input});
    validate_unbind(&input)?;
    let actor = super::super::audit_actor();
    let key = input.idempotency_key.clone();
    let callback_request = request.clone();
    let app = state.clone();
    let result = dns::request_transaction(&state.pool, &key, &request, move |tx| {
        let (node, input, request, app, actor) = (
            node.clone(),
            input.clone(),
            callback_request.clone(),
            app.clone(),
            actor.clone(),
        );
        Box::pin(async move { unbind_on(tx, node, input, request, app, actor).await })
    })
    .await?;
    Ok(Json(result))
}
