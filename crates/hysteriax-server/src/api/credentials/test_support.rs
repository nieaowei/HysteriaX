//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn create(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Json(input): Json<CreateCredential>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let prepared = preparation::Creation::new(&state.secrets, input)?;
    let response = db::serializable(&state.pool, move |tx| {
        let (prepared, actor) = (prepared.clone(), actor.clone());
        Box::pin(async move { prepared.commit_on(tx, &actor).await })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(response)))
}

pub(crate) async fn patch(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    Json(input): Json<PatchCredential>,
) -> Result<Json<Value>, ApiError> {
    let response = db::serializable(&state.pool, move |tx| {
        let (id, input, actor) = (id.clone(), input.clone(), actor.clone());
        Box::pin(async move { mutations::patch_on(tx, id, input, actor).await })
    })
    .await?;
    Ok(Json(response))
}

pub(crate) async fn publish(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    Json(input): Json<PublishVersion>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let prepared = preparation::Publication::new(&state, &id, &input).await?;
    let response = db::serializable(&state.pool, move |tx| {
        let (id, prepared, actor) = (id.clone(), prepared.clone(), actor.clone());
        Box::pin(async move {
            mutations::publish_on(tx, id, input.expected_revision, prepared, actor).await
        })
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    Query(query): Query<super::super::nodes::RevisionQuery>,
) -> Result<StatusCode, ApiError> {
    let expected = query
        .expected_revision
        .ok_or_else(|| ApiError::bad_request("expected_revision is required"))?;
    let secrets = state.secrets.clone();
    db::serializable(&state.pool, move |tx| {
        let (id, actor, secrets) = (id.clone(), actor.clone(), secrets.clone());
        Box::pin(async move { mutations::delete_on(tx, id, expected, secrets, actor).await })
    })
    .await
}

pub(crate) async fn retry(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let response = db::serializable(&state.pool, move |tx| {
        let (id, actor) = (id.clone(), actor.clone());
        Box::pin(async move { mutations::retry_on(tx, id, actor).await })
    })
    .await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}
