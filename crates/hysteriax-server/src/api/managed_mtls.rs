//! Credential-center entry points. Signing happens before the write transaction;
//! the transaction only verifies dependencies and publishes immutable material.
mod audit;

use axum::{
    Json,
    extract::{Extension, Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};

use super::AdminActor;
use crate::{
    credentials::{
        self,
        ca::{Authority, Material, Policy},
    },
    db,
    error::ApiError,
    state::AppState,
};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Generated,
    Imported,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAuthority {
    pub name: String,
    pub source: Source,
    pub certificate: Option<String>,
    pub private_key: Option<String>,
    #[serde(default)]
    pub policy: Policy,
    pub idempotency_key: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePolicy {
    pub expected_revision: i64,
    pub policy: Policy,
    pub idempotency_key: String,
}

fn bad(error: anyhow::Error) -> ApiError {
    ApiError::bad_request(error.to_string())
}

fn digest<T: Serialize>(request: &T) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(request).map_err(|_| ApiError::internal())?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn validate_key(key: &str) -> Result<(), ApiError> {
    if key.is_empty() || key.len() > 180 || key.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "idempotency_key must contain 1–180 printable bytes",
        ));
    }
    Ok(())
}

async fn existing(
    connection: &mut sqlx::PgConnection,
    actor: &AdminActor,
    key: &str,
    operation: &str,
    digest: &str,
) -> Result<Option<Value>, ApiError> {
    let row = sqlx::query("SELECT operation,request_digest,receipt FROM mtls_operations WHERE actor_id=$1 AND idempotency_key=$2")
        .bind(&actor.id).bind(key).fetch_optional(connection).await?;
    match row {
        Some(row)
            if row.get::<String, _>("operation") == operation
                && row.get::<String, _>("request_digest") == digest =>
        {
            Ok(Some(row.get("receipt")))
        }
        Some(_) => Err(ApiError::conflict(
            "idempotency key already used for a different request",
        )),
        None => Ok(None),
    }
}

async fn receipt(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminActor,
    key: &str,
    operation: &str,
    digest: &str,
    result: &Value,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO mtls_operations(actor_id,idempotency_key,operation,request_digest,receipt,created_at) VALUES($1,$2,$3,$4,$5,$6)")
        .bind(&actor.id).bind(key).bind(operation).bind(digest).bind(result).bind(Utc::now()).execute(&mut **tx).await?;
    Ok(())
}

