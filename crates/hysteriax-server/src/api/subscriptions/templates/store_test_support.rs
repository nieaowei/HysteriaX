//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn create_inner(
    State(state): State<AppState>,
    Json(input): Json<Create>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let prepared = PreparedCreation::new(&state, input)?;
    db::serializable(&state.pool, move |tx| {
        let prepared = prepared.clone();
        Box::pin(async move { prepared.commit_on(tx).await })
    })
    .await
}

pub(crate) async fn patch_inner(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Patch>,
) -> Result<Json<Value>, ApiError> {
    db::serializable(&state.pool, move |tx| {
        let (id, input) = (id.clone(), input.clone());
        Box::pin(async move { patch_on(tx, id, input).await })
    })
    .await
}

pub(crate) async fn save_inner(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Save>,
) -> Result<Json<Value>, ApiError> {
    let encrypted = prepare_save(&state, &input)?;
    db::serializable(&state.pool, move |tx| {
        let (id, encrypted) = (id.clone(), encrypted.clone());
        Box::pin(async move { save_on(tx, id, input.expected_revision, encrypted).await })
    })
    .await
}

pub(crate) async fn publish_document(
    state: &AppState,
    id: &str,
    revision: i64,
    version: Option<i64>,
) -> Result<Json<Value>, ApiError> {
    let prepared = PreparedPublication::new(state, id, revision, version).await?;
    db::serializable(&state.pool, move |tx| {
        let prepared = prepared.clone();
        Box::pin(async move { prepared.commit_on(tx).await })
    })
    .await
}

pub(crate) async fn publish_inner(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Revision>,
) -> Result<Json<Value>, ApiError> {
    publish_document(&state, &id, input.expected_revision, None).await
}

pub(crate) async fn rollback_inner(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Rollback>,
) -> Result<Json<Value>, ApiError> {
    publish_document(&state, &id, input.expected_revision, Some(input.version)).await
}

pub(crate) async fn import(Json(input): Json<Import>) -> Result<Response, ApiError> {
    Ok(no_store(import_value(input)?))
}

pub(crate) async fn create(
    State(state): State<AppState>,
    Json(input): Json<Create>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let revision = input.expected_revision;
    let result = create_inner(State(state.clone()), Json(input)).await;
    report_failure(
        &state,
        "new",
        "template.create_failed",
        revision,
        None,
        result,
    )
    .await
}

pub(crate) async fn patch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Patch>,
) -> Result<Json<Value>, ApiError> {
    let revision = input.expected_revision;
    let action = if input.archived == Some(true) {
        "template.archive_failed"
    } else if input.is_default == Some(true) {
        "template.default_change_failed"
    } else {
        "template.metadata_update_failed"
    };
    let result = patch_inner(State(state.clone()), Path(id.clone()), Json(input)).await;
    report_failure(&state, &id, action, revision, None, result).await
}

pub(crate) async fn save(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Save>,
) -> Result<Json<Value>, ApiError> {
    let revision = input.expected_revision;
    let result = save_inner(State(state.clone()), Path(id.clone()), Json(input)).await;
    report_failure(
        &state,
        &id,
        "template.draft_save_failed",
        revision,
        None,
        result,
    )
    .await
}

pub(crate) async fn publish(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Revision>,
) -> Result<Json<Value>, ApiError> {
    let revision = input.expected_revision;
    let result = publish_inner(State(state.clone()), Path(id.clone()), Json(input)).await;
    report_failure(
        &state,
        &id,
        "template.publish_failed",
        revision,
        None,
        result,
    )
    .await
}

pub(crate) async fn rollback(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Rollback>,
) -> Result<Json<Value>, ApiError> {
    let revision = input.expected_revision;
    let version = input.version;
    let result = rollback_inner(State(state.clone()), Path(id.clone()), Json(input)).await;
    report_failure(
        &state,
        &id,
        "template.rollback_failed",
        revision,
        Some(version),
        result,
    )
    .await
}
