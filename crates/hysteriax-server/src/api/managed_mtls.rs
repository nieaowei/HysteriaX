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

async fn lock_ca_on(tx: &mut Transaction<'_, Postgres>, ca: &str) -> Result<(), ApiError> {
    sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR UPDATE")
        .bind(ca)
        .fetch_all(&mut **tx)
        .await?;
    sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE")
        .bind(ca)
        .fetch_optional(&mut **tx)
        .await?;
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

impl PreparedAuthorityPublication {
    pub(crate) fn matches_automatic_scope(&self, ca: &str, revision: i64, key: &str) -> bool {
        self.id == ca
            && self.actor.id == "system:mtls-maintenance"
            && self.input.expected_revision == revision
            && self.input.replace_rotation_id.is_none()
            && self.input.idempotency_key == key
    }
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
    // Ordinary publication changes CA-owned rows; rotation work discovers
    // members from authoritative trust before it can publish the new version.
    // Replacement changes the same CA-owned facts. Material flags and old-job
    // cancellation are derived in bounded work after this atomic source change.
    lock_ca_on(tx, &id).await?;
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
    let pending=sqlx::query("SELECT r.id,r.phase,r.ca_version,g.expires_at FROM mtls_rotations r JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.ca_id=$1 AND r.phase<>'complete' FOR UPDATE OF r,g").bind(&id).fetch_optional(&mut **tx).await?;
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
            crate::credentials::rotations::superseded::enqueue_on(tx, &previous_id, &id, previous)
                .await?;
            let deadline =
                crate::credentials::rotations::retention_deadline(tx, &id, previous).await?;
            sqlx::query("INSERT INTO mtls_trust_cleanup(ca_id,ca_version,next_action_at) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(&id).bind(previous).bind(deadline).execute(&mut **tx).await?;
        }
        crate::credentials::rotations::cancelled::enqueue_on(tx, &previous_id, &id, previous)
            .await?;
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReissueUser {
    pub ca_credential_id: Option<String>,
    pub expected_revisions: std::collections::BTreeMap<String, i64>,
    pub idempotency_key: String,
}

pub(super) async fn user_identities(state: &AppState, user: &str) -> Result<Vec<Value>, ApiError> {
    let rows=sqlx::query("SELECT i.*,c.name AS ca_name,a.published_version,a.policy_revision,g.expires_at AS ca_expires_at,coalesce((SELECT jsonb_agg(jsonb_build_object('version',v.credential_version,'ca_version',v.ca_version,'not_before',v.not_before,'expires_at',v.expires_at,'released',v.released,'discarded',v.discarded) ORDER BY v.credential_version DESC) FROM mtls_user_version_publication v WHERE v.identity_id=i.id),'[]'::jsonb) AS versions,coalesce((SELECT jsonb_agg(DISTINCT t.node_id ORDER BY t.node_id) FROM (SELECT node_id,ca_id FROM mtls_node_trust WHERE desired OR deployed UNION SELECT node_id,ca_id FROM mtls_live_activation_trust) t JOIN node_assignments na ON na.node_id=t.node_id AND na.user_id=i.user_id WHERE t.ca_id=i.ca_id),'[]'::jsonb) AS node_ids FROM mtls_user_identities i JOIN mtls_authorities a ON a.credential_id=i.ca_id JOIN credentials c ON c.id=i.ca_id JOIN mtls_ca_generations g ON g.ca_id=i.ca_id AND g.ca_version=a.published_version WHERE i.user_id=$1 ORDER BY c.name,i.ca_id")
        .bind(user).fetch_all(&mut *crate::read_snapshot::begin(&state.pool).await?).await?;
    Ok(rows.into_iter().map(|row|json!({"identity_id":row.get::<String,_>("id"),"ca_credential_id":row.get::<String,_>("ca_id"),"ca_name":row.get::<String,_>("ca_name"),"revision":row.get::<i64,_>("revision"),"authorization_revision":row.get::<i64,_>("authorization_revision"),"credential_id":row.get::<Option<String>,_>("credential_id"),"archived":row.get::<bool,_>("archived"),"maintenance_error":row.get::<Option<String>,_>("maintenance_error"),"next_action_at":row.get::<Option<chrono::DateTime<Utc>>,_>("next_action_at"),"published_ca_version":row.get::<i64,_>("published_version"),"ca_expires_at":row.get::<chrono::DateTime<Utc>,_>("ca_expires_at"),"policy_revision":row.get::<i64,_>("policy_revision"),"versions":row.get::<Value,_>("versions"),"node_ids":row.get::<Value,_>("node_ids")})).collect())
}

#[cfg(test)]
pub(crate) mod tests;

mod preparation;
use preparation::*;

pub(super) mod v2;

// Direct domain fixtures are test-only; production writes use protocol-2 handlers.
#[cfg(test)]
#[path = "managed_mtls_test_support.rs"]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::*;

#[cfg(test)]
use crate::db;
