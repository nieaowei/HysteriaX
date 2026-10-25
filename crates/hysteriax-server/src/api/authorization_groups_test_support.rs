//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn preview_create(
    State(state): State<AppState>,
    Json(input): Json<GroupPreviewRequest>,
) -> Result<Json<Value>, ApiError> {
    let proposal = preview_create_input(input)?;
    let token = Uuid::new_v4().to_string();
    let pool = state.pool.clone();
    db::serializable(&pool, move |tx| {
        let (state, proposal) = (state.clone(), proposal.clone());
        let token = token.clone();
        Box::pin(async move { preview_create_on(tx, state, proposal, token).await })
    })
    .await
}

pub(crate) async fn preview_group(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
    Json(input): Json<GroupPreviewRequest>,
) -> Result<Json<Value>, ApiError> {
    let (action, expected_revision, proposal) = preview_group_input(input)?;
    let token = Uuid::new_v4().to_string();
    let pool = state.pool.clone();
    db::serializable(&pool, move |tx| {
        let (state, group_id, action, proposal) = (
            state.clone(),
            group_id.clone(),
            action.clone(),
            proposal.clone(),
        );
        let token = token.clone();
        Box::pin(async move {
            preview_group_on(
                tx,
                state,
                group_id,
                action,
                expected_revision,
                proposal,
                token,
            )
            .await
        })
    })
    .await
}

pub(crate) async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateGroupRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let proposal = normalized_proposal(GroupProposal {
        name: input.name,
        user_ids: input.user_ids,
        node_ids: input.node_ids,
    })?;
    let response =
        commit_group_mutation(&state, "create", None, None, &input.preview_token, proposal).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

pub(crate) async fn update(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
    Json(input): Json<UpdateGroupRequest>,
) -> Result<Json<Value>, ApiError> {
    let proposal = normalized_proposal(GroupProposal {
        name: input.name,
        user_ids: input.user_ids,
        node_ids: input.node_ids,
    })?;
    let response = commit_group_mutation(
        &state,
        "update",
        Some(&group_id),
        Some(input.expected_revision),
        &input.preview_token,
        proposal,
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
    Json(input): Json<DeleteGroupRequest>,
) -> Result<Json<Value>, ApiError> {
    let proposal = GroupProposal {
        name: String::new(),
        user_ids: Vec::new(),
        node_ids: Vec::new(),
    };
    let response = commit_group_mutation(
        &state,
        "delete",
        Some(&group_id),
        Some(input.expected_revision),
        &input.preview_token,
        proposal,
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn preview_memberships(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<MembershipPreviewRequest>,
) -> Result<Json<Value>, ApiError> {
    let (expected_revision, proposal) = preview_memberships_input(input)?;
    let token = Uuid::new_v4().to_string();
    let pool = state.pool.clone();
    db::serializable(&pool, move |tx| {
        let (user_id, proposal) = (user_id.clone(), proposal.clone());
        let token = token.clone();
        Box::pin(async move {
            preview_memberships_on(tx, user_id, proposal, expected_revision, token).await
        })
    })
    .await
}

pub(crate) async fn update_memberships(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<MembershipUpdateRequest>,
) -> Result<Json<Value>, ApiError> {
    let proposal = normalized_membership(MembershipProposal {
        group_ids: input.group_ids,
    })?;
    let response = commit_membership(
        &state,
        &user_id,
        input.expected_revision,
        &input.preview_token,
        proposal,
    )
    .await?;
    Ok(Json(response))
}

async fn commit_group_mutation(
    state: &AppState,
    action: &str,
    group_id: Option<&str>,
    expected_revision: Option<i64>,
    preview_token: &str,
    proposal: GroupProposal,
) -> Result<Value, ApiError> {
    let prepared = crate::credentials::authorization::prepare_preview(
        &state.pool,
        &state.secrets,
        preview_token,
    )
    .await
    .map_err(|e| ApiError::conflict(e.to_string()))?;
    let assignments = prepare_group_assignments(state, action, group_id, &proposal).await?;
    let creation_id = Uuid::new_v4().to_string();
    let pool = state.pool.clone();
    let (state, action, group_id, preview_token) = (
        state.clone(),
        action.to_owned(),
        group_id.map(str::to_owned),
        preview_token.to_owned(),
    );
    db::serializable(&pool, move |tx| {
        let (state, action, group_id, preview_token, proposal, prepared) = (
            state.clone(),
            action.clone(),
            group_id.clone(),
            preview_token.clone(),
            proposal.clone(),
            prepared.clone(),
        );
        let creation_id = creation_id.clone();
        let assignments = assignments.clone();
        Box::pin(async move {
            commit_group_on(
                tx,
                &state,
                &action,
                group_id.as_deref(),
                expected_revision,
                &preview_token,
                proposal,
                prepared,
                &creation_id,
                assignments,
            )
            .await
        })
    })
    .await
}

async fn commit_membership(
    state: &AppState,
    user_id: &str,
    expected_revision: i64,
    preview_token: &str,
    proposal: MembershipProposal,
) -> Result<Value, ApiError> {
    if expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let prepared = crate::credentials::authorization::prepare_preview(
        &state.pool,
        &state.secrets,
        preview_token,
    )
    .await
    .map_err(|e| ApiError::conflict(e.to_string()))?;
    let assignments = prepare_membership_assignments(state, user_id, &proposal).await?;
    let pool = state.pool.clone();
    let (state, user_id, preview_token) =
        (state.clone(), user_id.to_owned(), preview_token.to_owned());
    db::serializable(&pool, move |tx| {
        let (state, user_id, preview_token, proposal, prepared) = (
            state.clone(),
            user_id.clone(),
            preview_token.clone(),
            proposal.clone(),
            prepared.clone(),
        );
        let assignments = assignments.clone();
        Box::pin(async move {
            commit_membership_on(
                tx,
                &state,
                &user_id,
                expected_revision,
                &preview_token,
                proposal,
                prepared,
                assignments,
            )
            .await
        })
    })
    .await
}
