//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(super) async fn create_admin_token(
    State(state): State<AppState>,
    Json(input): Json<CreateAdminToken>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let prepared = admin_tokens::PreparedToken::new(&input.label)?;
    let actor = audit_actor();
    let response = db::serializable::<_, ApiError, _>(&state.pool, move |tx| {
        let (prepared, actor) = (prepared.clone(), actor.clone());
        Box::pin(async move { prepared.commit_on(tx, &actor).await })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(response)))
}

pub(super) async fn revoke_admin_token(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if actor.id == id {
        return Err(ApiError::conflict(
            "cannot revoke the token used by this request",
        ));
    }
    db::serializable::<_, ApiError, _>(&state.pool, move |tx| {
        let (id, actor) = (id.clone(), actor.clone());
        Box::pin(async move { admin_tokens::revoke_on(tx, &actor.id, &id).await })
    })
    .await?;
    state
        .admin_tokens_changed
        .send_modify(|version| *version = version.wrapping_add(1));
    Ok(StatusCode::NO_CONTENT)
}
