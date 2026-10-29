use crate::{error::ApiError, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use std::collections::HashSet;
use uuid::Uuid;
type Pair = (String, String);
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteGroupRequest {
    expected_revision: i64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MembershipProposal {
    group_ids: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MembershipUpdateRequest {
    expected_revision: i64,
    group_ids: Vec<String>,
}
#[cfg(test)]
#[derive(Deserialize)]
pub(crate) struct FixtureGroupCreate {
    name: String,
    user_ids: Vec<String>,
    node_ids: Vec<String>,
}
pub(crate) mod changes;
mod reads;
pub(crate) use reads::{get, list, list_nodes, list_users};
#[cfg(test)]
#[path = "authorization_groups_test_support.rs"]
mod test_support;
pub(crate) mod v2;
#[cfg(test)]
pub(crate) use test_support::{create, delete, group_named};
fn normalized_membership(mut proposal: MembershipProposal) -> Result<MembershipProposal, ApiError> {
    normalize_ids(&mut proposal.group_ids, "group_ids")?;
    Ok(proposal)
}

fn validate_name(value: &str) -> Result<&str, ApiError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 180 || value.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "name must contain 1–180 printable characters",
        ));
    }
    Ok(value)
}

fn normalize_ids(values: &mut Vec<String>, field: &str) -> Result<(), ApiError> {
    if values
        .iter()
        .any(|value| value.trim().is_empty() || value.chars().any(char::is_control))
    {
        return Err(ApiError::bad_request(format!(
            "{field} contains an invalid ID"
        )));
    }
    sort_dedup(values);
    Ok(())
}

fn sort_dedup(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn check_revision(current: i64, expected: Option<i64>) -> Result<(), ApiError> {
    if expected != Some(current) {
        return Err(ApiError::conflict(format!(
            "revision is {current}; reload before editing"
        )));
    }
    Ok(())
}

async fn validate_ids_exist(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    ids: &[String],
    label: &str,
) -> Result<(), ApiError> {
    if ids.is_empty() {
        return Ok(());
    }
    let query = format!("SELECT id FROM {table} WHERE id=ANY($1::text[])");
    let existing: HashSet<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(query.as_str()))
        .bind(ids)
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .collect();
    if let Some(missing) = ids.iter().find(|id| !existing.contains(*id)) {
        return Err(ApiError::not_found(&format!("{label} {missing}")));
    }
    Ok(())
}

async fn validate_group_ids(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[String],
) -> Result<(), ApiError> {
    validate_ids_exist(tx, "authorization_groups", ids, "authorization group").await
}

pub(crate) async fn before_node_delete(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    timestamp: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    crate::credentials::authorization::reconcile_node(tx, node_id, true)
        .await
        .map_err(|error| {
            error
                .downcast::<sqlx::Error>()
                .unwrap_or_else(|error| sqlx::Error::Protocol(error.to_string()))
        })?;
    sqlx::query("UPDATE users SET revision=revision+1,updated_at=$2 WHERE id IN (SELECT user_id FROM node_assignments WHERE node_id=$1 UNION SELECT gu.user_id FROM authorization_group_users gu JOIN authorization_group_nodes gn ON gn.group_id=gu.group_id WHERE gn.node_id=$1)")
        .bind(node_id)
        .bind(timestamp)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE authorization_groups SET revision=revision+1,updated_at=$2 WHERE id IN (SELECT group_id FROM authorization_group_nodes WHERE node_id=$1)")
        .bind(node_id)
        .bind(timestamp)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