async fn create_inner(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Json(input): Json<CreateAuthority>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    validate_key(&input.idempotency_key)?;
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 180 || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "name must contain 1–180 printable characters",
        ));
    }
    input.policy.validate().map_err(bad)?;
    let digest = digest(&input)?;
    if let Some(value) = existing(
        &mut *state.pool.acquire().await?,
        &actor,
        &input.idempotency_key,
        "ca.create",
        &digest,
    )
    .await?
    {
        return Ok((StatusCode::CREATED, Json(value)));
    }
    let now = Utc::now();
    // spawn_blocking isolates CPU signing/chain checks from the async executor.
    let source = match input.source {
        Source::Generated => "generated",
        Source::Imported => "imported",
    };
    let authority = match input.source {
        Source::Generated => {
            if input.certificate.is_some() || input.private_key.is_some() {
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
                certificate: input
                    .certificate
                    .clone()
                    .ok_or_else(|| ApiError::bad_request("certificate required"))?,
                private_key: input
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
    let mut tx = db::begin_write(&state.pool).await?;
    if let Some(value) = existing(
        &mut tx,
        &actor,
        &input.idempotency_key,
        "ca.create",
        &digest,
    )
    .await?
    {
        return Ok((StatusCode::CREATED, Json(value)));
    }
    let id = credentials::insert(
        &mut tx,
        &state.secrets,
        name,
        "ca_identity",
        None,
        &authority.payload(),
        &metadata,
        &json!({}),
    )
    .await?;
    let next_action = (source == "generated").then_some(authority.expires_at - Duration::days(365));
    sqlx::query("INSERT INTO mtls_authorities(credential_id,source,client_validity_days,renew_before_hours,next_action_at) VALUES($1,$2,$3,$4,$5)")
        .bind(&id).bind(source).bind(i32::from(input.policy.client_validity_days)).bind(i32::from(input.policy.renew_before_hours)).bind(next_action).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO mtls_ca_generations(ca_id,ca_version,state,expires_at,published_at) VALUES($1,1,'published',$2,$3)")
        .bind(&id).bind(authority.expires_at).bind(now).execute(&mut *tx).await?;
    let result = json!({"id":id,"version":1,"revision":1,"policy_revision":1});
    receipt(
        &mut tx,
        &actor,
        &input.idempotency_key,
        "ca.create",
        &digest,
        &result,
    )
    .await?;
    super::credentials::record(
        &mut tx,
        &actor,
        "ca.created",
        &id,
        json!({"source":source,"version":1,"policy":input.policy}),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(result)))
}

async fn update_policy_inner(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    Json(input): Json<UpdatePolicy>,
) -> Result<Json<Value>, ApiError> {
    validate_key(&input.idempotency_key)?;
    input.policy.validate().map_err(bad)?;
    let digest = digest(&(&id, &input))?;
    let mut tx = db::begin_write(&state.pool).await?;
    if let Some(value) = existing(
        &mut tx,
        &actor,
        &input.idempotency_key,
        "ca.policy",
        &digest,
    )
    .await?
    {
        return Ok(Json(value));
    }
    let result = sqlx::query("UPDATE mtls_authorities a SET client_validity_days=$1,renew_before_hours=$2,policy_revision=policy_revision+1 FROM credentials c WHERE a.credential_id=$3 AND a.policy_revision=$4 AND c.id=a.credential_id AND NOT c.archived RETURNING a.policy_revision")
        .bind(i32::from(input.policy.client_validity_days)).bind(i32::from(input.policy.renew_before_hours)).bind(&id).bind(input.expected_revision).fetch_optional(&mut *tx).await?
        .ok_or_else(|| ApiError::conflict("CA policy changed, is archived, or was deleted"))?;
    let result = json!({"id":id,"policy_revision":result.get::<i64,_>("policy_revision"),"policy":input.policy});
    receipt(
        &mut tx,
        &actor,
        &input.idempotency_key,
        "ca.policy",
        &digest,
        &result,
    )
    .await?;
    super::credentials::record(
        &mut tx,
        &actor,
        "ca.policy_updated",
        &id,
        json!({"previous_revision":input.expected_revision,"policy":input.policy}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(result))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublishAuthority {
    pub expected_revision: i64,
    pub certificate: Option<String>,
    pub private_key: Option<String>,
    pub replace_rotation_id: Option<String>,
    pub idempotency_key: String,
}

async fn publish_inner(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    Json(input): Json<PublishAuthority>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    validate_key(&input.idempotency_key)?;
    let digest = digest(&(&id, &input))?;
    if let Some(value) = existing(
        &mut *state.pool.acquire().await?,
        &actor,
        &input.idempotency_key,
        "ca.publish",
        &digest,
    )
    .await?
    {
        return Ok((StatusCode::ACCEPTED, Json(value)));
    }
    let source: String =
        sqlx::query_scalar("SELECT source FROM mtls_authorities WHERE credential_id=$1")
            .bind(&id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| ApiError::not_found("CA identity"))?;
    let now = Utc::now();
    let authority = if source == "generated" {
        if input.certificate.is_some() || input.private_key.is_some() {
            return Err(ApiError::bad_request(
                "generated CA can only be rotated by the system",
            ));
        }
        tokio::task::spawn_blocking(move || Authority::generate(now))
            .await
            .map_err(|_| ApiError::internal())?
            .map_err(bad)?
    } else {
        let material = Material {
            certificate: input
                .certificate
                .clone()
                .ok_or_else(|| ApiError::bad_request("certificate required"))?,
            private_key: input
                .private_key
                .clone()
                .ok_or_else(|| ApiError::bad_request("private_key required"))?,
        };
        tokio::task::spawn_blocking(move || Authority::import(material, now))
            .await
            .map_err(|_| ApiError::internal())?
            .map_err(bad)?
    };
    let mut tx = db::begin_write(&state.pool).await?;
    if let Some(value) = existing(
        &mut tx,
        &actor,
        &input.idempotency_key,
        "ca.publish",
        &digest,
    )
    .await?
    {
        return Ok((StatusCode::ACCEPTED, Json(value)));
    }
    let row = sqlx::query("SELECT c.revision,c.latest_version,c.archived,a.source,a.published_version FROM credentials c JOIN mtls_authorities a ON a.credential_id=c.id WHERE c.id=$1").bind(&id).fetch_optional(&mut *tx).await?.ok_or_else(|| ApiError::not_found("CA identity"))?;
    if row.get::<i64, _>("revision") != input.expected_revision
        || row.get::<bool, _>("archived")
        || row.get::<String, _>("source") != source
    {
        return Err(ApiError::conflict("CA changed or is archived"));
    }
    let pending=sqlx::query("SELECT r.id,r.phase,r.ca_version,g.expires_at FROM mtls_rotations r JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.ca_id=$1 AND r.phase<>'complete'").bind(&id).fetch_optional(&mut *tx).await?;
    if let Some(pending) = pending {
        let phase: String = pending.get("phase");
        let previous_id: String = pending.get("id");
        let previous: i64 = pending.get("ca_version");
        let unpublished = matches!(phase.as_str(), "trust" | "identities" | "publish");
        let expired_publication =
            phase == "probes" && pending.get::<DateTime<Utc>, _>("expires_at") <= Utc::now();
        if input.replace_rotation_id.as_deref() != Some(previous_id.as_str())
            || !(unpublished || expired_publication)
        {
            return Err(ApiError::conflict(
                "previous CA publication has not completed; explicitly replace its unpublished or expired target",
            ));
        }
        if unpublished {
            sqlx::query("UPDATE mtls_ca_generations SET state='retired' WHERE ca_id=$1 AND ca_version=$2 AND state='preparing'").bind(&id).bind(previous).execute(&mut *tx).await?;
            sqlx::query("UPDATE mtls_user_versions SET discarded=TRUE WHERE ca_id=$1 AND ca_version=$2 AND NOT released").bind(&id).bind(previous).execute(&mut *tx).await?;
            let deadline:Option<DateTime<Utc>>=sqlx::query_scalar("SELECT max(expires_at)+interval '5 minutes' FROM(SELECT expires_at FROM mtls_user_versions WHERE ca_id=$1 AND ca_version=$2 UNION ALL SELECT expires_at FROM mtls_probe_versions WHERE ca_id=$1 AND ca_version=$2) materials").bind(&id).bind(previous).fetch_one(&mut *tx).await?;
            sqlx::query("INSERT INTO mtls_trust_cleanup(ca_id,ca_version,next_action_at) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(&id).bind(previous).bind(deadline.unwrap_or_else(Utc::now)).execute(&mut *tx).await?;
        }
        let cancelled=sqlx::query("UPDATE jobs j SET status='cancelled',stage='ca_target_superseded',updated_at=$2,finished_at=$2 FROM mtls_node_operations o WHERE o.job_id=j.id AND o.rotation_id=$1 AND j.status IN ('queued','waiting_confirmation') RETURNING j.id,j.kind,j.node_id").bind(&previous_id).bind(Utc::now()).fetch_all(&mut *tx).await?;
        for job in cancelled {
            crate::kick_requests::event(&mut tx,&job.get::<String,_>("id"),"job.cancelled",json!({"id":job.get::<String,_>("id"),"kind":job.get::<String,_>("kind"),"node_id":job.get::<Option<String>,_>("node_id"),"status":"cancelled","stage":"ca_target_superseded"})).await?;
        }
        sqlx::query("UPDATE mtls_rotations SET phase='complete',outcome=$3,error_message='Target was replaced by an explicit publication request',updated_at=$2 WHERE id=$1").bind(&previous_id).bind(Utc::now()).bind(if unpublished{"superseded_unpublished_target"}else{"superseded_expired_publication"}).execute(&mut *tx).await?;
        super::credentials::record(
            &mut tx,
            &actor,
            "ca.target_superseded",
            &id,
            json!({"rotation_id":previous_id,"version":previous,"unpublished":unpublished}),
        )
        .await?;
    } else if input.replace_rotation_id.is_some() {
        return Err(ApiError::conflict(
            "replacement target changed; reload before publishing",
        ));
    }
    let version = row.get::<i64, _>("latest_version") + 1;
    let mut metadata = authority.metadata.clone();
    metadata["source"] = json!(source);
    metadata["trust_anchor_certificate"] = json!(authority.trust_anchor().map_err(bad)?);
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES($1,$2,$3,$4,$5)")
        .bind(&id).bind(version).bind(state.secrets.encrypt(&authority.payload().to_string())?).bind(metadata).bind(now).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE credentials SET latest_version=$2,revision=revision+1,updated_at=$3 WHERE id=$1",
    )
    .bind(&id)
    .bind(version)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let batch = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO credential_batches(id,credential_id,version,created_at) VALUES($1,$2,$3,$4)",
    )
    .bind(&batch)
    .bind(&id)
    .bind(version)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    // A historical or deferred user identity also needs a coordinated transition.
    let referenced: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_node_trust WHERE ca_id=$1 UNION ALL SELECT 1 FROM mtls_user_identities WHERE ca_id=$1)").bind(&id).fetch_one(&mut *tx).await?;
    let rotation = if referenced {
        sqlx::query("INSERT INTO mtls_ca_generations(ca_id,ca_version,state,expires_at) VALUES($1,$2,'preparing',$3)").bind(&id).bind(version).bind(authority.expires_at).execute(&mut *tx).await?;
        let rotation = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO mtls_rotations(id,ca_id,ca_version,phase,batch_id,next_action_at,created_at,updated_at) VALUES($1,$2,$3,'trust',$4,$5,$5,$5)")
            .bind(&rotation).bind(&id).bind(version).bind(&batch).bind(now).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO mtls_rotation_nodes(rotation_id,node_id,expected_revision) SELECT DISTINCT $1,t.node_id,n.desired_revision FROM mtls_node_trust t JOIN nodes n ON n.id=t.node_id WHERE t.ca_id=$2 AND (t.desired OR t.deployed)")
            .bind(&rotation).bind(&id).execute(&mut *tx).await?;
        Some(rotation)
    } else {
        sqlx::query(
            "UPDATE mtls_ca_generations SET state='retired' WHERE ca_id=$1 AND state='published'",
        )
        .bind(&id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO mtls_ca_generations(ca_id,ca_version,state,expires_at,published_at) VALUES($1,$2,'published',$3,$4)").bind(&id).bind(version).bind(authority.expires_at).bind(now).execute(&mut *tx).await?;
        sqlx::query("UPDATE mtls_authorities SET published_version=$2,next_action_at=$3,maintenance_error=NULL WHERE credential_id=$1")
            .bind(&id).bind(version).bind((source=="generated").then_some(authority.expires_at - Duration::days(365))).execute(&mut *tx).await?;
        None
    };
    if let Some(rotation) = &rotation {
        sqlx::query("UPDATE mtls_rotations SET actor_id=$2,previous_version=$3 WHERE id=$1")
            .bind(rotation)
            .bind(&actor.id)
            .bind(row.get::<i64, _>("published_version"))
            .execute(&mut *tx)
            .await?;
    }
    let result = json!({"id":id,"version":version,"revision":input.expected_revision+1,"batch_id":batch,"rotation_id":rotation,"published":!referenced});
    receipt(
        &mut tx,
        &actor,
        &input.idempotency_key,
        "ca.publish",
        &digest,
        &result,
    )
    .await?;
    super::credentials::record(
        &mut tx,
        &actor,
        "ca.version_prepared",
        &id,
        json!({"version":version,"batch_id":batch,"rotation_id":rotation}),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(result)))
}

pub async fn create(
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

pub async fn update_policy(
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

pub async fn publish(
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

pub async fn reissue_user(
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReissueUser {
    pub ca_credential_id: Option<String>,
    pub expected_revisions: std::collections::BTreeMap<String, i64>,
    pub idempotency_key: String,
}

async fn reissue_user_inner(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(user): Path<String>,
    Json(input): Json<ReissueUser>,
) -> Result<Json<Value>, ApiError> {
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
        return Ok(Json(value));
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
    let mut tx = db::begin_write(&state.pool).await?;
    if let Some(value) = existing(
        &mut tx,
        &actor,
        &input.idempotency_key,
        "mtls.reissue",
        &digest,
    )
    .await?
    {
        return Ok(Json(value));
    }
    let current: std::collections::BTreeMap<String,i64> = sqlx::query_as::<_,(String,i64)>("SELECT ca_id,revision FROM mtls_user_identities WHERE user_id=$1 AND NOT archived AND ($2::text IS NULL OR ca_id=$2) ORDER BY ca_id")
        .bind(&user).bind(&input.ca_credential_id).fetch_all(&mut *tx).await?.into_iter().collect();
    if current != input.expected_revisions {
        return Err(ApiError::conflict(
            "managed identities changed during signing; reload before reissuing",
        ));
    }
    let mut results = Vec::new();
    for (ca, candidate) in prepared {
        let issued = candidate.will_issue();
        let (identity, version) = identities::commit_user(&mut tx, &state.secrets, candidate)
            .await
            .map_err(|e| ApiError::conflict(e.to_string()))?;
        let nodes =
            crate::credentials::authorization::referenced_nodes_on(&mut tx, &user, &ca).await?;
        results.push(json!({"ca_credential_id":ca,"identity_id":identity,"credential_version":version,"issued":issued,"affected_node_ids":nodes}));
    }
    let result = json!({"user_id":user,"identities":results,"refresh_subscription":true});
    receipt(
        &mut tx,
        &actor,
        &input.idempotency_key,
        "mtls.reissue",
        &digest,
        &result,
    )
    .await?;
    audit::record(
        &mut tx,
        &actor,
        "mtls.user_reissued",
        ("user", &user),
        result.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(result))
}

pub(super) async fn user_identities(state: &AppState, user: &str) -> Result<Vec<Value>, ApiError> {
    let rows=sqlx::query("SELECT i.*,c.name AS ca_name,a.published_version,a.policy_revision,g.expires_at AS ca_expires_at,coalesce((SELECT jsonb_agg(jsonb_build_object('version',v.credential_version,'ca_version',v.ca_version,'not_before',v.not_before,'expires_at',v.expires_at,'released',v.released,'discarded',v.discarded) ORDER BY v.credential_version DESC) FROM mtls_user_versions v WHERE v.identity_id=i.id),'[]'::jsonb) AS versions,coalesce((SELECT jsonb_agg(DISTINCT t.node_id ORDER BY t.node_id) FROM (SELECT node_id,ca_id FROM mtls_node_trust WHERE desired OR deployed UNION SELECT node_id,ca_id FROM mtls_live_activation_trust) t JOIN node_assignments na ON na.node_id=t.node_id AND na.user_id=i.user_id WHERE t.ca_id=i.ca_id),'[]'::jsonb) AS node_ids FROM mtls_user_identities i JOIN mtls_authorities a ON a.credential_id=i.ca_id JOIN credentials c ON c.id=i.ca_id JOIN mtls_ca_generations g ON g.ca_id=i.ca_id AND g.ca_version=a.published_version WHERE i.user_id=$1 ORDER BY c.name,i.ca_id")
        .bind(user).fetch_all(&mut *crate::read_snapshot::begin(&state.pool).await?).await?;
    Ok(rows.into_iter().map(|row|json!({"identity_id":row.get::<String,_>("id"),"ca_credential_id":row.get::<String,_>("ca_id"),"ca_name":row.get::<String,_>("ca_name"),"revision":row.get::<i64,_>("revision"),"authorization_revision":row.get::<i64,_>("authorization_revision"),"credential_id":row.get::<Option<String>,_>("credential_id"),"archived":row.get::<bool,_>("archived"),"maintenance_error":row.get::<Option<String>,_>("maintenance_error"),"next_action_at":row.get::<Option<chrono::DateTime<Utc>>,_>("next_action_at"),"published_ca_version":row.get::<i64,_>("published_version"),"ca_expires_at":row.get::<chrono::DateTime<Utc>,_>("ca_expires_at"),"policy_revision":row.get::<i64,_>("policy_revision"),"versions":row.get::<Value,_>("versions"),"node_ids":row.get::<Value,_>("node_ids")})).collect())
}

#[cfg(test)]
mod tests;
