use super::*;

#[derive(Clone)]
struct AuthorityCreationMaterial {
    id: String,
    ciphertext: String,
    metadata: Value,
    expires_at: DateTime<Utc>,
    timestamp: DateTime<Utc>,
}

#[derive(Clone)]
pub(super) struct PreparedAuthorityCreation {
    pub(super) actor: AdminActor,
    pub(super) input: CreateAuthority,
    pub(super) digest: String,
    pub(super) replay: Option<Value>,
    name: String,
    source: &'static str,
    material: Option<AuthorityCreationMaterial>,
}

pub(super) async fn prepare_authority_creation(
    state: &AppState,
    actor: AdminActor,
    input: CreateAuthority,
) -> Result<PreparedAuthorityCreation, ApiError> {
    validate_key(&input.idempotency_key)?;
    let name = input.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 180 || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "name must contain 1–180 printable characters",
        ));
    }
    input.policy.validate().map_err(bad)?;
    let digest = digest(&input)?;
    let replay = existing(
        &mut *state.pool.acquire().await?,
        &actor,
        &input.idempotency_key,
        "ca.create",
        &digest,
    )
    .await?;
    let source = match input.source {
        Source::Generated => "generated",
        Source::Imported => "imported",
    };
    let mut prepared = PreparedAuthorityCreation {
        actor,
        input,
        digest,
        replay,
        name,
        source,
        material: None,
    };
    if prepared.replay.is_some() {
        return Ok(prepared);
    }
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await?;
    let authority = match prepared.input.source {
        Source::Generated => {
            if prepared.input.certificate.is_some() || prepared.input.private_key.is_some() {
                return Err(ApiError::bad_request(
                    "generated CA cannot include imported material",
                ));
            }
            tokio::task::spawn_blocking(move || Authority::generate(now))
                .await
                .map_err(|_| ApiError::internal())?
                .map_err(bad)?
        }
        Source::Imported => {
            let material = Material {
                certificate: prepared
                    .input
                    .certificate
                    .clone()
                    .ok_or_else(|| ApiError::bad_request("certificate required"))?,
                private_key: prepared
                    .input
                    .private_key
                    .clone()
                    .ok_or_else(|| ApiError::bad_request("private_key required"))?,
            };
            tokio::task::spawn_blocking(move || Authority::import(material, now))
                .await
                .map_err(|_| ApiError::internal())?
                .map_err(bad)?
        }
    };
    let mut metadata = authority.metadata.clone();
    metadata["source"] = json!(source);
    metadata["trust_anchor_certificate"] = json!(authority.trust_anchor().map_err(bad)?);
    prepared.material = Some(AuthorityCreationMaterial {
        id: uuid::Uuid::new_v4().to_string(),
        ciphertext: state.secrets.encrypt(&authority.payload().to_string())?,
        metadata,
        expires_at: authority.expires_at,
        timestamp: now,
    });
    Ok(prepared)
}

