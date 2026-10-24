use super::*;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(super) struct PreparedPublication {
    id: String,
    revision: i64,
    source_version: Option<i64>,
    format: String,
    ciphertext: String,
}
impl PreparedPublication {
    pub(super) async fn new(
        state: &AppState,
        id: &str,
        revision: i64,
        source_version: Option<i64>,
    ) -> Result<Self, ApiError> {
        let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
        let current = sqlx::query("SELECT * FROM subscription_templates WHERE id=$1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| ApiError::not_found("template"))?;
        if current.get::<i64, _>("revision") != revision {
            return Err(ApiError::conflict(
                "template changed; local edits must be retained and compared before retrying",
            ));
        }
        if current.get::<bool, _>("archived") {
            return Err(ApiError::conflict("template is archived"));
        }
        let format: String = current.get("format");
        let ciphertext = if let Some(version) = source_version {
            sqlx::query_scalar::<_,String>("SELECT document_enc FROM subscription_template_versions WHERE template_id=$1 AND version=$2").bind(id).bind(version).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::not_found("template version"))?
        } else {
            current.get("draft_enc")
        };
        tx.commit().await?;
        let document = decode(&state.secrets, &ciphertext)?;
        ensure_valid(&validate(&format, &document)?)?;
        super::super::verify_rendering(&format, &document)?;
        Ok(Self {
            id: id.to_owned(),
            revision,
            source_version,
            format,
            ciphertext,
        })
    }

    pub(super) async fn commit_on(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Json<Value>, ApiError> {
        let current = locked(tx, &self.id, self.revision).await?;
        let cipher = if let Some(version) = self.source_version {
            sqlx::query_scalar::<_,String>("SELECT document_enc FROM subscription_template_versions WHERE template_id=$1 AND version=$2").bind(&self.id).bind(version).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::not_found("template version"))?
        } else {
            current.get("draft_enc")
        };
        if current.get::<String, _>("format") != self.format || cipher != self.ciphertext {
            return Err(ApiError::conflict(
                "template source changed during validation; reload before publishing",
            ));
        }
        let next:i64=sqlx::query_scalar("SELECT coalesce(max(version),0)+1 FROM subscription_template_versions WHERE template_id=$1").bind(&self.id).fetch_one(&mut **tx).await?;
        sqlx::query("INSERT INTO subscription_template_versions(template_id,version,document_enc) VALUES($1,$2,$3)").bind(&self.id).bind(next).bind(&self.ciphertext).execute(&mut **tx).await?;
        let row=sqlx::query("UPDATE subscription_templates SET published_version=$2,draft_enc=$3,revision=revision+1,updated_at=now() WHERE id=$1 RETURNING *").bind(&self.id).bind(next).bind(&self.ciphertext).fetch_one(&mut **tx).await?;
        audit_with_source(
            tx,
            &self.id,
            if self.source_version.is_some() {
                "template.rolled_back"
            } else {
                "template.published"
            },
            row.get("revision"),
            Some(next),
            self.source_version,
        )
        .await?;
        Ok(Json(metadata(&row)))
    }
}

#[derive(Clone)]
pub(super) struct PreparedCreation {
    id: String,
    name: String,
    format: String,
    ciphertext: String,
}
impl PreparedCreation {
    pub(super) fn new(state: &AppState, input: Create) -> Result<Self, ApiError> {
        if input.expected_revision != 0 || !["mihomo", "singbox"].contains(&input.format.as_str()) {
            return Err(ApiError::bad_request(
                "new templates require revision 0 and a supported format",
            ));
        }
        let name = name(&input.name)?.to_owned();
        let document = input
            .document
            .unwrap_or_else(|| render::single_seed(&input.format));
        document.shape()?;
        let id = Uuid::new_v4().to_string();
        let encrypted = state
            .secrets
            .encrypt(&serde_json::to_string(&document).map_err(|_| ApiError::internal())?)?;
        Ok(Self {
            id,
            name,
            format: input.format,
            ciphertext: encrypted,
        })
    }
    pub(super) async fn commit_on(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<(StatusCode, Json<Value>), ApiError> {
        let row=sqlx::query("INSERT INTO subscription_templates(id,name,format,draft_enc) VALUES($1,$2,$3,$4) RETURNING *")
            .bind(&self.id).bind(&self.name).bind(&self.format).bind(&self.ciphertext).fetch_one(&mut **tx).await?;
        audit(tx, &self.id, "template.created", 1, None).await?;
        Ok((StatusCode::CREATED, Json(metadata(&row))))
    }
}

pub(super) fn prepare_save(state: &AppState, input: &Save) -> Result<String, ApiError> {
    input.document.shape()?;
    let encrypted = state
        .secrets
        .encrypt(&serde_json::to_string(&input.document).map_err(|_| ApiError::internal())?)?;
    Ok(encrypted)
}
pub(super) async fn save_on(
    tx: &mut Transaction<'_, Postgres>,
    id: String,
    revision: i64,
    encrypted: String,
) -> Result<Json<Value>, ApiError> {
    locked(tx, &id, revision).await?;
    let row=sqlx::query("UPDATE subscription_templates SET draft_enc=$2,revision=revision+1,updated_at=now() WHERE id=$1 RETURNING *").bind(&id).bind(encrypted).fetch_one(&mut **tx).await?;
    audit(tx, &id, "template.draft_saved", row.get("revision"), None).await?;
    Ok(Json(metadata(&row)))
}
