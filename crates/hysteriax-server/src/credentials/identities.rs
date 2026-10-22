//! Shared user × CA identities. Expensive preparation is outside the global
//! write lock; publication validates independent identity/policy revisions.
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use super::{
    ca::{Authority, Issued, Policy},
    load,
};
use crate::security::SecretBox;

#[derive(Clone)]
pub struct PreparedIdentity {
    id: String,
    user_id: String,
    ca_id: String,
    ca_version: i64,
    policy_revision: i64,
    user_revision: i64,
    expected_identity_revision: Option<i64>,
    authorization_revision: Option<i64>,
    generation_state: String,
    issued: Option<Issued>,
    existing_version: Option<i64>,
    needs_ca_update: bool,
    prepared_material: Option<(String, Value, String)>,
}

impl PreparedIdentity {
    pub fn will_issue(&self) -> bool {
        self.issued.is_some()
    }
    pub(crate) fn matches_rotation(
        &self,
        identity: &str,
        user: &str,
        ca: &str,
        version: i64,
    ) -> bool {
        self.id == identity
            && self.user_id == user
            && self.ca_id == ca
            && self.ca_version == version
            && self.generation_state == "preparing"
    }
}

pub async fn prepare_user(
    pool: &PgPool,
    secrets: &SecretBox,
    user_id: &str,
    ca_id: &str,
    ca_version: i64,
    force: bool,
) -> Result<PreparedIdentity> {
    prepare_user_with_identity(pool, secrets, user_id, ca_id, ca_version, force, None).await
}

async fn prepare_user_with_identity(
    pool: &PgPool,
    secrets: &SecretBox,
    user_id: &str,
    ca_id: &str,
    ca_version: i64,
    force: bool,
    identity_id: Option<&str>,
) -> Result<PreparedIdentity> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await?;
    let row = sqlx::query("SELECT a.policy_revision,a.client_validity_days,a.renew_before_hours,g.state,c.archived,u.revision AS user_revision,u.enabled,u.expires_at,u.quota_bytes,u.usage_bytes FROM mtls_authorities a JOIN mtls_ca_generations g ON g.ca_id=a.credential_id AND g.ca_version=$3 JOIN credentials c ON c.id=a.credential_id CROSS JOIN users u WHERE a.credential_id=$2 AND u.id=$1")
        .bind(user_id).bind(ca_id).bind(ca_version).fetch_one(&mut *tx).await?;
    ensure!(!row.get::<bool, _>("archived"), "CA identity is archived");
    let generation_state: String = row.get("state");
    ensure!(
        generation_state != "retired",
        "retired CA generations cannot sign new identities"
    );
    let eligible = row.get::<bool, _>("enabled")
        && row
            .get::<Option<DateTime<Utc>>, _>("expires_at")
            .is_none_or(|t| t > now)
        && row
            .get::<Option<i64>, _>("quota_bytes")
            .is_none_or(|q| row.get::<i64, _>("usage_bytes") < q);
    let identity = sqlx::query("SELECT id,revision,authorization_revision,credential_id FROM mtls_user_identities WHERE user_id=$1 AND ca_id=$2")
        .bind(user_id).bind(ca_id).fetch_optional(&mut *tx).await?;
    let id: String = identity.as_ref().map(|r| r.get("id")).unwrap_or_else(|| {
        identity_id
            .map(str::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().to_string())
    });
    let existing: Option<i64> = sqlx::query_scalar("SELECT v.credential_version FROM mtls_user_versions v JOIN mtls_ca_generations g ON g.ca_id=v.ca_id AND g.ca_version=v.ca_version WHERE v.identity_id=$1 AND v.ca_id=$2 AND v.ca_version=$3 AND NOT v.discarded AND v.not_before<=$4 AND v.expires_at>$4 AND (v.next_action_at>$4 OR v.expires_at>=g.expires_at-interval '5 minutes') ORDER BY v.credential_version DESC LIMIT 1")
        .bind(&id).bind(ca_id).bind(ca_version).bind(now).fetch_optional(&mut *tx).await?;
    let needs_ca_update = if let Some(version) = existing {
        sqlx::query_scalar::<_,bool>("SELECT next_action_at<=$3 FROM mtls_user_versions WHERE identity_id=$1 AND credential_version=$2").bind(&id).bind(version).bind(now).fetch_one(&mut *tx).await?
    } else {
        false
    };
    let previous_expiry: Option<DateTime<Utc>> = sqlx::query_scalar("SELECT max(expires_at) FROM mtls_user_versions WHERE identity_id=$1 AND ca_version=$2 AND NOT discarded")
        .bind(&id).bind(ca_version).fetch_one(&mut *tx).await?;
    let expected_identity_revision = identity.as_ref().map(|r| r.get("revision"));
    let authorization_revision = identity.as_ref().map(|r| r.get("authorization_revision"));
    let policy = Policy {
        client_validity_days: u16::try_from(row.get::<i32, _>("client_validity_days"))?,
        renew_before_hours: u16::try_from(row.get::<i32, _>("renew_before_hours"))?,
    };
    let policy_revision = row.get("policy_revision");
    let user_revision = row.get("user_revision");
    // Decrypt the immutable signing version on the same read snapshot.
    let payload = if eligible && (force || existing.is_none()) {
        let cipher: String = sqlx::query_scalar(
            "SELECT payload_enc FROM credential_versions WHERE credential_id=$1 AND version=$2",
        )
        .bind(ca_id)
        .bind(ca_version)
        .fetch_one(&mut *tx)
        .await?;
        Some(serde_json::from_str::<Value>(&secrets.decrypt(&cipher)?)?)
    } else {
        None
    };
    tx.commit().await?;
    let signing_id = Uuid::parse_str(&id)?;
    let issued = if let Some(payload) = payload {
        let (issued, ca_expiry) = tokio::task::spawn_blocking(move || {
            let authority = Authority::from_payload(&payload, now)?;
            Ok::<_, anyhow::Error>((
                authority.sign_user(&signing_id, policy, now)?,
                authority.expires_at,
            ))
        })
        .await??;
        ensure!(
            force
                || issued.expires_at < ca_expiry - chrono::Duration::minutes(5)
                || previous_expiry.is_none_or(|previous| issued.expires_at > previous),
            "CA update required: cannot extend the existing certificate"
        );
        Some(issued)
    } else {
        None
    };
    let prepared_material = if let Some(issued) = &issued {
        let payload = issued.material.payload();
        let mut metadata = super::validate_payload("tls_identity", &payload)?;
        metadata["managed"] = json!(true);
        metadata["identity_id"] = json!(id);
        metadata["ca_id"] = json!(ca_id);
        metadata["ca_version"] = json!(ca_version);
        metadata["policy_revision"] = json!(policy_revision);
        Some((
            secrets.encrypt(&payload.to_string())?,
            metadata,
            Uuid::new_v4().to_string(),
        ))
    } else {
        None
    };
    Ok(PreparedIdentity {
        id,
        user_id: user_id.into(),
        ca_id: ca_id.into(),
        ca_version,
        policy_revision,
        user_revision,
        expected_identity_revision,
        authorization_revision,
        generation_state,
        issued,
        existing_version: existing,
        needs_ca_update,
        prepared_material,
    })
}

