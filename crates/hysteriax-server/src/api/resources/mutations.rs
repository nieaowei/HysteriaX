use axum::{
    Json,
    body::Bytes,
    extract::{Extension, OriginalUri, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::Digest;
use sqlx::PgConnection;
use uuid::Uuid;

use super::{
    CreateResource, MAX_RESOURCE_BYTES, editable_node_on, safe_name, validate_text_resource,
};
use crate::{
    api::{
        AdminActor, audit_on,
        idempotency::{RequestIdentity, StoredResponse, execute, replay},
        now,
    },
    error::ApiError,
    security::SecretBox,
    state::AppState,
};

#[derive(Clone)]
pub(super) struct Prepared {
    id: String,
    name: String,
    kind: String,
    digest: String,
    size: i64,
    ciphertext: String,
    timestamp: DateTime<Utc>,
}

impl Prepared {
    pub(super) fn new(secrets: &SecretBox, input: CreateResource) -> Result<Self, ApiError> {
        if !safe_name(&input.name) {
            return Err(ApiError::bad_request(
                "name must be a single filename without path separators",
            ));
        }
        if !matches!(input.resource_kind.as_str(), "acl" | "geoip" | "geosite") {
            return Err(ApiError::bad_request("unsupported config resource kind"));
        }
        let content = STANDARD
            .decode(input.content_base64.as_bytes())
            .map_err(|_| ApiError::bad_request("content_base64 is invalid"))?;
        if content.is_empty() || content.len() > MAX_RESOURCE_BYTES {
            return Err(ApiError::bad_request(
                "resource must be between 1 byte and 20 MiB",
            ));
        }
        validate_text_resource(&input.resource_kind, &content)?;
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            name: input.name.trim().into(),
            kind: input.resource_kind,
            digest: hex::encode(sha2::Sha256::digest(&content)),
            size: content.len() as i64,
            ciphertext: secrets.encrypt_bytes(&content)?,
            timestamp: now(),
        })
    }

    pub(super) async fn commit_on(
        &self,
        tx: &mut PgConnection,
        node: &str,
        actor: &str,
    ) -> Result<Value, ApiError> {
        editable_node_on(tx, node).await?;
        let inserted = sqlx::query("INSERT INTO config_resources(id,node_id,name,resource_kind,content_enc,content_sha256,size_bytes,created_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(node_id,name) DO NOTHING")
            .bind(&self.id).bind(node).bind(&self.name).bind(&self.kind).bind(&self.ciphertext).bind(&self.digest).bind(self.size).bind(self.timestamp)
            .execute(&mut *tx).await?;
        if inserted.rows_affected() != 1 {
            return Err(ApiError::conflict(
                "a resource with this name already exists on the node",
            ));
        }
        audit_on(tx,actor,"node.resource_uploaded","node",node,json!({"resource_id":self.id,"name":self.name,"kind":self.kind,"sha256":self.digest,"size_bytes":self.size})).await?;
        Ok(
            json!({"id":self.id,"name":self.name,"resource_kind":self.kind,"content_sha256":self.digest,"size_bytes":self.size,"reference":format!("resource://{}",self.id)}),
        )
    }
}

pub(super) async fn delete_on(
    tx: &mut PgConnection,
    node: &str,
    resource: &str,
    actor: &str,
) -> Result<(), ApiError> {
    editable_node_on(tx, node).await?;
    crate::credentials::reference_index::assert_clean_on(tx).await?;
    let name: String = sqlx::query_scalar(
        "SELECT name FROM config_resources WHERE node_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(node)
    .bind(resource)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::not_found("config resource"))?;
    let referenced: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM config_resource_references WHERE node_id=$1 AND resource_id=$2)")
        .bind(node).bind(resource).fetch_one(&mut *tx).await?;
    if referenced {
        return Err(ApiError::conflict(
            "resource is referenced by a current, deployed or saved node configuration",
        ));
    }
    sqlx::query("DELETE FROM config_resources WHERE node_id=$1 AND id=$2")
        .bind(node)
        .bind(resource)
        .execute(&mut *tx)
        .await?;
    audit_on(
        tx,
        actor,
        "node.resource_deleted",
        "node",
        node,
        json!({"resource_id":resource,"name":name}),
    )
    .await
}

pub(crate) async fn create_v2(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(node): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "nodes.resources.create",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"node":node,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid resource upload request"))?;
    let prepared = Prepared::new(&state.secrets, input)?;
    execute(&state, identity, move |tx| {
        let (prepared, node, actor) = (prepared.clone(), node.clone(), actor.id.clone());
        Box::pin(async move {
            Ok(StoredResponse::json(
                StatusCode::CREATED,
                prepared.commit_on(tx, &node, &actor).await?,
            ))
        })
    })
    .await?
    .into_http()
}

pub(crate) async fn delete_v2(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path((node, resource)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "nodes.resources.delete",
        &headers,
        json!({"method":"DELETE","uri":uri.to_string(),"node":node,"resource":resource,"body":body.as_ref()}),
    )?;
    execute(&state, identity, move |tx| {
        let (node, resource, actor) = (node.clone(), resource.clone(), actor.id.clone());
        Box::pin(async move {
            delete_on(tx, &node, &resource, &actor).await?;
            Ok(StoredResponse::empty(StatusCode::NO_CONTENT))
        })
    })
    .await?
    .into_http()
}

#[cfg(test)]
mod tests;
