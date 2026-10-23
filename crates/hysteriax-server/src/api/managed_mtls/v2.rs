use super::*;
use crate::api::idempotency::{RequestIdentity, StoredResponse, execute, replay};
use axum::{extract::OriginalUri, http::HeaderMap, response::Response};

fn input<T: serde::de::DeserializeOwned>(
    state: &AppState,
    identity: &RequestIdentity,
    mut body: Value,
) -> Result<T, ApiError> {
    body.as_object_mut()
        .ok_or_else(|| ApiError::bad_request("invalid managed certificate request"))?
        .insert(
            "idempotency_key".into(),
            json!(identity.operation_key(&state.secrets)?),
        );
    serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid managed certificate request"))
}

async fn preparation_error(
    state: &AppState,
    identity: &RequestIdentity,
    error: ApiError,
) -> Result<Response, ApiError> {
    if let Some(response) = replay(state, identity).await? {
        return response.into_http();
    }
    Err(error)
}

async fn legacy_replay(state: &AppState, identity: &RequestIdentity) -> Result<Response, ApiError> {
    preparation_error(state,identity,ApiError::conflict("managed certificate operation exists without this management receipt; inspect its outcome before retrying")).await
}

async fn reject_legacy_on(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminActor,
    key: &str,
    operation: &str,
    digest: &str,
) -> Result<(), ApiError> {
    if existing(tx, actor, key, operation, digest).await?.is_some() {
        return Err(ApiError::conflict(
            "managed certificate operation exists without this management receipt; inspect its outcome before retrying",
        ));
    }
    Ok(())
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
        "mtls.ca.create",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: CreateAuthority = input(&state, &identity, body)?;
    let detail = json!({"source":input.source,"policy":input.policy,"request_key_hash":crate::security::token_digest(&input.idempotency_key)});
    let redactions = audit::redactions(input.certificate.as_deref(), input.private_key.as_deref());
    let operation = async {
        let prepared = match prepare_authority_creation(&state, actor.clone(), input).await {
            Ok(prepared) => prepared,
            Err(error) => return preparation_error(&state, &identity, error).await,
        };
        if prepared.replay.is_some() {
            return legacy_replay(&state, &identity).await;
        }
        execute(&state, identity.clone(), move |tx| {
            let prepared = prepared.clone();
            Box::pin(async move {
                reject_legacy_on(
                    tx,
                    &prepared.actor,
                    &prepared.input.idempotency_key,
                    "ca.create",
                    &prepared.digest,
                )
                .await?;
                Ok(StoredResponse::json(
                    StatusCode::CREATED,
                    create_authority_on(tx, prepared).await?,
                ))
            })
        })
        .await?
        .into_http()
    };
    audit::request(
        &state,
        &actor,
        "ca.create_request_failed",
        ("credential", "new"),
        detail,
        redactions,
        operation,
    )
    .await
}

pub(crate) async fn update_policy(
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
        "mtls.ca.policy",
        &headers,
        json!({"method":"PATCH","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: UpdatePolicy = input(&state, &identity, body)?;
    let detail = json!({"expected_policy_revision":input.expected_revision,"policy":input.policy,"request_key_hash":crate::security::token_digest(&input.idempotency_key)});
    let operation = async {
        input.policy.validate().map_err(bad)?;
        let digest = digest(&(&id, &input))?;
        let (id, actor) = (id.clone(), actor.clone());
        execute(&state, identity.clone(), move |tx| {
            let (id, actor, input, digest) =
                (id.clone(), actor.clone(), input.clone(), digest.clone());
            Box::pin(async move {
                reject_legacy_on(tx, &actor, &input.idempotency_key, "ca.policy", &digest).await?;
                Ok(StoredResponse::json(
                    StatusCode::OK,
                    update_policy_on(tx, actor, input, digest, id).await?,
                ))
            })
        })
        .await?
        .into_http()
    };
    audit::request(
        &state,
        &actor,
        "ca.policy_request_failed",
        ("credential", &id),
        detail,
        vec![],
        operation,
    )
    .await
}

pub(crate) async fn publish(
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
        "mtls.ca.publish",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: PublishAuthority = input(&state, &identity, body)?;
    let detail = json!({"expected_revision":input.expected_revision,"replace_rotation_id":input.replace_rotation_id,"request_key_hash":crate::security::token_digest(&input.idempotency_key)});
    let redactions = audit::redactions(input.certificate.as_deref(), input.private_key.as_deref());
    let operation = async {
        let prepared =
            match prepare_authority_publication(&state, actor.clone(), id.clone(), input).await {
                Ok(prepared) => prepared,
                Err(error) => return preparation_error(&state, &identity, error).await,
            };
        if prepared.replay.is_some() {
            return legacy_replay(&state, &identity).await;
        }
        execute(&state, identity.clone(), move |tx| {
            let prepared = prepared.clone();
            Box::pin(async move {
                reject_legacy_on(
                    tx,
                    &prepared.actor,
                    &prepared.input.idempotency_key,
                    "ca.publish",
                    &prepared.digest,
                )
                .await?;
                Ok(StoredResponse::json(
                    StatusCode::ACCEPTED,
                    publish_authority_on(tx, prepared).await?,
                ))
            })
        })
        .await?
        .into_http()
    };
    audit::request(
        &state,
        &actor,
        "ca.publish_request_failed",
        ("credential", &id),
        detail,
        redactions,
        operation,
    )
    .await
}

pub(crate) async fn reissue_user(
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
        "mtls.user.reissue",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":user,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: ReissueUser = input(&state, &identity, body)?;
    let detail = json!({"ca_credential_id":input.ca_credential_id,"expected_identity_revisions":input.expected_revisions,"request_key_hash":crate::security::token_digest(&input.idempotency_key)});
    let operation = async {
        let prepared = match prepare_user_reissue(&state, actor.clone(), user.clone(), input).await
        {
            Ok(prepared) => prepared,
            Err(error) => return preparation_error(&state, &identity, error).await,
        };
        if prepared.replay.is_some() {
            return legacy_replay(&state, &identity).await;
        }
        let app = state.clone();
        execute(&state, identity.clone(), move |tx| {
            let (prepared, app) = (prepared.clone(), app.clone());
            Box::pin(async move {
                reject_legacy_on(
                    tx,
                    &prepared.actor,
                    &prepared.input.idempotency_key,
                    "mtls.reissue",
                    &prepared.digest,
                )
                .await?;
                Ok(StoredResponse::json(
                    StatusCode::OK,
                    reissue_user_on(tx, &app, prepared).await?,
                ))
            })
        })
        .await?
        .into_http()
    };
    audit::request(
        &state,
        &actor,
        "ca.reissue_request_failed",
        ("user", &user),
        detail,
        vec![],
        operation,
    )
    .await
}

#[cfg(test)]
mod tests;
