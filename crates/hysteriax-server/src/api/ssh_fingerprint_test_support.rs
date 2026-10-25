//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn confirm(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<Confirmation>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let pool = state.pool.clone();
    let actor = super::super::audit_actor();
    let history_id = Uuid::new_v4().to_string();
    db::serializable(&pool, move |tx| {
        let (state, id, input, actor) = (state.clone(), id.clone(), input.clone(), actor.clone());
        let history_id = history_id.clone();
        Box::pin(async move { confirm_on(tx, &state, id, input, actor, history_id).await })
    })
    .await
}
