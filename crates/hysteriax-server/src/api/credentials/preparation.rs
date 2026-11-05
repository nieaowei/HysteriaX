use super::{AdminActor, CreateCredential, PublishVersion, invalid, name, record};
use crate::{credentials as vault, error::ApiError, security::SecretBox, state::AppState};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Clone)]
pub(super) struct Creation {
    id: String,
    name: String,
    kind: String,
    reminder: Option<DateTime<Utc>>,
    metadata: Value,
    ciphertext: String,
    timestamp: DateTime<Utc>,
}

impl Creation {
    pub(super) fn new(secrets: &SecretBox, input: CreateCredential) -> Result<Self, ApiError> {
        let name = name(&input.name)?.to_owned();
        if input.kind == "ca_identity" {
            return Err(ApiError::bad_request(
                "use the CA identity creation endpoint",
            ));
        }
        let metadata = vault::validate_payload(&input.kind, &input.payload).map_err(invalid)?;
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            name,
            kind: input.kind,
            reminder: input.reminder_at,
            metadata,
            ciphertext: secrets.encrypt(&input.payload.to_string())?,
            timestamp: Utc::now(),
        })
    }

    pub(super) async fn commit_on(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        actor: &AdminActor,
    ) -> Result<Value, ApiError> {
        sqlx::query("INSERT INTO credentials(id,name,kind,owner_user_id,created_at,updated_at) VALUES($1,$2,$3,NULL,$4,$4)")
            .bind(&self.id).bind(&self.name).bind(&self.kind).bind(self.timestamp).execute(&mut **tx).await?;
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,artifact_names,created_at) VALUES($1,1,$2,$3,'{}'::jsonb,$4)")
            .bind(&self.id).bind(&self.ciphertext).bind(&self.metadata).bind(self.timestamp).execute(&mut **tx).await?;
        sqlx::query("UPDATE credentials SET reminder_at=$1 WHERE id=$2")
            .bind(self.reminder)
            .bind(&self.id)
            .execute(&mut **tx)
            .await?;
        record(
            tx,
            actor,
            "credential.created",
            &self.id,
            json!({"kind":self.kind,"version":1}),
        )
        .await?;
        Ok(json!({"id":self.id,"revision":1,"version":1}))
    }
}

#[derive(Clone)]
pub(super) struct Publication {
    pub kind: String,
    pub metadata: Value,
    pub ciphertext: String,
    pub batch: String,
    pub timestamp: DateTime<Utc>,
}

impl Publication {
    pub(super) async fn new(
        state: &AppState,
        id: &str,
        input: &PublishVersion,
    ) -> Result<Self, ApiError> {
        // An unlocked snapshot is only preparation, never authority to publish.
        // The commit repeats all source checks after canonical resource locking.
        let row = sqlx::query("SELECT c.kind,c.revision,c.archived,(c.owner_user_id IS NOT NULL OR EXISTS(SELECT 1 FROM credential_versions v WHERE v.credential_id=c.id AND v.metadata->>'managed'='true')) AS managed FROM credentials c WHERE c.id=$1")
            .bind(id).fetch_optional(&state.pool).await?.ok_or_else(||ApiError::not_found("credential"))?;
        if row.get::<i64, _>("revision") != input.expected_revision
            || row.get::<bool, _>("archived")
        {
            return Err(ApiError::conflict("credential changed or is archived"));
        }
        let kind: String = row.get("kind");
        if kind == "ca_identity" {
            return Err(ApiError::bad_request(
                "CA versions require managed rotation",
            ));
        }
        if row.get::<bool, _>("managed") {
            return Err(ApiError::bad_request(
                "system-issued identities cannot be edited",
            ));
        }
        let metadata = vault::validate_payload(&kind, &input.payload).map_err(invalid)?;
        Ok(Self {
            kind,
            metadata,
            ciphertext: state.secrets.encrypt(&input.payload.to_string())?,
            batch: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
        })
    }
}

#[cfg(test)]
#[path = "preparation_tests.rs"]
mod tests;
