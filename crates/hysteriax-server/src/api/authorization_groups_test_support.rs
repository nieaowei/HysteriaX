//! Ready authorization fixtures; HTTP tests explicitly exercise queued responses.
use super::*;
async fn finish(
    state: &AppState,
    target_type: &str,
    target: &str,
    action: &str,
    input: Value,
) -> Result<Value, ApiError> {
    let mut tx = state.pool.begin().await?;
    let result = Box::pin(changes::admit(
        &mut tx,
        target_type,
        target,
        action,
        input,
        "fixture",
    ))
    .await?;
    tx.commit().await?;
    Box::pin(changes::finish_for_test(
        state,
        result["job_id"].as_str().unwrap(),
    ))
    .await;
    Ok(result)
}
pub(crate) async fn create(
    State(state): State<AppState>,
    Json(input): Json<FixtureGroupCreate>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let name = validate_name(&input.name)?;
    let id = Uuid::new_v4().to_string();
    let result = finish(
        &state,
        "group",
        &id,
        "create",
        json!({"name":name,"add_user_ids":input.user_ids,"add_node_ids":input.node_ids}),
    )
    .await?;
    crate::connection_credentials::migrate(&state.pool, &state.secrets).await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
pub(crate) async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<DeleteGroupRequest>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        finish(
            &state,
            "group",
            &id,
            "delete",
            json!({"expected_revision":input.expected_revision}),
        )
        .await?,
    ))
}
async fn group_named_by_id(state: &AppState, id: &str) -> Value {
    sqlx::query_scalar("SELECT to_jsonb(g)||jsonb_build_object('user_ids',ARRAY(SELECT user_id FROM authorization_group_users WHERE group_id=g.id ORDER BY user_id),'node_ids',ARRAY(SELECT node_id FROM authorization_group_nodes WHERE group_id=g.id ORDER BY node_id)) FROM authorization_groups g WHERE id=$1")
        .bind(id).fetch_one(&state.pool).await.unwrap()
}
pub(crate) async fn group_named(state: &AppState, name: &str) -> Value {
    let id: String = sqlx::query_scalar("SELECT id FROM authorization_groups WHERE name=$1")
        .bind(name)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    group_named_by_id(state, &id).await
}
