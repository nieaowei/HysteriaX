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

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateGroupRequest {
    name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PatchGroupRequest {
    expected_revision: i64,
    #[serde(default, deserialize_with = "present_name")]
    name: Option<String>,
    #[serde(default)]
    add_user_ids: Vec<String>,
    #[serde(default)]
    remove_user_ids: Vec<String>,
    #[serde(default)]
    add_node_ids: Vec<String>,
    #[serde(default)]
    remove_node_ids: Vec<String>,
}

fn present_name<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}

impl PatchGroupRequest {
    fn normalize(mut self) -> Result<Self, ApiError> {
        if self.expected_revision < 1 {
            return Err(ApiError::bad_request("expected_revision must be positive"));
        }
        if let Some(name) = &mut self.name {
            *name = validate_name(name)?.to_owned();
        }
        normalize_ids(&mut self.add_user_ids, "add_user_ids")?;
        normalize_ids(&mut self.remove_user_ids, "remove_user_ids")?;
        normalize_ids(&mut self.add_node_ids, "add_node_ids")?;
        normalize_ids(&mut self.remove_node_ids, "remove_node_ids")?;
        for (add, remove) in [
            (&self.add_user_ids, &self.remove_user_ids),
            (&self.add_node_ids, &self.remove_node_ids),
        ] {
            if add.iter().any(|id| remove.binary_search(id).is_ok()) {
                return Err(ApiError::bad_request(
                    "an ID cannot be both added and removed",
                ));
            }
        }
        if self.name.is_none() && self.relations_unchanged() {
            return Err(ApiError::bad_request(
                "at least one group change is required",
            ));
        }
        Ok(self)
    }

    fn relations_unchanged(&self) -> bool {
        self.add_user_ids.is_empty()
            && self.remove_user_ids.is_empty()
            && self.add_node_ids.is_empty()
            && self.remove_node_ids.is_empty()
    }
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
        json!({"uri":uri.to_string(),"body":body}),
    )?;
    if let Some(result) = replay(&state, &identity).await? {
        return result.into_http();
    }
    let input: CreateGroupRequest = parse(body)?;
    let name = validate_name(&input.name)?.to_owned();
    let id = Uuid::new_v4().to_string();
    execute(&state, identity, move |tx| {
        let (id, name, actor) = (id.clone(), name.clone(), actor.id.clone());
        Box::pin(async move {
            let result =
                super::changes::admit(tx, "group", &id, "create", json!({"name":name}), &actor)
                    .await?;
            Ok(StoredResponse::json(StatusCode::ACCEPTED, result))
        })
    })
    .await?
    .into_http()
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
    let operation = if method == Method::PATCH {
        "groups.update"
    } else {
        "groups.delete"
    };
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        operation,
        &headers,
        json!({"uri":uri.to_string(),"body":body}),
    )?;
    if let Some(result) = replay(&state, &identity).await? {
        return result.into_http();
    }
    let (action, input) = if method == Method::PATCH {
        let input: PatchGroupRequest = parse::<PatchGroupRequest>(body)?.normalize()?;
        (
            "update",
            serde_json::to_value(input).map_err(|_| ApiError::internal())?,
        )
    } else {
        let input: DeleteGroupRequest = parse(body)?;
        (
            "delete",
            json!({"expected_revision":input.expected_revision}),
        )
    };
    execute(&state, identity, move |tx| {
        let (id, input, actor) = (id.clone(), input.clone(), actor.id.clone());
        Box::pin(async move {
            let result = super::changes::admit(tx, "group", &id, action, input, &actor).await?;
            Ok(StoredResponse::json(StatusCode::ACCEPTED, result))
        })
    })
    .await?
    .into_http()
}
pub(crate) async fn update_memberships(
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
        "groups.update_memberships",
        &headers,
        json!({"uri":uri.to_string(),"body":body}),
    )?;
    if let Some(result) = replay(&state, &identity).await? {
        return result.into_http();
    }
    let input: MembershipUpdateRequest = parse(body)?;
    if input.expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let proposal = normalized_membership(MembershipProposal {
        group_ids: input.group_ids,
    })?;
    let body = json!({"expected_revision":input.expected_revision,"group_ids":proposal.group_ids});
    execute(&state, identity, move |tx| {
        let (id, body, actor) = (id.clone(), body.clone(), actor.id.clone());
        Box::pin(async move {
            let result =
                super::changes::admit(tx, "user", &id, "memberships", body, &actor).await?;
            Ok(StoredResponse::json(StatusCode::ACCEPTED, result))
        })
    })
    .await?
    .into_http()
}
