use super::{lock_user_graph_on, now};
use crate::{error::ApiError, state::AppState};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};

pub(super) async fn prepare(
    state: &AppState,
    _user: &str,
) -> Result<crate::connection_credentials::Credential, ApiError> {
    Ok(crate::connection_credentials::Credential::new(
        &state.secrets,
    )?)
}
pub(super) async fn commit_on(
    tx: &mut Transaction<'static, Postgres>,
    user_id: String,
    expected: i64,
    prepared: crate::connection_credentials::Credential,
    actor: String,
) -> Result<Value, ApiError> {
    if expected < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let user = lock_user_graph_on(tx, &user_id, false).await?;
    let revision: i64 = user.get("revision");
    if revision != expected {
        return Err(ApiError::conflict("user changed; reload before rotating"));
    }
    sqlx::query("UPDATE users SET connection_credential_hash=$2,connection_credential_enc=$3,revision=revision+1,updated_at=$4 WHERE id=$1")
        .bind(&user_id).bind(&prepared.hash).bind(&prepared.ciphertext).bind(now()).execute(&mut **tx).await?;
    let pairs: Vec<(String, String)> = sqlx::query_as(
        "SELECT user_id,node_id FROM node_assignments WHERE user_id=$1 ORDER BY node_id",
    )
    .bind(&user_id)
    .fetch_all(&mut **tx)
    .await?;
    let jobs = crate::kick_requests::enqueue_pairs(tx, &pairs, "credentials_revoked").await?;
    super::super::audit_on(
        tx,
        &actor,
        "user.credentials_rotated",
        "user",
        &user_id,
        json!({"revision":revision+1,"nodes":pairs.len()}),
    )
    .await?;
    Ok(
        json!({"user_id":user_id,"revision":revision+1,"credential":prepared.plaintext,"revocation_job_ids":jobs}),
    )
}
