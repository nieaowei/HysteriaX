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
mod tests {
    use super::*;

    #[tokio::test]
    async fn creation_and_publication_native_retries_preserve_prepared_ids_ciphertexts_and_batch() {
        let (state, _) = super::super::transaction_tests::fixture().await;
        let prepared = Creation::new(
            &state.secrets,
            CreateCredential {
                name: "Fixed".into(),
                kind: "api_token".into(),
                reminder_at: None,
                payload: json!({"token":"fixed-secret"}),
            },
        )
        .unwrap();
        let (id, cipher) = (prepared.id.clone(), prepared.ciphertext.clone());
        sqlx::raw_sql("CREATE SEQUENCE preparation_attempt; CREATE FUNCTION retry_preparation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('preparation_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_preparation BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_preparation()")
            .execute(&state.pool).await.unwrap();
        let created = crate::db::serializable(&state.pool, move |tx| {
            let prepared = prepared.clone();
            Box::pin(async move {
                prepared
                    .commit_on(
                        tx,
                        &AdminActor {
                            id: "prepared-test".into(),
                        },
                    )
                    .await
            })
        })
        .await
        .unwrap();
        assert_eq!(created["id"], id);
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT payload_enc FROM credential_versions WHERE credential_id=$1 AND version=1"
            )
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
            cipher
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT last_value FROM preparation_attempt")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            2
        );
        sqlx::raw_sql("ALTER SEQUENCE preparation_attempt RESTART WITH 1; CREATE OR REPLACE FUNCTION retry_preparation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('preparation_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40P01'; END IF; RETURN NEW; END $$")
            .execute(&state.pool).await.unwrap();
        let prepared = Publication::new(
            &state,
            &id,
            &PublishVersion {
                expected_revision: 1,
                payload: json!({"token":"new-secret"}),
            },
        )
        .await
        .unwrap();
        let (cipher, batch) = (prepared.ciphertext.clone(), prepared.batch.clone());
        let current = id.clone();
        let published = crate::db::serializable(&state.pool, move |tx| {
            let (prepared, id) = (prepared.clone(), current.clone());
            Box::pin(async move {
                super::super::mutations::publish_on(
                    tx,
                    id,
                    1,
                    prepared,
                    AdminActor {
                        id: "prepared-test".into(),
                    },
                )
                .await
            })
        })
        .await
        .unwrap();
        assert_eq!(published["batch_id"], batch);
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT payload_enc FROM credential_versions WHERE credential_id=$1 AND version=2"
            )
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
            cipher
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT last_value FROM preparation_attempt")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn changed_revision_archive_kind_or_owner_rejects_prepared_publication_without_writing() {
        for (change, status) in [
            ("revision=revision+1", axum::http::StatusCode::CONFLICT),
            ("archived=TRUE", axum::http::StatusCode::CONFLICT),
            ("kind='api_token'", axum::http::StatusCode::CONFLICT),
            ("owner_user_id='owner'", axum::http::StatusCode::BAD_REQUEST),
        ] {
            let (state, id) = super::super::transaction_tests::fixture().await;
            let prepared = Publication::new(
                &state,
                &id,
                &PublishVersion {
                    expected_revision: 1,
                    payload: json!({"secret":"candidate"}),
                },
            )
            .await
            .unwrap();
            sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('owner','Owner',clock_timestamp(),clock_timestamp())").execute(&state.pool).await.unwrap();
            sqlx::query(&format!("UPDATE credentials SET {change} WHERE id=$1"))
                .bind(&id)
                .execute(&state.pool)
                .await
                .unwrap();
            let query = "SELECT jsonb_build_object('credentials',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM credentials t),'versions',(SELECT jsonb_agg(to_jsonb(t) ORDER BY credential_id,version) FROM credential_versions t),'batches',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM credential_batches t),'jobs',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM jobs t),'audits',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM audit_records t))";
            let before: Value = sqlx::query_scalar(query)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            let error = crate::db::serializable(&state.pool, move |tx| {
                let (prepared, id) = (prepared.clone(), id.clone());
                Box::pin(async move {
                    super::super::mutations::publish_on(
                        tx,
                        id,
                        1,
                        prepared,
                        AdminActor {
                            id: "prepared-test".into(),
                        },
                    )
                    .await
                })
            })
            .await
            .unwrap_err();
            assert_eq!(error.status, status);
            assert_eq!(
                sqlx::query_scalar::<_, Value>(query)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap(),
                before
            );
        }
    }
}