/// Returns the shared identity ID and the chosen credential version. The caller
/// commits this in the same transaction as its authorization change.
pub async fn commit_user(
    tx: &mut Transaction<'_, Postgres>,
    _secrets: &SecretBox,
    candidate: PreparedIdentity,
) -> Result<(String, Option<i64>)> {
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    let ca = sqlx::query("SELECT a.policy_revision,g.state,c.archived FROM mtls_authorities a JOIN mtls_ca_generations g ON g.ca_id=a.credential_id AND g.ca_version=$2 JOIN credentials c ON c.id=a.credential_id WHERE a.credential_id=$1")
        .bind(&candidate.ca_id).bind(candidate.ca_version).fetch_one(&mut **tx).await?;
    ensure!(
        !ca.get::<bool, _>("archived")
            && ca.get::<i64, _>("policy_revision") == candidate.policy_revision
            && ca.get::<String, _>("state") == candidate.generation_state,
        "CA changed during signing; retry"
    );
    let user = sqlx::query(
        "SELECT revision,enabled,expires_at,quota_bytes,usage_bytes FROM users WHERE id=$1",
    )
    .bind(&candidate.user_id)
    .fetch_one(&mut **tx)
    .await?;
    ensure!(
        user.get::<i64, _>("revision") == candidate.user_revision,
        "user changed during signing; retry"
    );
    let eligible = user.get::<bool, _>("enabled")
        && user
            .get::<Option<DateTime<Utc>>, _>("expires_at")
            .is_none_or(|t| t > now)
        && user
            .get::<Option<i64>, _>("quota_bytes")
            .is_none_or(|q| user.get::<i64, _>("usage_bytes") < q);
    ensure!(
        candidate.issued.is_none() || eligible,
        "user became ineligible during signing; retry"
    );
    let identity = sqlx::query("SELECT id,revision,authorization_revision,credential_id FROM mtls_user_identities WHERE user_id=$1 AND ca_id=$2").bind(&candidate.user_id).bind(&candidate.ca_id).fetch_optional(&mut **tx).await?;
    ensure!(
        identity.as_ref().map(|r| r.get::<i64, _>("revision"))
            == candidate.expected_identity_revision
            && identity
                .as_ref()
                .map(|r| r.get::<i64, _>("authorization_revision"))
                == candidate.authorization_revision,
        "shared identity changed during signing; retry"
    );
    if identity.is_none() {
        sqlx::query("INSERT INTO mtls_user_identities(id,user_id,ca_id,created_at,updated_at) VALUES($1,$2,$3,$4,$4)")
            .bind(&candidate.id).bind(&candidate.user_id).bind(&candidate.ca_id).bind(now).execute(&mut **tx).await?;
    }
    let Some(issued) = candidate.issued else {
        if let Some(version) = candidate.existing_version {
            let usable:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_user_versions WHERE identity_id=$1 AND credential_version=$2 AND NOT discarded AND not_before<=$3 AND expires_at>$3)").bind(&candidate.id).bind(version).bind(now).fetch_one(&mut **tx).await?;
            ensure!(
                usable,
                "shared certificate expired or was discarded during preparation; retry"
            );
        }
        sqlx::query("UPDATE mtls_user_identities i SET archived=FALSE,maintenance_error=CASE WHEN $3 THEN 'CA update required: cannot extend the existing certificate' ELSE maintenance_error END,next_action_at=CASE WHEN $3 THEN $2+interval '1 hour' WHEN $4 THEN (SELECT next_action_at FROM mtls_user_versions WHERE identity_id=i.id AND credential_version=$5) ELSE next_action_at END,updated_at=$2 WHERE i.id=$1")
            .bind(&candidate.id).bind(now) .bind(candidate.needs_ca_update && candidate.generation_state=="published").bind(candidate.generation_state=="published").bind(candidate.existing_version).execute(&mut **tx).await?;
        return Ok((candidate.id, candidate.existing_version));
    };
    ensure!(
        issued.expires_at > now,
        "prepared certificate expired; retry"
    );
    let (payload_enc, metadata, new_credential) = candidate
        .prepared_material
        .context("prepared identity material is missing")?;
    let credential: Option<String> = identity.as_ref().and_then(|r| r.get("credential_id"));
    let (credential, version) = if let Some(credential) = credential {
        let version: i64 =
            sqlx::query_scalar("SELECT latest_version+1 FROM credentials WHERE id=$1")
                .bind(&credential)
                .fetch_one(&mut **tx)
                .await?;
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES($1,$2,$3,$4,$5)")
            .bind(&credential).bind(version).bind(payload_enc).bind(&metadata).bind(now).execute(&mut **tx).await?;
        sqlx::query("UPDATE credentials SET latest_version=$2,revision=revision+1,archived=FALSE,updated_at=$3 WHERE id=$1").bind(&credential).bind(version).bind(now).execute(&mut **tx).await?;
        (credential, version)
    } else {
        sqlx::query("INSERT INTO credentials(id,name,kind,owner_user_id,created_at,updated_at) VALUES($1,$2,'tls_identity',$3,$4,$4)")
            .bind(&new_credential).bind(format!("Managed mTLS · {}",candidate.id)).bind(&candidate.user_id).bind(now).execute(&mut **tx).await?;
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES($1,1,$2,$3,$4)")
            .bind(&new_credential).bind(payload_enc).bind(&metadata).bind(now).execute(&mut **tx).await?;
        (new_credential, 1)
    };
    let released = candidate.generation_state == "published";
    sqlx::query("UPDATE mtls_user_identities SET credential_id=$2,revision=revision+1,archived=FALSE,next_action_at=CASE WHEN $3 AND $4<=$5 THEN $5+interval '1 hour' WHEN $3 THEN $4 ELSE next_action_at END,maintenance_error=CASE WHEN $3 AND $4<=$5 THEN 'CA update required: cannot extend the existing certificate' WHEN $3 THEN NULL ELSE maintenance_error END,updated_at=$5 WHERE id=$1")
        .bind(&candidate.id).bind(&credential).bind(released).bind(issued.next_action_at).bind(now).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO mtls_user_versions(identity_id,credential_id,credential_version,ca_id,ca_version,not_before,expires_at,next_action_at,released,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
        .bind(&candidate.id).bind(&credential).bind(version).bind(&candidate.ca_id).bind(candidate.ca_version).bind(issued.not_before).bind(issued.expires_at).bind(issued.next_action_at).bind(released).bind(now).execute(&mut **tx).await?;
    Ok((candidate.id, Some(version)))
}

