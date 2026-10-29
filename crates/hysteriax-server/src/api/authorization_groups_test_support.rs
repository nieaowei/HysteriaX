//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn create(
    State(state): State<AppState>,
    Json(input): Json<FixtureGroupCreate>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let proposal = normalized_proposal(GroupProposal {
        name: input.name,
        user_ids: input.user_ids,
        node_ids: input.node_ids,
    })?;
    let response = commit_group_mutation(&state, "create", None, None, proposal).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

pub(crate) async fn update(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
    Json(input): Json<FixtureGroupReplace>,
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
        proposal,
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn update_memberships(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<MembershipUpdateRequest>,
) -> Result<Json<Value>, ApiError> {
    let proposal = normalized_membership(MembershipProposal {
        group_ids: input.group_ids,
    })?;
    let response = commit_membership(&state, &user_id, input.expected_revision, proposal).await?;
    Ok(Json(response))
}

async fn commit_group_mutation(
    state: &AppState,
    action: &str,
    group_id: Option<&str>,
    expected_revision: Option<i64>,
    proposal: GroupProposal,
) -> Result<Value, ApiError> {
    let (prepared, assignments) = prepare_group_change(state, action, group_id, &proposal).await?;
    let creation_id = Uuid::new_v4().to_string();
    let pool = state.pool.clone();
    let (state, action, group_id) = (
        state.clone(),
        action.to_owned(),
        group_id.map(str::to_owned),
    );
    db::serializable(&pool, move |tx| {
        let (state, action, group_id, proposal, prepared) = (
            state.clone(),
            action.clone(),
            group_id.clone(),
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
    proposal: MembershipProposal,
) -> Result<Value, ApiError> {
    if expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let (prepared, assignments) = prepare_membership_change(state, user_id, &proposal).await?;
    let pool = state.pool.clone();
    let (state, user_id) = (state.clone(), user_id.to_owned());
    db::serializable(&pool, move |tx| {
        let (state, user_id, proposal, prepared) = (
            state.clone(),
            user_id.clone(),
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
                proposal,
                prepared,
                assignments,
            )
            .await
        })
    })
    .await
}

/// Exercise the same identity admission/commit boundary as direct HTTP mutations.
pub(crate) async fn create_prepared(
    state: &AppState,
    input: FixtureGroupCreate,
    prepared: crate::credentials::authorization::Prepared,
) -> Result<Value, ApiError> {
    let proposal = normalized_proposal(GroupProposal {
        name: input.name,
        user_ids: input.user_ids,
        node_ids: input.node_ids,
    })?;
    let assignments = prepare_group_assignments(state, "create", None, &proposal).await?;
    let id = Uuid::new_v4().to_string();
    let app = state.clone();
    db::serializable(&state.pool, move |tx| {
        let (app, proposal, prepared, assignments, id) = (
            app.clone(),
            proposal.clone(),
            prepared.clone(),
            assignments.clone(),
            id.clone(),
        );
        Box::pin(async move {
            commit_group_on(
                tx,
                &app,
                "create",
                None,
                None,
                proposal,
                prepared,
                &id,
                assignments,
            )
            .await
        })
    })
    .await
}

pub(crate) async fn group_named(state: &AppState, name: &str) -> Value {
    let id: String = sqlx::query_scalar("SELECT id FROM authorization_groups WHERE name=$1")
        .bind(name)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    group_json(&load_group(&state.pool, &id).await.unwrap().unwrap())
}
