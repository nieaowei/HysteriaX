//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn rotate(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<RotateSubscription>,
) -> Result<Json<Value>, ApiError> {
    let prepared = rotation::Prepared::new(&state.secrets)?;
    let actor = super::super::audit_actor();
    let response = db::serializable(&state.pool, move |tx| {
        let (prepared, user_id, actor) = (prepared.clone(), user_id.clone(), actor.clone());
        Box::pin(async move {
            prepared
                .commit_on(tx, &user_id, input.expected_revision, &actor)
                .await
        })
    })
    .await?;
    Ok(Json(response))
}
