//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn create(
    State(state): State<AppState>,
    AxumPath(node): AxumPath<String>,
    Json(input): Json<CreateResource>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let prepared = mutations::Prepared::new(&state.secrets, input)?;
    let actor = super::super::audit_actor();
    let response = db::serializable(&state.pool, move |tx| {
        let (prepared, node, actor) = (prepared.clone(), node.clone(), actor.clone());
        Box::pin(async move { prepared.commit_on(tx, &node, &actor).await })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(response)))
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    AxumPath((node, resource)): AxumPath<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let actor = super::super::audit_actor();
    db::serializable(&state.pool, move |tx| {
        let (node, resource, actor) = (node.clone(), resource.clone(), actor.clone());
        Box::pin(async move { mutations::delete_on(tx, &node, &resource, &actor).await })
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
