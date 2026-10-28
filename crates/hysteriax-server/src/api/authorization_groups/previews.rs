use super::*;

pub(super) fn preview_create_input(input: GroupPreviewRequest) -> Result<GroupProposal, ApiError> {
    if input.action != "create" {
        return Err(ApiError::bad_request(
            "create preview requires action=create",
        ));
    }
    if input.expected_revision.is_some() {
        return Err(ApiError::bad_request(
            "expected_revision is not valid when creating a group",
        ));
    }
    let proposal = proposal_from_preview(input)?;
    Ok(proposal)
}

pub(super) async fn preview_create_on(
    tx: &mut Transaction<'_, Postgres>,
    state: AppState,
    proposal: GroupProposal,
    token: String,
) -> Result<Json<Value>, ApiError> {
    validate_proposal_entities(tx, &proposal).await?;
    let affected = proposal.user_ids.clone();
    let old_pairs = effective_pairs(tx, &affected, None).await?;
    let mut new_pairs = old_pairs.clone();
    add_group_pairs(&mut new_pairs, &proposal.user_ids, &proposal.node_ids);
    let (guard, result, request_json) = preview_data(
        &state, tx, "create", None, None, &affected, &proposal, &old_pairs, &new_pairs,
    )
    .await?;
    let token = store_preview(
        tx,
        "create",
        None,
        None,
        request_json,
        guard,
        &result,
        &token,
    )
    .await?;
    Ok(Json(preview_json("create", token, result)))
}

pub(super) fn preview_group_input(
    input: GroupPreviewRequest,
) -> Result<(String, Option<i64>, Option<GroupProposal>), ApiError> {
    if !matches!(input.action.as_str(), "update" | "delete") {
        return Err(ApiError::bad_request(
            "group preview action must be update or delete",
        ));
    }
    let action = input.action.clone();
    let expected_revision = input.expected_revision;
    if expected_revision.is_none() {
        return Err(ApiError::bad_request("expected_revision is required"));
    }
    let proposal = if action == "update" {
        Some(proposal_from_preview(input.clone())?)
    } else {
        if input.name.is_some() || input.user_ids.is_some() || input.node_ids.is_some() {
            return Err(ApiError::bad_request(
                "delete preview accepts only action and expected_revision",
            ));
        }
        None
    };
    Ok((action, expected_revision, proposal))
}

pub(super) async fn preview_group_on(
    tx: &mut Transaction<'_, Postgres>,
    state: AppState,
    group_id: String,
    action: String,
    expected_revision: Option<i64>,
    proposal: Option<GroupProposal>,
    token: String,
) -> Result<Json<Value>, ApiError> {
    let current = load_group_tx(tx, &group_id)
        .await?
        .ok_or_else(|| ApiError::not_found("authorization group"))?;
    check_revision(current.revision, expected_revision)?;
    if let Some(proposal) = &proposal {
        validate_proposal_entities(tx, proposal).await?;
    }
    let nodes_changed = proposal
        .as_ref()
        .is_some_and(|proposal| proposal.node_ids != current.node_ids);
    let mut affected = current.user_ids.clone();
    if let Some(proposal) = &proposal {
        affected.extend(proposal.user_ids.clone());
    }
    sort_dedup(&mut affected);
    let old_pairs = effective_pairs(tx, &affected, None).await?;
    let mut new_pairs = effective_pairs(tx, &affected, Some(&group_id)).await?;
    if let Some(proposal) = &proposal {
        add_group_pairs(&mut new_pairs, &proposal.user_ids, &proposal.node_ids);
    }
    let proposal_for_snapshot = proposal.unwrap_or(GroupProposal {
        name: current.name.clone(),
        user_ids: Vec::new(),
        node_ids: Vec::new(),
    });
    let (guard, result, request_json) = preview_data(
        &state,
        tx,
        &action,
        Some(&group_id),
        Some(expected_revision.unwrap_or_default()),
        &affected,
        &proposal_for_snapshot,
        &old_pairs,
        &new_pairs,
    )
    .await?;
    let token = store_preview(
        tx,
        &action,
        Some(&group_id),
        None,
        request_json,
        guard,
        &result,
        &token,
    )
    .await?;
    Ok(Json(if nodes_changed {
        node_change_preview_json(&action, token, result)
    } else {
        preview_json(&action, token, result)
    }))
}

pub(super) fn preview_memberships_input(
    input: MembershipPreviewRequest,
) -> Result<(i64, MembershipProposal), ApiError> {
    if input.expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let proposal = normalized_membership(MembershipProposal {
        group_ids: input.group_ids,
    })?;
    let expected_revision = input.expected_revision;
    Ok((expected_revision, proposal))
}

pub(super) async fn preview_memberships_on(
    tx: &mut Transaction<'_, Postgres>,
    user_id: String,
    proposal: MembershipProposal,
    expected_revision: i64,
    token: String,
) -> Result<Json<Value>, ApiError> {
    let revision = user_revision(tx, &user_id).await?;
    check_revision(revision, Some(expected_revision))?;
    validate_group_ids(tx, &proposal.group_ids).await?;
    let old_pairs = effective_pairs(tx, std::slice::from_ref(&user_id), None).await?;
    let new_pairs = pairs_for_user_groups(tx, &user_id, &proposal.group_ids).await?;
    let guard = guard_snapshot(
        tx,
        std::slice::from_ref(&user_id),
        None,
        &proposal.group_ids,
        &[],
    )
    .await?;
    let (additions, removals) = pair_diff(&old_pairs, &new_pairs);
    let request_json = json!({
        "expected_revision": expected_revision,
        "group_ids": proposal.group_ids,
    });
    let result = with_plan(
        diff_json(&additions, &removals),
        crate::credentials::authorization::plan_on(tx, &additions).await?,
    );
    let token = store_preview(
        tx,
        "update_memberships",
        None,
        Some(&user_id),
        request_json,
        guard,
        &result,
        &token,
    )
    .await?;
    Ok(Json(preview_json("update_memberships", token, result)))
}