pub(super) async fn create_authority_on(
    tx: &mut Transaction<'_, Postgres>,
    prepared: PreparedAuthorityCreation,
) -> Result<Value, ApiError> {
    let PreparedAuthorityCreation {
        actor,
        input,
        digest,
        name,
        source,
        material,
        ..
    } = prepared;
    if let Some(value) = existing(tx, &actor, &input.idempotency_key, "ca.create", &digest).await? {
        return Ok(value);
    }
    let material = material.ok_or_else(ApiError::internal)?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    if material.expires_at <= now {
        return Err(ApiError::conflict(
            "prepared CA expired; prepare the creation again",
        ));
    }
    let id = material.id;
    sqlx::query("INSERT INTO credentials(id,name,kind,owner_user_id,created_at,updated_at) VALUES($1,$2,'ca_identity',NULL,$3,$3)")
        .bind(&id).bind(name).bind(material.timestamp).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,artifact_names,created_at) VALUES($1,1,$2,$3,'{}'::jsonb,$4)")
        .bind(&id).bind(material.ciphertext).bind(material.metadata).bind(material.timestamp).execute(&mut **tx).await?;
    let next_action = (source == "generated").then_some(material.expires_at - Duration::days(365));
    sqlx::query("INSERT INTO mtls_authorities(credential_id,source,client_validity_days,renew_before_hours,next_action_at) VALUES($1,$2,$3,$4,$5)")
        .bind(&id).bind(source).bind(i32::from(input.policy.client_validity_days)).bind(i32::from(input.policy.renew_before_hours)).bind(next_action).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO mtls_ca_generations(ca_id,ca_version,state,expires_at,published_at) VALUES($1,1,'published',$2,$3)")
        .bind(&id).bind(material.expires_at).bind(material.timestamp).execute(&mut **tx).await?;
    let result = json!({"id":id,"version":1,"revision":1,"policy_revision":1});
    receipt(
        tx,
        &actor,
        &input.idempotency_key,
        "ca.create",
        &digest,
        &result,
    )
    .await?;
    crate::api::credentials::record(
        tx,
        &actor,
        "ca.created",
        &id,
        json!({"source":source,"version":1,"policy":input.policy}),
    )
    .await?;
    Ok(result)
}

#[derive(Clone)]
pub(super) struct PreparedUserReissue {
    pub(super) actor: AdminActor,
    pub(super) input: ReissueUser,
    pub(super) digest: String,
    pub(super) user: String,
    pub(super) prepared: Vec<(String, credentials::identities::PreparedIdentity)>,
    pub(super) replay: Option<Value>,
}

pub(super) async fn update_policy_on(
    tx: &mut Transaction<'_, Postgres>,
    actor: AdminActor,
    input: UpdatePolicy,
    digest: String,
    id: String,
) -> Result<Value, ApiError> {
    if let Some(value) = existing(tx, &actor, &input.idempotency_key, "ca.policy", &digest).await? {
        return Ok(value);
    }
    lock_ca_on(tx, &id).await?;
    let result = sqlx::query("UPDATE mtls_authorities a SET client_validity_days=$1,renew_before_hours=$2,policy_revision=policy_revision+1 FROM credentials c WHERE a.credential_id=$3 AND a.policy_revision=$4 AND c.id=a.credential_id AND NOT c.archived RETURNING a.policy_revision")
        .bind(i32::from(input.policy.client_validity_days)).bind(i32::from(input.policy.renew_before_hours)).bind(&id).bind(input.expected_revision).fetch_optional(&mut **tx).await?
        .ok_or_else(|| ApiError::conflict("CA policy changed, is archived, or was deleted"))?;
    let result = json!({"id":id,"policy_revision":result.get::<i64,_>("policy_revision"),"policy":input.policy});
    receipt(
        tx,
        &actor,
        &input.idempotency_key,
        "ca.policy",
        &digest,
        &result,
    )
    .await?;
    crate::api::credentials::record(
        tx,
        &actor,
        "ca.policy_updated",
        &id,
        json!({"previous_revision":input.expected_revision,"policy":input.policy}),
    )
    .await?;
    Ok(result)
}

