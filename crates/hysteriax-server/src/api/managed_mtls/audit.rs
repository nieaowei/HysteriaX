//! Failed request auditing is separate from the rolled-back operation. A lost
//! commit acknowledgement is not treated as proof that nothing was applied.
use std::future::Future;

use serde_json::Value;
use sqlx::{Postgres, Transaction};

use super::AdminActor;
use crate::{db, error::ApiError, state::AppState};

pub(super) fn redactions(certificate: Option<&str>, private_key: Option<&str>) -> Vec<String> {
    let mut values = Vec::new();
    for material in [certificate, private_key].into_iter().flatten() {
        if !material.is_empty() {
            values.push(material.to_owned());
            values.extend(
                material
                    .lines()
                    .map(str::trim)
                    .filter(|line| line.len() >= 12)
                    .map(str::to_owned),
            );
        }
    }
    values
}

pub(super) async fn request<T, F>(
    state: &AppState,
    actor: &AdminActor,
    action: &'static str,
    target: (&str, &str),
    mut detail: Value,
    redactions: Vec<String>,
    operation: F,
) -> Result<T, ApiError>
where
    T: Send,
    F: Future<Output = Result<T, ApiError>> + Send,
{
    let mut result = operation.await;
    if let Err(error) = &mut result {
        let mut reason = error.message.clone();
        for value in redactions {
            reason = reason.replace(&value, "[redacted]");
        }
        error.message = reason.chars().take(1024).collect();
        detail["reason"] = error.message.clone().into();
        detail["http_status"] = error.status.as_u16().into();
        let persisted = async {
            let mut tx = db::begin_write(&state.pool).await?;
            record(&mut tx, actor, action, target, detail).await?;
            tx.commit().await
        }
        .await;
        if persisted.is_err() {
            tracing::error!(operation=action,actor_id=%actor.id,"failed to persist managed CA request failure audit");
        }
    }
    result
}

pub(super) async fn record(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AdminActor,
    action: &str,
    target: (&str, &str),
    detail: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,$2,$3,$4,$5,$6,now())")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&actor.id).bind(action).bind(target.0).bind(target.1).bind(detail).execute(&mut **tx).await?;
    Ok(())
}
