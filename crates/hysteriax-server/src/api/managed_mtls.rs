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

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Generated,
    Imported,
}

#[derive(Clone, Deserialize, Serialize)]
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

#[derive(Clone, Deserialize, Serialize)]
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
    sqlx::query("INSERT INTO mtls_operations(actor_id,idempotency_key,operation,request_digest,receipt,created_at) VALUES($1,$2,$3,$4,$5,clock_timestamp())")
        .bind(&actor.id).bind(key).bind(operation).bind(digest).bind(result).execute(&mut **tx).await?;
    Ok(())
}

fn managed_transaction_error(error: anyhow::Error) -> ApiError {
    if crate::db::RetryableTransactionError::retryable_transaction(&error) {
        ApiError::from(error)
    } else {
        ApiError::conflict(error.to_string())
    }
}
async fn managed_transaction<F>(
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
async fn lock_ca_on(
    tx: &mut Transaction<'_, Postgres>,
    ca: &str,
    publication: bool,
) -> Result<(), ApiError> {
    if publication {
        sqlx::query("SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM mtls_node_trust WHERE ca_id=$1 UNION SELECT node_id FROM mtls_live_activation_trust WHERE ca_id=$1) ORDER BY n.id FOR NO KEY UPDATE OF n").bind(ca).fetch_all(&mut **tx).await?;
        let users:Vec<String>=sqlx::query_scalar("SELECT u.id FROM users u JOIN mtls_user_identities i ON i.user_id=u.id WHERE i.ca_id=$1 ORDER BY u.id FOR NO KEY UPDATE OF u").bind(ca).fetch_all(&mut **tx).await?;
        sqlx::query("SELECT id FROM credentials WHERE id=$1 OR owner_user_id=ANY($2) ORDER BY id FOR UPDATE").bind(ca).bind(&users).fetch_all(&mut **tx).await?;
    } else {
        sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR UPDATE")
            .bind(ca)
            .fetch_all(&mut **tx)
            .await?;
    }
    let authority =
        sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE")
            .bind(ca)
            .fetch_optional(&mut **tx)
            .await?;
    if publication && authority.is_none() {
        return Err(ApiError::not_found("CA identity"));
    }
    if publication {
        sqlx::query("SELECT id FROM mtls_user_identities WHERE ca_id=$1 ORDER BY id FOR UPDATE")
            .bind(ca)
            .fetch_all(&mut **tx)
            .await?;
        sqlx::query("SELECT id FROM mtls_rotations WHERE ca_id=$1 ORDER BY id FOR UPDATE")
            .bind(ca)
            .fetch_all(&mut **tx)
            .await?;
    }
    Ok(())
}
async fn lock_reissue_on(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    cas: &[String],
) -> Result<(), ApiError> {
    sqlx::query("SELECT n.id FROM nodes n JOIN node_assignments a ON a.node_id=n.id WHERE a.user_id=$1 ORDER BY n.id FOR NO KEY UPDATE OF n").bind(user).fetch_all(&mut **tx).await?;
    sqlx::query("SELECT id FROM users WHERE id=$1 FOR NO KEY UPDATE")
        .bind(user)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))?;
    sqlx::query(
        "SELECT id FROM credentials WHERE owner_user_id=$1 OR id=ANY($2) ORDER BY id FOR UPDATE",
    )
    .bind(user)
    .bind(cas)
    .fetch_all(&mut **tx)
    .await?;
    sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=ANY($1) ORDER BY credential_id FOR UPDATE").bind(cas).fetch_all(&mut **tx).await?;
    sqlx::query("SELECT id FROM mtls_user_identities WHERE user_id=$1 ORDER BY id FOR UPDATE")
        .bind(user)
        .fetch_all(&mut **tx)
        .await?;
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
    let name = name.to_owned();
    let authority = std::sync::Arc::new(authority);
    let pool = state.pool.clone();
    let (request_actor, request_key, request_digest) =
        (actor.clone(), input.idempotency_key.clone(), digest.clone());
    let result=managed_transaction(&pool,&request_actor,&request_key,"ca.create",&request_digest,move |tx| {
        let (state,actor,input,digest,authority,metadata,name)=(state.clone(),actor.clone(),input.clone(),digest.clone(),authority.clone(),metadata.clone(),name.clone());
        Box::pin(async move {

    if let Some(value) = existing(
        tx,
        &actor,
        &input.idempotency_key,
        "ca.create",
        &digest,
    )
    .await?
    {
        return Ok(value);
    }
    let id = credentials::insert(
        tx,
        &state.secrets,
        &name,
        "ca_identity",
        None,
        &authority.payload(),
        &metadata,
        &json!({}),
    )
    .await?;
    let next_action = (source == "generated").then_some(authority.expires_at - Duration::days(365));
    sqlx::query("INSERT INTO mtls_authorities(credential_id,source,client_validity_days,renew_before_hours,next_action_at) VALUES($1,$2,$3,$4,$5)")
        .bind(&id).bind(source).bind(i32::from(input.policy.client_validity_days)).bind(i32::from(input.policy.renew_before_hours)).bind(next_action).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO mtls_ca_generations(ca_id,ca_version,state,expires_at,published_at) VALUES($1,1,'published',$2,$3)")
        .bind(&id).bind(authority.expires_at).bind(now).execute(&mut **tx).await?;
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
    super::credentials::record(
        tx,
        &actor,
        "ca.created",
        &id,
        json!({"source":source,"version":1,"policy":input.policy}),
    )
    .await?;
    Ok(result)
        })
    }).await?;
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

    let pool = state.pool.clone();
    let (request_actor, request_key, request_digest) =
        (actor.clone(), input.idempotency_key.clone(), digest.clone());
    let result=managed_transaction(&pool,&request_actor,&request_key,"ca.policy",&request_digest,move |tx| {
        let (actor,input,digest,id)=(actor.clone(),input.clone(),digest.clone(),id.clone());
        Box::pin(async move {

    if let Some(value) = existing(
        tx,
        &actor,
        &input.idempotency_key,
        "ca.policy",
        &digest,
    )
    .await?
    {
        return Ok(value);
    }
    lock_ca_on(tx,&id,false).await?;
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
    super::credentials::record(
        tx,
        &actor,
        "ca.policy_updated",
        &id,
        json!({"previous_revision":input.expected_revision,"policy":input.policy}),
    )
    .await?;
    Ok(result)
        })
    }).await?;
    Ok(Json(result))
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublishAuthority {
    pub expected_revision: i64,
    pub certificate: Option<String>,
    pub private_key: Option<String>,
    pub replace_rotation_id: Option<String>,
    pub idempotency_key: String,
}

