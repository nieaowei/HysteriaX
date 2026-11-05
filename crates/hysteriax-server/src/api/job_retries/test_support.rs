//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn retry(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<RetryRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let actor = super::super::audit_actor();
    db::serializable(&state.pool, move |tx| {
        let (id, input, actor) = (id.clone(), input.clone(), actor.clone());
        Box::pin(async move { retry_on(tx, id, input, actor).await })
    })
    .await
}