/// Resolve only released, time-valid versions trusted by this node. Retained
/// historical versions provide controlled rollback compatibility without issuing
/// a second per-node identity or treating latest_version as publication state.
pub async fn for_node(
    pool: &PgPool,
    secrets: &SecretBox,
    user: &str,
    node: &str,
    ca: &str,
) -> Result<Option<(String, String)>> {
    let now = Utc::now();
    let row = sqlx::query("SELECT v.credential_id,v.credential_version FROM mtls_user_identities i JOIN mtls_user_versions v ON v.identity_id=i.id JOIN mtls_node_trust t ON t.ca_id=v.ca_id AND t.ca_version=v.ca_version JOIN mtls_authorities a ON a.credential_id=i.ca_id WHERE i.user_id=$1 AND i.ca_id=$2 AND NOT i.archived AND t.node_id=$3 AND t.deployed AND v.released AND NOT v.discarded AND v.not_before<=$4 AND v.expires_at>$4 ORDER BY (v.ca_version=a.published_version) DESC,v.ca_version DESC,v.credential_version DESC LIMIT 1")
        .bind(user).bind(ca).bind(node).bind(now).fetch_optional(&mut *crate::read_snapshot::begin(pool).await?).await?;
    let Some(row) = row else { return Ok(None) };
    let (kind, owner, _, payload, _) = load(
        pool,
        secrets,
        &row.get::<String, _>("credential_id"),
        row.get("credential_version"),
    )
    .await?;
    ensure!(
        kind == "tls_identity" && owner.as_deref() == Some(user),
        "invalid managed identity owner"
    );
    let certificate = payload["certificate"]
        .as_str()
        .context("managed certificate missing")?
        .into();
    let private_key = payload["private_key"]
        .as_str()
        .context("managed key missing")?
        .into();
    Ok(Some((certificate, private_key)))
}