#[derive(Clone)]
struct PreparedAuthorityMaterial {
    payload_enc: String,
    metadata: Value,
    expires_at: DateTime<Utc>,
    batch_id: String,
    rotation_id: String,
}

#[derive(Clone)]
pub(crate) struct PreparedAuthorityPublication {
    actor: AdminActor,
    id: String,
    input: PublishAuthority,
    digest: String,
    source: Option<String>,
    material: Option<PreparedAuthorityMaterial>,
    replay: Option<Value>,
}

pub(crate) async fn prepare_authority_publication(
    state: &AppState,
    actor: AdminActor,
    id: String,
    input: PublishAuthority,
) -> Result<PreparedAuthorityPublication, ApiError> {
    validate_key(&input.idempotency_key)?;
    let digest = digest(&(&id, &input))?;
    let replay = existing(
        &mut *state.pool.acquire().await?,
        &actor,
        &input.idempotency_key,
        "ca.publish",
        &digest,
    )
    .await?;
    if replay.is_some() {
        return Ok(PreparedAuthorityPublication {
            actor,
            id,
            input,
            digest,
            source: None,
            material: None,
            replay,
        });
    }
    let row=sqlx::query("SELECT source,clock_timestamp() AS database_now FROM mtls_authorities WHERE credential_id=$1")
        .bind(&id).fetch_optional(&state.pool).await?.ok_or_else(||ApiError::not_found("CA identity"))?;
    let source: String = row.get("source");
    let now: DateTime<Utc> = row.get("database_now");
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
    let mut metadata = authority.metadata.clone();
    metadata["source"] = json!(source);
    metadata["trust_anchor_certificate"] = json!(authority.trust_anchor().map_err(bad)?);
    let material = PreparedAuthorityMaterial {
        payload_enc: state
            .secrets
            .encrypt(&authority.payload().to_string())
            .map_err(ApiError::from)?,
        metadata,
        expires_at: authority.expires_at,
        batch_id: uuid::Uuid::new_v4().to_string(),
        rotation_id: uuid::Uuid::new_v4().to_string(),
    };
    Ok(PreparedAuthorityPublication {
        actor,
        id,
        input,
        digest,
        source: Some(source),
        material: Some(material),
        replay: None,
    })
}

