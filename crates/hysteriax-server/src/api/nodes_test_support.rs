//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateNode>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let allocation_request = serde_json::to_value(&input).map_err(|_| ApiError::internal())?;
    if let Some(allocation) = &input.dns_allocation {
        let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
        let previous = crate::dns::existing_operation(
            &mut tx,
            &allocation.idempotency_key,
            &allocation_request,
        )
        .await?;
        tx.commit().await?;
        if let Some(previous) = previous {
            return replay_created_node(&state, previous).await;
        }
    }
    let replay_key = input
        .dns_allocation
        .as_ref()
        .map(|a| a.idempotency_key.clone());
    let creation = prepare_creation(&state, input).await?;
    let app = state.clone();
    let outcome = db::serializable::<_, ApiError, _>(&state.pool, move |tx| {
        let (creation, app) = (creation.clone(), app.clone());
        Box::pin(async move { insert_node_on(tx, &app, &creation).await })
    })
    .await;
    let outcome = match outcome {
        Err(error) if replay_key.is_some() => NodeCreation::Existing(
            crate::dns::replay_after_error(
                &state.pool,
                replay_key.as_deref().unwrap(),
                &allocation_request,
                error,
            )
            .await?,
        ),
        other => other?,
    };
    match outcome {
        NodeCreation::Created(value) => Ok((StatusCode::CREATED, Json(value))),
        NodeCreation::Existing(previous) => replay_created_node(&state, previous).await,
    }
}

pub(crate) async fn replay_created_node(
    state: &AppState,
    previous: Value,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let id = previous["resource_id"]
        .as_str()
        .ok_or_else(ApiError::internal)?;
    let Json(node) = get(State(state.clone()), Path(id.into())).await?;
    Ok((
        StatusCode::OK,
        Json(
            json!({"node":node,"note":"Node already created; authentication token is not reissued."}),
        ),
    ))
}

pub(crate) async fn patch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<PatchNode>,
) -> Result<Json<Value>, ApiError> {
    let prepared = prepare_patch(&state, id, input).await?;
    let pool = state.pool.clone();
    db::serializable(&pool, move |tx| {
        let (state, prepared) = (state.clone(), prepared.clone());
        Box::pin(async move { patch_on(tx, &state, (*prepared).clone()).await })
    })
    .await
}

pub(crate) async fn remove_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RevisionQuery>,
) -> Result<StatusCode, ApiError> {
    let expected = query.expected_revision.filter(|r| *r > 0).ok_or_else(|| {
        ApiError::bad_request("positive expected_revision query parameter is required")
    })?;
    let pool = state.pool.clone();
    let node = id.clone();
    let result = db::serializable(&pool, move |tx| {
        let id = node.clone();
        Box::pin(async move { remove_record_on(tx, id, expected).await })
    })
    .await?;
    if result == StatusCode::NO_CONTENT {
        let _ = state.removed_nodes.send(id);
    }
    Ok(result)
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RevisionQuery>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let expected = query
        .expected_revision
        .ok_or_else(|| ApiError::bad_request("expected_revision query parameter is required"))?;
    let pool = state.pool.clone();
    let node = id.clone();
    let result = db::serializable(&pool, move |tx| {
        let id = node.clone();
        Box::pin(async move { delete_on(tx, id, expected).await })
    })
    .await?;
    if result.0 == StatusCode::NO_CONTENT {
        let _ = state.removed_nodes.send(id);
    }
    Ok(result)
}

pub(crate) async fn deploy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<NodeAction>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    action(&state, &id, input.expected_revision, "deploy").await
}

pub(crate) async fn ssh_test(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<NodeAction>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    action(&state, &id, input.expected_revision, "ssh-test").await
}

pub(crate) async fn sync(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<NodeAction>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    action(&state, &id, input.expected_revision, "sync").await
}

pub(crate) async fn rollback(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<NodeAction>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    action(&state, &id, input.expected_revision, "rollback").await
}

pub(crate) async fn action(
    state: &AppState,
    id: &str,
    expected: i64,
    kind: &str,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (id, kind, state, actor) = (
        id.to_owned(),
        kind.to_owned(),
        state.clone(),
        super::super::audit_actor(),
    );
    let pool = state.pool.clone();
    let receipt = db::serializable(&pool, move |tx| {
        let (id, kind, state, actor) = (id.clone(), kind.clone(), state.clone(), actor.clone());
        Box::pin(async move { action_on(tx, &state, &id, expected, &kind, &actor).await })
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(receipt)))
}
