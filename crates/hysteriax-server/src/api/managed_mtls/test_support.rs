//! Direct database fixtures, never registered as HTTP endpoints.
use super::*;

pub(crate) async fn managed_transaction<F>(
    pool: &sqlx::PgPool,
    actor: &AdminActor,
    key: &str,
    operation: &str,
    digest: &str,
    write: F,
) -> Result<Value, ApiError>
where
    F: for<'c> FnMut(
        &'c mut Transaction<'static, Postgres>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Value, ApiError>> + Send + 'c>,
    >,
{
    match db::serializable(pool, write).await {
        Err(error) if error.code == "mtls_operation_committed" => {
            let mut tx = crate::read_snapshot::transaction(pool).await?;
            let prior = existing(&mut tx, actor, key, operation, digest).await?;
            tx.commit().await?;
            prior.ok_or(error)
        }
        result => result,
    }
}

pub(crate) async fn create_inner(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Json(input): Json<CreateAuthority>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let prepared = prepare_authority_creation(&state, actor, input).await?;
    if let Some(value) = prepared.replay.clone() {
        return Ok((StatusCode::CREATED, Json(value)));
    }
    let (actor, key, digest) = (
        prepared.actor.clone(),
        prepared.input.idempotency_key.clone(),
        prepared.digest.clone(),
    );
    let result = managed_transaction(&state.pool, &actor, &key, "ca.create", &digest, move |tx| {
        let prepared = prepared.clone();
        Box::pin(async move { create_authority_on(tx, prepared).await })
    })
    .await?;
    Ok((StatusCode::CREATED, Json(result)))
}

pub(crate) async fn update_policy_inner(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    Json(input): Json<UpdatePolicy>,
) -> Result<Json<Value>, ApiError> {
    validate_key(&input.idempotency_key)?;
    input.policy.validate().map_err(bad)?;
    let digest = digest(&(&id, &input))?;

    let pool = state.pool.clone();
    let (request_actor, request_key, request_digest) =
        (actor.clone(), input.idempotency_key.clone(), digest.clone());
    let result = managed_transaction(
        &pool,
        &request_actor,
        &request_key,
        "ca.policy",
        &request_digest,
        move |tx| {
            let (actor, input, digest, id) =
                (actor.clone(), input.clone(), digest.clone(), id.clone());
            Box::pin(async move { update_policy_on(tx, actor, input, digest, id).await })
        },
    )
    .await?;
    Ok(Json(result))
}

pub(crate) async fn publish_inner(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    Json(input): Json<PublishAuthority>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let prepared = prepare_authority_publication(&state, actor, id, input).await?;
    if let Some(value) = prepared.replay.clone() {
        return Ok((StatusCode::ACCEPTED, Json(value)));
    }
    let (actor, key, digest) = (
        prepared.actor.clone(),
        prepared.input.idempotency_key.clone(),
        prepared.digest.clone(),
    );
    let result = managed_transaction(
        &state.pool,
        &actor,
        &key,
        "ca.publish",
        &digest,
        move |tx| {
            let prepared = prepared.clone();
            Box::pin(async move { publish_authority_on(tx, prepared).await })
        },
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

pub(crate) async fn create(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Json(input): Json<CreateAuthority>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let detail = json!({"source":&input.source,"policy":input.policy,"request_key_hash":crate::security::token_digest(&input.idempotency_key)});
    let redactions = audit::redactions(input.certificate.as_deref(), input.private_key.as_deref());
    let operation = create_inner(State(state.clone()), Extension(actor.clone()), Json(input));
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
    Json(input): Json<UpdatePolicy>,
) -> Result<Json<Value>, ApiError> {
    let detail = json!({"expected_policy_revision":input.expected_revision,"policy":input.policy,"request_key_hash":crate::security::token_digest(&input.idempotency_key)});
    let operation = update_policy_inner(
        State(state.clone()),
        Extension(actor.clone()),
        Path(id.clone()),
        Json(input),
    );
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
    Json(input): Json<PublishAuthority>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let detail = json!({"expected_revision":input.expected_revision,"replace_rotation_id":input.replace_rotation_id,"request_key_hash":crate::security::token_digest(&input.idempotency_key)});
    let redactions = audit::redactions(input.certificate.as_deref(), input.private_key.as_deref());
    let operation = publish_inner(
        State(state.clone()),
        Extension(actor.clone()),
        Path(id.clone()),
        Json(input),
    );
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
    Json(input): Json<ReissueUser>,
) -> Result<Json<Value>, ApiError> {
    let detail = json!({"ca_credential_id":input.ca_credential_id,"expected_identity_revisions":input.expected_revisions,"request_key_hash":crate::security::token_digest(&input.idempotency_key)});
    let operation = reissue_user_inner(
        State(state.clone()),
        Extension(actor.clone()),
        Path(user.clone()),
        Json(input),
    );
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

pub(crate) async fn reissue_user_inner(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(user): Path<String>,
    Json(input): Json<ReissueUser>,
) -> Result<Json<Value>, ApiError> {
    let prepared = prepare_user_reissue(&state, actor, user, input).await?;
    if let Some(value) = prepared.replay.clone() {
        return Ok(Json(value));
    }
    let (actor, key, digest) = (
        prepared.actor.clone(),
        prepared.input.idempotency_key.clone(),
        prepared.digest.clone(),
    );
    let app = state.clone();
    let result = managed_transaction(
        &state.pool,
        &actor,
        &key,
        "mtls.reissue",
        &digest,
        move |tx| {
            let (app, prepared) = (app.clone(), prepared.clone());
            Box::pin(async move { reissue_user_on(tx, &app, prepared).await })
        },
    )
    .await?;
    Ok(Json(result))
}
