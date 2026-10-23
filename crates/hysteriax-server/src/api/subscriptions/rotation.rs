use axum::{
    Json,
    extract::{Extension, OriginalUri, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{
    api::{
        AdminActor, generate_token,
        idempotency::{RequestIdentity, StoredResponse, execute, replay},
        now,
    },
    error::ApiError,
    security::{SecretBox, token_digest},
    state::AppState,
};

#[derive(Clone)]
pub(super) struct Prepared {
    id: String,
    plaintext: String,
    hash: String,
    ciphertext: String,
}

impl Prepared {
    pub(super) fn new(secrets: &SecretBox) -> Result<Self, ApiError> {
        let plaintext = generate_token();
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            hash: token_digest(&plaintext),
            ciphertext: secrets.encrypt(&plaintext)?,
            plaintext,
        })
    }

    pub(super) async fn commit_on(
        &self,
        tx: &mut PgConnection,
        user: &str,
        expected: i64,
        actor: &str,
    ) -> Result<Value, ApiError> {
        if expected < 1 {
            return Err(ApiError::bad_request("expected_revision must be positive"));
        }
        let revision: i64 =
            sqlx::query_scalar("SELECT revision FROM users WHERE id=$1 FOR NO KEY UPDATE")
                .bind(user)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| ApiError::not_found("user"))?;
        if revision != expected {
            return Err(ApiError::conflict(format!(
                "user revision is {revision}; reload before rotating subscription"
            )));
        }
        sqlx::query("SELECT id FROM subscription_credentials WHERE user_id=$1 AND revoked_at IS NULL ORDER BY id FOR UPDATE")
            .bind(user).fetch_all(&mut *tx).await?;
        let timestamp = now();
        sqlx::query("UPDATE subscription_credentials SET revoked_at=$1 WHERE user_id=$2 AND revoked_at IS NULL")
            .bind(timestamp).bind(user).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO subscription_credentials(id,user_id,token_hash,token_enc,created_at) VALUES($1,$2,$3,$4,$5)")
            .bind(&self.id).bind(user).bind(&self.hash).bind(&self.ciphertext).bind(timestamp).execute(&mut *tx).await?;
        let updated =
            sqlx::query("UPDATE users SET revision=$1,updated_at=$2 WHERE id=$3 AND revision=$4")
                .bind(revision + 1)
                .bind(timestamp)
                .bind(user)
                .bind(revision)
                .execute(&mut *tx)
                .await?;
        if updated.rows_affected() != 1 {
            return Err(ApiError::conflict(
                "user changed while rotating subscription",
            ));
        }
        crate::api::audit_on(
            tx,
            actor,
            "user.subscription_rotated",
            "user",
            user,
            json!({"revision":revision+1}),
        )
        .await?;
        Ok(
            json!({"user_id":user,"revision":revision+1,"token":self.plaintext,"url":super::auto_subscription_url(&self.plaintext),"note":"The old subscription URL has been revoked."}),
        )
    }
}

pub(crate) async fn rotate_v2(
    State(state): State<AppState>,
    Extension(actor): Extension<AdminActor>,
    Path(id): Path<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = RequestIdentity::from_headers(
        &state.secrets,
        &actor.id,
        "users.subscription_rotate",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: super::RotateSubscription = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid subscription rotation request"))?;
    let prepared = Prepared::new(&state.secrets)?;
    execute(&state, identity, move |tx| {
        let (prepared, id, actor) = (prepared.clone(), id.clone(), actor.id.clone());
        Box::pin(async move {
            Ok(StoredResponse::json(
                StatusCode::OK,
                prepared
                    .commit_on(tx, &id, input.expected_revision, &actor)
                    .await?,
            ))
        })
    })
    .await?
    .into_http()
}
