use super::*;
use crate::api::{
    AdminActor,
    idempotency::{RequestIdentity, StoredResponse, execute, replay},
};
use axum::{
    Extension,
    extract::OriginalUri,
    http::{HeaderMap, Method},
    response::Response,
};

fn parse<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, ApiError> {
    serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid authorization group request"))
}

pub(crate) async fn preview_create(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "groups.preview_create",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let proposal = preview_create_input(parse(body)?)?;
    let token = Uuid::new_v4().to_string();
    let app = state.clone();
    execute(&state, identity, move |tx| {
        let (app, proposal, token) = (app.clone(), proposal.clone(), token.clone());
        Box::pin(async move {
            let Json(value) = preview_create_on(tx, app, proposal, token).await?;
            Ok(StoredResponse::json(StatusCode::OK, value))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn preview_group(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "groups.preview_group",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let (action, expected_revision, proposal) = preview_group_input(parse(body)?)?;
    let token = Uuid::new_v4().to_string();
    let app = state.clone();
    execute(&state, identity, move |tx| {
        let (app, id, action, proposal, token) = (
            app.clone(),
            id.clone(),
            action.clone(),
            proposal.clone(),
            token.clone(),
        );
        Box::pin(async move {
            let Json(value) =
                preview_group_on(tx, app, id, action, expected_revision, proposal, token).await?;
            Ok(StoredResponse::json(StatusCode::OK, value))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn preview_memberships(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(user): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "groups.preview_memberships",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":user,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let (expected_revision, proposal) = preview_memberships_input(parse(body)?)?;
    let token = Uuid::new_v4().to_string();
    execute(&state, identity, move |tx| {
        let (user, proposal, token) = (user.clone(), proposal.clone(), token.clone());
        Box::pin(async move {
            let Json(value) =
                preview_memberships_on(tx, user, proposal, expected_revision, token).await?;
            Ok(StoredResponse::json(StatusCode::OK, value))
        })
    })
    .await?
    .into_http()
}

#[derive(Clone)]
enum GroupChange {
    Create(CreateGroupRequest),
    Update(String, UpdateGroupRequest),
    Delete(String, DeleteGroupRequest),
}
impl GroupChange {
    fn normalize(
        self,
    ) -> Result<(String, Option<String>, Option<i64>, String, GroupProposal), ApiError> {
        Ok(match self {
            Self::Create(input) => (
                "create".into(),
                None,
                None,
                input.preview_token,
                normalized_proposal(GroupProposal {
                    name: input.name,
                    user_ids: input.user_ids,
                    node_ids: input.node_ids,
                })?,
            ),
            Self::Update(id, input) => (
                "update".into(),
                Some(id),
                Some(input.expected_revision),
                input.preview_token,
                normalized_proposal(GroupProposal {
                    name: input.name,
                    user_ids: input.user_ids,
                    node_ids: input.node_ids,
                })?,
            ),
            Self::Delete(id, input) => (
                "delete".into(),
                Some(id),
                Some(input.expected_revision),
                input.preview_token,
                GroupProposal {
                    name: String::new(),
                    user_ids: Vec::new(),
                    node_ids: Vec::new(),
                },
            ),
        })
    }
}

async fn commit_group(
    state: &AppState,
    identity: RequestIdentity,
    change: GroupChange,
) -> Result<Response, ApiError> {
    let (action, id, revision, token, proposal) = change.normalize()?;
    let prepared = match crate::credentials::authorization::prepare_preview(
        &state.pool,
        &state.secrets,
        &token,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            if let Some(response) = replay(state, &identity).await? {
                return response.into_http();
            }
            return Err(ApiError::conflict(error.to_string()));
        }
    };
    let assignments =
        match prepare_group_assignments(state, &action, id.as_deref(), &proposal).await {
            Ok(assignments) => assignments,
            Err(error) => {
                if let Some(response) = replay(state, &identity).await? {
                    return response.into_http();
                }
                return Err(error);
            }
        };
    let status = if action == "create" {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let creation_id = Uuid::new_v4().to_string();
    let app = state.clone();
    execute(state, identity, move |tx| {
        let (app, action, id, token, proposal, prepared, creation_id) = (
            app.clone(),
            action.clone(),
            id.clone(),
            token.clone(),
            proposal.clone(),
            prepared.clone(),
            creation_id.clone(),
        );
        let assignments = assignments.clone();
        Box::pin(async move {
            let value = super::commit_group_on(
                tx,
                &app,
                &action,
                id.as_deref(),
                revision,
                &token,
                proposal,
                prepared,
                &creation_id,
                assignments,
            )
            .await?;
            Ok(StoredResponse::json(status, value))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn create(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "groups.create",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    commit_group(&state, identity, GroupChange::Create(parse(body)?)).await
}

pub(crate) async fn change(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let operation = match method.as_str() {
        "PUT" => "groups.update",
        "DELETE" => "groups.delete",
        _ => return Err(ApiError::not_found("authorization group action")),
    };
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        operation,
        &headers,
        json!({"method":method.as_str(),"uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let change = if method == Method::PUT {
        GroupChange::Update(id, parse(body)?)
    } else {
        GroupChange::Delete(id, parse(body)?)
    };
    commit_group(&state, identity, change).await
}

pub(crate) async fn update_memberships(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(user): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "groups.update_memberships",
        &headers,
        json!({"method":"PUT","uri":uri.to_string(),"id":user,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: MembershipUpdateRequest = parse(body)?;
    if input.expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let proposal = normalized_membership(MembershipProposal {
        group_ids: input.group_ids,
    })?;
    let token = input.preview_token;
    let revision = input.expected_revision;
    let prepared = match crate::credentials::authorization::prepare_preview(
        &state.pool,
        &state.secrets,
        &token,
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            if let Some(response) = replay(&state, &identity).await? {
                return response.into_http();
            }
            return Err(ApiError::conflict(error.to_string()));
        }
    };
    let assignments = match prepare_membership_assignments(&state, &user, &proposal).await {
        Ok(assignments) => assignments,
        Err(error) => {
            if let Some(response) = replay(&state, &identity).await? {
                return response.into_http();
            }
            return Err(error);
        }
    };
    let app = state.clone();
    execute(&state, identity, move |tx| {
        let (app, user, token, proposal, prepared) = (
            app.clone(),
            user.clone(),
            token.clone(),
            proposal.clone(),
            prepared.clone(),
        );
        let assignments = assignments.clone();
        Box::pin(async move {
            let value = super::commit_membership_on(
                tx,
                &app,
                &user,
                revision,
                &token,
                proposal,
                prepared,
                assignments,
            )
            .await?;
            Ok(StoredResponse::json(StatusCode::OK, value))
        })
    })
    .await?
    .into_http()
}

#[cfg(test)]
mod tests;
