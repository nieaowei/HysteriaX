//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateUser>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let prepared = PreparedUser::new(input, &state.secrets)?;
    let actor = super::super::audit_actor();
    let response = db::serializable::<_, ApiError, _>(&state.pool, move |tx| {
        let (prepared, actor) = (prepared.clone(), actor.clone());
        Box::pin(async move { prepared.commit_on(tx, &actor).await })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(response)))
}

pub(crate) async fn patch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<PatchUser>,
) -> Result<Json<Value>, ApiError> {
    let actor = super::super::audit_actor();
    let value = db::serializable(&state.pool, move |tx| {
        let (id, input, actor) = (id.clone(), input.clone(), actor.clone());
        Box::pin(async move { patch_on(tx, id, input, actor).await })
    })
    .await?;
    Ok(Json(value))
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<super::super::nodes::RevisionQuery>,
) -> Result<StatusCode, ApiError> {
    let expected = query
        .expected_revision
        .ok_or_else(|| ApiError::bad_request("expected_revision query parameter is required"))?;
    let actor = super::super::audit_actor();
    db::serializable(&state.pool, move |tx| {
        let (id, actor) = (id.clone(), actor.clone());
        Box::pin(async move { delete_on(tx, id, expected, actor).await })
    })
    .await
}

pub(crate) async fn rotate_credentials(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<RevisionRequest>,
) -> Result<Json<Value>, ApiError> {
    let prepared = rotation::prepare(&state, &user_id).await?;
    let actor = super::super::audit_actor();
    let response = db::serializable(&state.pool, move |tx| {
        let (user_id, prepared, actor) = (user_id.clone(), prepared.clone(), actor.clone());
        Box::pin(async move {
            rotation::commit_on(tx, user_id, input.expected_revision, prepared, actor).await
        })
    })
    .await?;
    Ok(Json(response))
}

pub(crate) async fn reset_quota(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<RevisionRequest>,
) -> Result<Json<Value>, ApiError> {
    let actor = super::super::audit_actor();
    let response = db::serializable(&state.pool, move |tx| {
        let (user_id, actor) = (user_id.clone(), actor.clone());
        Box::pin(async move { reset_quota_on(tx, user_id, input.expected_revision, actor).await })
    })
    .await?;
    Ok(Json(response))
}