pub(super) async fn prepare_user_reissue(
    state: &AppState,
    actor: AdminActor,
    user: String,
    input: ReissueUser,
) -> Result<PreparedUserReissue, ApiError> {
    use crate::credentials::identities;
    validate_key(&input.idempotency_key)?;
    let digest = digest(&(&user, &input))?;
    if let Some(value) = existing(
        &mut *state.pool.acquire().await?,
        &actor,
        &input.idempotency_key,
        "mtls.reissue",
        &digest,
    )
    .await?
    {
        return Ok(PreparedUserReissue {
            actor,
            input,
            digest,
            user,
            prepared: Vec::new(),
            replay: Some(value),
        });
    }
    let rows = sqlx::query("SELECT i.ca_id,i.revision,a.published_version FROM mtls_user_identities i JOIN mtls_authorities a ON a.credential_id=i.ca_id WHERE i.user_id=$1 AND NOT i.archived AND ($2::text IS NULL OR i.ca_id=$2) ORDER BY i.ca_id")
        .bind(&user).bind(&input.ca_credential_id).fetch_all(&state.pool).await?;
    if rows.is_empty() {
        return Err(ApiError::not_found("active managed identity"));
    }
    let actual: std::collections::BTreeMap<String, i64> = rows
        .iter()
        .map(|r| (r.get("ca_id"), r.get("revision")))
        .collect();
    if actual != input.expected_revisions {
        // A same-key request may have committed between the early receipt read
        // and this preparation snapshot. Recover its receipt before rejecting
        // the now-stale identity revision.
        if let Some(value) = existing(
            &mut *state.pool.acquire().await?,
            &actor,
            &input.idempotency_key,
            "mtls.reissue",
            &digest,
        )
        .await?
        {
            return Ok(PreparedUserReissue {
                actor,
                input,
                digest,
                user,
                prepared: Vec::new(),
                replay: Some(value),
            });
        }
        return Err(ApiError::conflict(
            "managed identities changed; reload before reissuing",
        ));
    }
    let mut prepared = Vec::new();
    for row in rows {
        let ca: String = row.get("ca_id");
        let identity = identities::prepare_user(
            &state.pool,
            &state.secrets,
            &user,
            &ca,
            row.get("published_version"),
            true,
        )
        .await
        .map_err(|e| ApiError::conflict(e.to_string()))?;
        prepared.push((ca, identity));
    }

    Ok(PreparedUserReissue {
        actor,
        input,
        digest,
        user,
        prepared,
        replay: None,
    })
}

pub(super) async fn reissue_user_on(
    tx: &mut Transaction<'_, Postgres>,
    state: &AppState,
    candidate: PreparedUserReissue,
) -> Result<Value, ApiError> {
    let PreparedUserReissue {
        actor,
        input,
        digest,
        user,
        prepared,
        ..
    } = candidate;

    if let Some(value) =
        existing(tx, &actor, &input.idempotency_key, "mtls.reissue", &digest).await?
    {
        return Ok(value);
    }
    let cas: Vec<String> = prepared.iter().map(|(ca, _)| ca.clone()).collect();
    lock_reissue_on(tx, &user, &cas).await?;
    let current: std::collections::BTreeMap<String,i64> = sqlx::query_as::<_,(String,i64)>("SELECT ca_id,revision FROM mtls_user_identities WHERE user_id=$1 AND NOT archived AND ($2::text IS NULL OR ca_id=$2) ORDER BY ca_id")
        .bind(&user).bind(&input.ca_credential_id).fetch_all(&mut **tx).await?.into_iter().collect();
    if current != input.expected_revisions {
        return Err(ApiError::conflict(
            "managed identities changed during signing; reload before reissuing",
        ));
    }
    let mut results = Vec::new();
    for (ca, candidate) in prepared {
        let issued = candidate.will_issue();
        let (identity, version) =
            crate::credentials::identities::commit_user(tx, &state.secrets, candidate)
                .await
                .map_err(managed_transaction_error)?;
        let nodes = crate::credentials::authorization::referenced_nodes_on(tx, &user, &ca).await?;
        results.push(json!({"ca_credential_id":ca,"identity_id":identity,"credential_version":version,"issued":issued,"affected_node_ids":nodes}));
    }
    let result = json!({"user_id":user,"identities":results,"refresh_subscription":true});
    receipt(
        tx,
        &actor,
        &input.idempotency_key,
        "mtls.reissue",
        &digest,
        &result,
    )
    .await?;
    audit::record(
        tx,
        &actor,
        "mtls.user_reissued",
        ("user", &user),
        result.clone(),
    )
    .await?;
    Ok(result)
}

#[cfg(test)]
mod tests;
