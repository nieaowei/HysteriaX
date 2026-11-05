//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn update_usage(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UsageUpdate>,
) -> Result<Json<Value>, ApiError> {
    let (secrets, actor) = (state.secrets.clone(), crate::api::audit_actor());
    let response = db::serializable(&state.pool, move |tx| {
        let (id, input, secrets, actor) =
            (id.clone(), input.clone(), secrets.clone(), actor.clone());
        Box::pin(async move { update_usage_on(tx, id, input, secrets, actor).await })
    })
    .await?;
    Ok(Json(response))
}
