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

#[derive(Clone, Debug, Deserialize)]
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

    fn apply(self, current: GroupInfo) -> GroupProposal {
        fn apply_ids(current: Vec<String>, add: Vec<String>, remove: Vec<String>) -> Vec<String> {
            let mut ids: BTreeSet<String> = current.into_iter().collect();
            for id in remove {
                ids.remove(&id);
            }
            ids.extend(add);
            ids.into_iter().collect()
        }
        GroupProposal {
            name: self.name.unwrap_or(current.name),
            user_ids: apply_ids(current.user_ids, self.add_user_ids, self.remove_user_ids),
            node_ids: apply_ids(current.node_ids, self.add_node_ids, self.remove_node_ids),
        }
    }
}

#[derive(Clone)]
enum GroupChange {
    Update(String, PatchGroupRequest),
    Delete(String, DeleteGroupRequest),
}
impl GroupChange {
    async fn normalize(
        self,
        state: &AppState,
    ) -> Result<(String, Option<String>, Option<i64>, GroupProposal), ApiError> {
        Ok(match self {
            Self::Update(id, input) => {
                let current = load_group(&state.pool, &id)
                    .await?
                    .ok_or_else(|| ApiError::not_found("authorization group"))?;
                check_revision(current.revision, Some(input.expected_revision))?;
                let revision = input.expected_revision;
                (
                    "update".into(),
                    Some(id),
                    Some(revision),
                    input.apply(current),
                )
            }
            Self::Delete(id, input) => (
                "delete".into(),
                Some(id),
                Some(input.expected_revision),
                GroupProposal {
                    name: String::new(),
                    user_ids: Vec::new(),
                    node_ids: Vec::new(),
                },
            ),
        })
    }
}

// Creating an empty group and renaming one never touch authorization material.
async fn commit_metadata(
    state: &AppState,
    identity: RequestIdentity,
    id: String,
    expected_revision: Option<i64>,
    name: String,
) -> Result<Response, ApiError> {
    let status = if expected_revision.is_none() {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    execute(state, identity, move |tx| {
        let (id, name) = (id.clone(), name.clone());
        Box::pin(async move {
            let timestamp = now();
            let detail = if let Some(expected) = expected_revision {
                let row = sqlx::query("SELECT name,revision FROM authorization_groups WHERE id=$1 FOR UPDATE")
                    .bind(&id).fetch_optional(&mut **tx).await?
                    .ok_or_else(|| ApiError::not_found("authorization group"))?;
                check_revision(row.get("revision"), Some(expected))?;
                let old: String = row.get("name");
                if old != name {
                    sqlx::query("UPDATE authorization_groups SET name=$2,revision=revision+1,updated_at=$3 WHERE id=$1")
                        .bind(&id).bind(&name).bind(timestamp).execute(&mut **tx).await?;
                }
                json!({"old_name":old,"name":name})
            } else {
                sqlx::query("INSERT INTO authorization_groups(id,name,revision,created_at,updated_at) VALUES($1,$2,1,$3,$3)")
                    .bind(&id).bind(&name).bind(timestamp).execute(&mut **tx).await?;
                json!({"name":name})
            };
            audit_group(tx, if expected_revision.is_none() { "authorization_group.created" } else { "authorization_group.updated" }, &id, detail, timestamp).await?;
            Ok(StoredResponse::json(status, json!({"revocation_job_ids":[]})))
        })
    }).await?.into_http()
}

async fn commit_group(
    state: &AppState,
    identity: RequestIdentity,
    change: GroupChange,
) -> Result<Response, ApiError> {
    let started = std::time::Instant::now();
    let (action, id, revision, proposal) = match change.normalize(state).await {
        Ok(change) => change,
        Err(error) => {
            if let Some(response) = replay(state, &identity).await? {
                return response.into_http();
            }
            return Err(error);
        }
    };
    let preparation = prepare_group_change(state, &action, id.as_deref(), &proposal).await;
    let (prepared, assignments) = match preparation {
        Ok(prepared) => prepared,
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
    let preparation_ms = started.elapsed().as_millis() as u64;
    let operation = action.clone();
    let execution_started = std::time::Instant::now();
    let creation_id = Uuid::new_v4().to_string();
    let app = state.clone();
    let result = execute(state, identity, move |tx| {
        let (app, action, id, proposal, prepared, creation_id) = (
            app.clone(),
            action.clone(),
            id.clone(),
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
                proposal,
                prepared,
                &creation_id,
                assignments,
            )
            .await?;
            Ok(StoredResponse::json(status, value))
        })
    })
    .await;
    tracing::info!(
        operation,
        preparation_ms,
        transaction_ms = execution_started.elapsed().as_millis() as u64,
        total_ms = started.elapsed().as_millis() as u64,
        success = result.is_ok(),
        "authorization group mutation finished"
    );
    result?.into_http()
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
    let input: CreateGroupRequest = parse(body)?;
    let name = validate_name(&input.name)?.to_owned();
    commit_metadata(&state, identity, Uuid::new_v4().to_string(), None, name).await
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
        "PATCH" => "groups.update",
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
    let change = if method == Method::PATCH {
        let input: PatchGroupRequest = parse::<PatchGroupRequest>(body)?.normalize()?;
        if input.relations_unchanged() {
            return commit_metadata(
                &state,
                identity,
                id,
                Some(input.expected_revision),
                input.name.expect("validated name"),
            )
            .await;
        }
        GroupChange::Update(id, input)
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
    let revision = input.expected_revision;
    let (prepared, assignments) = match prepare_membership_change(&state, &user, &proposal).await {
        Ok(prepared) => prepared,
        Err(error) => {
            if let Some(response) = replay(&state, &identity).await? {
                return response.into_http();
            }
            return Err(error);
        }
    };
    let app = state.clone();
    execute(&state, identity, move |tx| {
        let (app, user, proposal, prepared) = (
            app.clone(),
            user.clone(),
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
