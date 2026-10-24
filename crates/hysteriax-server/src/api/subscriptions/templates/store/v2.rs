use super::*;
use crate::api::{
    AdminActor,
    idempotency::{RequestIdentity, StoredResponse, execute, replay},
};
use axum::{Extension, extract::OriginalUri, http::HeaderMap};

fn parse<T: serde::de::DeserializeOwned>(body: Value) -> Result<T, ApiError> {
    serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid subscription template request"))
}

async fn finish(
    state: &AppState,
    identity: &RequestIdentity,
    id: &str,
    action: &str,
    revision: i64,
    version: Option<i64>,
    result: Result<StoredResponse, ApiError>,
) -> Result<Response, ApiError> {
    let result = match result {
        Err(error) => match replay(state, identity).await? {
            Some(response) => Ok(response),
            None => Err(error),
        },
        result => result,
    };
    report_failure(state, id, action, revision, version, result)
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
        "templates.create",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: Create = parse(body)?;
    let revision = input.expected_revision;
    let result = async {
        let prepared = PreparedCreation::new(&state, input)?;
        execute(&state, identity.clone(), move |tx| {
            let prepared = prepared.clone();
            Box::pin(async move {
                let (status, Json(value)) = prepared.commit_on(tx).await?;
                Ok(StoredResponse::json(status, value))
            })
        })
        .await
    }
    .await;
    finish(
        &state,
        &identity,
        "new",
        "template.create_failed",
        revision,
        None,
        result,
    )
    .await
}

pub(crate) async fn patch(
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
        "templates.patch",
        &headers,
        json!({"method":"PATCH","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: Patch = parse(body)?;
    let action = if input.archived == Some(true) {
        "template.archive_failed"
    } else if input.is_default == Some(true) {
        "template.default_change_failed"
    } else {
        "template.metadata_update_failed"
    };
    let revision = input.expected_revision;
    let target = id.clone();
    let result = execute(&state, identity.clone(), move |tx| {
        let (id, input) = (id.clone(), input.clone());
        Box::pin(async move {
            let Json(value) = patch_on(tx, id, input).await?;
            Ok(StoredResponse::json(StatusCode::OK, value))
        })
    })
    .await;
    finish(&state, &identity, &target, action, revision, None, result).await
}

pub(crate) async fn save(
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
        "templates.save",
        &headers,
        json!({"method":"PUT","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: Save = parse(body)?;
    let revision = input.expected_revision;
    let target = id.clone();
    let result = async {
        let encrypted = prepare_save(&state, &input)?;
        execute(&state, identity.clone(), move |tx| {
            let (id, encrypted) = (id.clone(), encrypted.clone());
            Box::pin(async move {
                let Json(value) = save_on(tx, id, revision, encrypted).await?;
                Ok(StoredResponse::json(StatusCode::OK, value))
            })
        })
        .await
    }
    .await;
    finish(
        &state,
        &identity,
        &target,
        "template.draft_save_failed",
        revision,
        None,
        result,
    )
    .await
}

pub(crate) async fn publication(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let rollback = match uri.path().rsplit('/').next() {
        Some("publish") => false,
        Some("rollback") => true,
        _ => return Err(ApiError::not_found("template publication")),
    };
    let kind = if rollback { "rollback" } else { "publish" };
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        &format!("templates.{kind}"),
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let (revision, version) = if rollback {
        let input: Rollback = parse(body)?;
        (input.expected_revision, Some(input.version))
    } else {
        (parse::<Revision>(body)?.expected_revision, None)
    };
    let result = async {
        let prepared = PreparedPublication::new(&state, &id, revision, version).await?;
        execute(&state, identity.clone(), move |tx| {
            let prepared = prepared.clone();
            Box::pin(async move {
                let Json(value) = prepared.commit_on(tx).await?;
                Ok(StoredResponse::json(StatusCode::OK, value))
            })
        })
        .await
    }
    .await;
    finish(
        &state,
        &identity,
        &id,
        &format!("template.{kind}_failed"),
        revision,
        version,
        result,
    )
    .await
}

async fn persist_computation(
    state: &AppState,
    identity: RequestIdentity,
    result: Result<Value, ApiError>,
) -> Result<Response, ApiError> {
    let value = match result {
        Ok(value) => value,
        Err(error) => {
            if let Some(response) = replay(state, &identity).await? {
                return response.into_http();
            }
            return Err(error);
        }
    };
    let response = StoredResponse::json(StatusCode::OK, value);
    execute(state, identity, move |_tx| {
        let response = response.clone();
        Box::pin(async move { Ok(response) })
    })
    .await?
    .into_http()
}

pub(crate) async fn compute_source(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let kind = match uri.path().rsplit('/').next() {
        Some("source") => "source",
        Some("import") => "import",
        _ => return Err(ApiError::not_found("template source action")),
    };
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        &format!("templates.{kind}"),
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let result = if kind == "source" {
        source_value(parse(body)?)
    } else {
        import_value(parse(body)?)
    };
    persist_computation(&state, identity, result).await
}

pub(crate) async fn compute_template(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let kind = match uri.path().rsplit('/').next() {
        Some("validate") => "validate",
        Some("refactor") => "refactor",
        Some("preview") => "preview",
        _ => return Err(ApiError::not_found("template computation")),
    };
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        &format!("templates.{kind}"),
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    // A computation is the historical artifact of one coherent source snapshot.
    // Its receipt is saved after that read scope closes, without a nested write.
    let result = crate::read_snapshot::scope(&state.pool, async {
        match kind {
            "validate" => validate_draft_value(&state, &id, parse(body)?).await,
            "refactor" => refactor_value(&state, &id, parse(body)?).await,
            _ => preview_value(&state, &id, parse(body)?).await,
        }
    })
    .await;
    persist_computation(&state, identity, result).await
}

#[cfg(test)]
mod tests;