pub(crate) async fn publish_authority_on(
    tx: &mut Transaction<'_, Postgres>,
    prepared: PreparedAuthorityPublication,
) -> Result<Value, ApiError> {
    let PreparedAuthorityPublication {
        actor,
        id,
        input,
        digest,
        source,
        material,
        ..
    } = prepared;

    if let Some(value) = existing(tx, &actor, &input.idempotency_key, "ca.publish", &digest).await?
    {
        return Ok(value);
    }
    let material = material.ok_or_else(ApiError::internal)?;
    let source = source.ok_or_else(ApiError::internal)?;
    lock_ca_on(tx, &id, true).await?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    if material.expires_at <= now {
        return Err(ApiError::conflict(
            "prepared CA expired; prepare the publication again",
        ));
    }
    let row = sqlx::query("SELECT c.revision,c.latest_version,c.archived,a.source,a.published_version FROM credentials c JOIN mtls_authorities a ON a.credential_id=c.id WHERE c.id=$1").bind(&id).fetch_optional(&mut **tx).await?.ok_or_else(|| ApiError::not_found("CA identity"))?;
    if row.get::<i64, _>("revision") != input.expected_revision
        || row.get::<bool, _>("archived")
        || row.get::<String, _>("source") != source
    {
        return Err(ApiError::conflict("CA changed or is archived"));
    }
    let pending=sqlx::query("SELECT r.id,r.phase,r.ca_version,g.expires_at FROM mtls_rotations r JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.ca_id=$1 AND r.phase<>'complete'").bind(&id).fetch_optional(&mut **tx).await?;
    if let Some(pending) = pending {
        let phase: String = pending.get("phase");
        let previous_id: String = pending.get("id");
        let previous: i64 = pending.get("ca_version");
        let unpublished = matches!(phase.as_str(), "trust" | "identities" | "publish");
        let expired_publication =
            phase == "probes" && pending.get::<DateTime<Utc>, _>("expires_at") <= now;
        if input.replace_rotation_id.as_deref() != Some(previous_id.as_str())
            || !(unpublished || expired_publication)
        {
            return Err(ApiError::conflict(
                "previous CA publication has not completed; explicitly replace its unpublished or expired target",
            ));
        }
        if unpublished {
            sqlx::query("UPDATE mtls_ca_generations SET state='retired' WHERE ca_id=$1 AND ca_version=$2 AND state='preparing'").bind(&id).bind(previous).execute(&mut **tx).await?;
            sqlx::query("UPDATE mtls_user_versions SET discarded=TRUE WHERE ca_id=$1 AND ca_version=$2 AND NOT released").bind(&id).bind(previous).execute(&mut **tx).await?;
            let deadline:Option<DateTime<Utc>>=sqlx::query_scalar("SELECT max(expires_at)+interval '5 minutes' FROM(SELECT expires_at FROM mtls_user_versions WHERE ca_id=$1 AND ca_version=$2 UNION ALL SELECT expires_at FROM mtls_probe_versions WHERE ca_id=$1 AND ca_version=$2) materials").bind(&id).bind(previous).fetch_one(&mut **tx).await?;
            sqlx::query("INSERT INTO mtls_trust_cleanup(ca_id,ca_version,next_action_at) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(&id).bind(previous).bind(deadline.unwrap_or(now)).execute(&mut **tx).await?;
        }
        let cancelled=sqlx::query("UPDATE jobs j SET status='cancelled',stage='ca_target_superseded',updated_at=$2,finished_at=$2 FROM mtls_node_operations o WHERE o.job_id=j.id AND o.rotation_id=$1 AND j.status IN ('queued','waiting_confirmation') RETURNING j.id,j.kind,j.node_id").bind(&previous_id).bind(now).fetch_all(&mut **tx).await?;
        for job in cancelled {
            crate::kick_requests::event(tx,&job.get::<String,_>("id"),"job.cancelled",json!({"id":job.get::<String,_>("id"),"kind":job.get::<String,_>("kind"),"node_id":job.get::<Option<String>,_>("node_id"),"status":"cancelled","stage":"ca_target_superseded"})).await?;
        }
        sqlx::query("UPDATE mtls_rotations SET phase='complete',outcome=$3,error_message='Target was replaced by an explicit publication request',updated_at=$2 WHERE id=$1").bind(&previous_id).bind(now).bind(if unpublished{"superseded_unpublished_target"}else{"superseded_expired_publication"}).execute(&mut **tx).await?;
        super::credentials::record(
            tx,
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
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES($1,$2,$3,$4,$5)")
        .bind(&id).bind(version).bind(material.payload_enc).bind(material.metadata).bind(now).execute(&mut **tx).await?;
    sqlx::query(
        "UPDATE credentials SET latest_version=$2,revision=revision+1,updated_at=$3 WHERE id=$1",
    )
    .bind(&id)
    .bind(version)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    let batch = material.batch_id;
    sqlx::query(
        "INSERT INTO credential_batches(id,credential_id,version,created_at) VALUES($1,$2,$3,$4)",
    )
    .bind(&batch)
    .bind(&id)
    .bind(version)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    // A historical or deferred user identity also needs a coordinated transition.
    let referenced: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_node_trust WHERE ca_id=$1 UNION ALL SELECT 1 FROM mtls_user_identities WHERE ca_id=$1)").bind(&id).fetch_one(&mut **tx).await?;
    let rotation = if referenced {
        sqlx::query("INSERT INTO mtls_ca_generations(ca_id,ca_version,state,expires_at) VALUES($1,$2,'preparing',$3)").bind(&id).bind(version).bind(material.expires_at).execute(&mut **tx).await?;
        let rotation = material.rotation_id;
        sqlx::query("INSERT INTO mtls_rotations(id,ca_id,ca_version,phase,batch_id,next_action_at,created_at,updated_at) VALUES($1,$2,$3,'trust',$4,$5,$5,$5)")
            .bind(&rotation).bind(&id).bind(version).bind(&batch).bind(now).execute(&mut **tx).await?;
        sqlx::query("INSERT INTO mtls_rotation_nodes(rotation_id,node_id,expected_revision) SELECT DISTINCT $1,t.node_id,n.desired_revision FROM mtls_node_trust t JOIN nodes n ON n.id=t.node_id WHERE t.ca_id=$2 AND (t.desired OR t.deployed)")
            .bind(&rotation).bind(&id).execute(&mut **tx).await?;
        Some(rotation)
    } else {
        sqlx::query(
            "UPDATE mtls_ca_generations SET state='retired' WHERE ca_id=$1 AND state='published'",
        )
        .bind(&id)
        .execute(&mut **tx)
        .await?;
        sqlx::query("INSERT INTO mtls_ca_generations(ca_id,ca_version,state,expires_at,published_at) VALUES($1,$2,'published',$3,$4)").bind(&id).bind(version).bind(material.expires_at).bind(now).execute(&mut **tx).await?;
        sqlx::query("UPDATE mtls_authorities SET published_version=$2,next_action_at=$3,maintenance_error=NULL WHERE credential_id=$1")
            .bind(&id).bind(version).bind((source=="generated").then_some(material.expires_at - Duration::days(365))).execute(&mut **tx).await?;
        None
    };
    if let Some(rotation) = &rotation {
        sqlx::query("UPDATE mtls_rotations SET actor_id=$2,previous_version=$3 WHERE id=$1")
            .bind(rotation)
            .bind(&actor.id)
            .bind(row.get::<i64, _>("published_version"))
            .execute(&mut **tx)
            .await?;
    }
    let result = json!({"id":id,"version":version,"revision":input.expected_revision+1,"batch_id":batch,"rotation_id":rotation,"published":!referenced});
    receipt(
        tx,
        &actor,
        &input.idempotency_key,
        "ca.publish",
        &digest,
        &result,
    )
    .await?;
    super::credentials::record(
        tx,
        &actor,
        "ca.version_prepared",
        &id,
        json!({"version":version,"batch_id":batch,"rotation_id":rotation}),
    )
    .await?;
    Ok(result)
}

async fn publish_inner(
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

#[derive(Clone, Deserialize, Serialize)]
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
            return Ok(Json(value));
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

    let pool = state.pool.clone();
    let (request_actor, request_key, request_digest) =
        (actor.clone(), input.idempotency_key.clone(), digest.clone());
    let result=managed_transaction(&pool,&request_actor,&request_key,"mtls.reissue",&request_digest,move |tx| {
        let (state,actor,input,digest,user,prepared)=(state.clone(),actor.clone(),input.clone(),digest.clone(),user.clone(),prepared.clone());
        Box::pin(async move {

    if let Some(value) = existing(
        tx,
        &actor,
        &input.idempotency_key,
        "mtls.reissue",
        &digest,
    )
    .await?
    {
        return Ok(value);
    }
    let cas:Vec<String>=prepared.iter().map(|(ca,_)|ca.clone()).collect();lock_reissue_on(tx,&user,&cas).await?;
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
        let (identity, version) = crate::credentials::identities::commit_user(tx, &state.secrets, candidate)
            .await
            .map_err(managed_transaction_error)?;
        let nodes =
            crate::credentials::authorization::referenced_nodes_on(tx, &user, &ca).await?;
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
        })
    }).await?;
    Ok(Json(result))
}

pub(super) async fn user_identities(state: &AppState, user: &str) -> Result<Vec<Value>, ApiError> {
    let rows=sqlx::query("SELECT i.*,c.name AS ca_name,a.published_version,a.policy_revision,g.expires_at AS ca_expires_at,coalesce((SELECT jsonb_agg(jsonb_build_object('version',v.credential_version,'ca_version',v.ca_version,'not_before',v.not_before,'expires_at',v.expires_at,'released',v.released,'discarded',v.discarded) ORDER BY v.credential_version DESC) FROM mtls_user_versions v WHERE v.identity_id=i.id),'[]'::jsonb) AS versions,coalesce((SELECT jsonb_agg(DISTINCT t.node_id ORDER BY t.node_id) FROM (SELECT node_id,ca_id FROM mtls_node_trust WHERE desired OR deployed UNION SELECT node_id,ca_id FROM mtls_live_activation_trust) t JOIN node_assignments na ON na.node_id=t.node_id AND na.user_id=i.user_id WHERE t.ca_id=i.ca_id),'[]'::jsonb) AS node_ids FROM mtls_user_identities i JOIN mtls_authorities a ON a.credential_id=i.ca_id JOIN credentials c ON c.id=i.ca_id JOIN mtls_ca_generations g ON g.ca_id=i.ca_id AND g.ca_version=a.published_version WHERE i.user_id=$1 ORDER BY c.name,i.ca_id")
        .bind(user).fetch_all(&mut *crate::read_snapshot::begin(&state.pool).await?).await?;
    Ok(rows.into_iter().map(|row|json!({"identity_id":row.get::<String,_>("id"),"ca_credential_id":row.get::<String,_>("ca_id"),"ca_name":row.get::<String,_>("ca_name"),"revision":row.get::<i64,_>("revision"),"authorization_revision":row.get::<i64,_>("authorization_revision"),"credential_id":row.get::<Option<String>,_>("credential_id"),"archived":row.get::<bool,_>("archived"),"maintenance_error":row.get::<Option<String>,_>("maintenance_error"),"next_action_at":row.get::<Option<chrono::DateTime<Utc>>,_>("next_action_at"),"published_ca_version":row.get::<i64,_>("published_version"),"ca_expires_at":row.get::<chrono::DateTime<Utc>,_>("ca_expires_at"),"policy_revision":row.get::<i64,_>("policy_revision"),"versions":row.get::<Value,_>("versions"),"node_ids":row.get::<Value,_>("node_ids")})).collect())
}

#[cfg(test)]
mod tests;