pub async fn prepare_versions(
    pool: &PgPool,
    secrets: &SecretBox,
    user: &str,
    ca: &str,
    versions: &[i64],
) -> Result<Vec<PreparedIdentity>> {
    let mut candidates: Vec<PreparedIdentity> = Vec::new();
    for version in versions {
        let id = candidates.first().map(|candidate| candidate.id.as_str());
        let candidate =
            prepare_user_with_identity(pool, secrets, user, ca, *version, false, id).await?;
        if let Some(first) = candidates.first() {
            ensure!(
                candidate.id == first.id
                    && candidate.expected_identity_revision == first.expected_identity_revision
                    && candidate.authorization_revision == first.authorization_revision
                    && candidate.user_revision == first.user_revision
                    && candidate.policy_revision == first.policy_revision,
                "shared identity dependencies changed during batch signing; preview again"
            );
        }
        candidates.push(candidate);
    }
    Ok(candidates)
}

pub async fn commit_versions(
    tx: &mut Transaction<'_, Postgres>,
    secrets: &SecretBox,
    candidates: Vec<PreparedIdentity>,
) -> Result<Vec<(String, Option<i64>)>> {
    if let Some(first) = candidates.first() {
        ensure!(
            candidates.iter().all(|candidate| candidate.id == first.id
                && candidate.user_id == first.user_id
                && candidate.ca_id == first.ca_id
                && candidate.expected_identity_revision == first.expected_identity_revision
                && candidate.authorization_revision == first.authorization_revision
                && candidate.user_revision == first.user_revision
                && candidate.policy_revision == first.policy_revision),
            "batch signing dependencies do not describe one stable identity"
        );
    }
    let mut output = Vec::new();
    for (index, mut candidate) in candidates.into_iter().enumerate() {
        if index > 0 {
            let current = sqlx::query(
                "SELECT revision,authorization_revision FROM mtls_user_identities WHERE id=$1",
            )
            .bind(&candidate.id)
            .fetch_one(&mut **tx)
            .await?;
            candidate.expected_identity_revision = Some(current.get("revision"));
            candidate.authorization_revision = Some(current.get("authorization_revision"));
        }
        output.push(commit_user(tx, secrets, candidate).await?);
    }
    Ok(output)
}
