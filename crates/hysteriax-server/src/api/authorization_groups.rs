use std::collections::{BTreeSet, HashSet};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    api::{generate_token, now},
    error::ApiError,
    security::token_digest,
    state::AppState,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct GroupProposal {
    name: String,
    user_ids: Vec<String>,
    node_ids: Vec<String>,
}

#[cfg(test)]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixtureGroupCreate {
    name: String,
    user_ids: Vec<String>,
    node_ids: Vec<String>,
}

#[cfg(test)]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FixtureGroupReplace {
    expected_revision: i64,
    name: String,
    user_ids: Vec<String>,
    node_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteGroupRequest {
    expected_revision: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct MembershipProposal {
    group_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MembershipUpdateRequest {
    expected_revision: i64,
    group_ids: Vec<String>,
}

#[derive(Clone, Debug)]
struct GroupInfo {
    id: String,
    name: String,
    revision: i64,
    user_ids: Vec<String>,
    node_ids: Vec<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

type Pair = (String, String);

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    crate::read_snapshot::scope(&state.pool, list_snapshot(State(state.clone()))).await
}
async fn list_snapshot(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    let version =
        crate::cache_domains::version(&state.pool, &["groups", "user_config", "node_config"])
            .await?;
    let value = state
        .caches
        .values
        .get(
            format!("group-directory:{version}"),
            std::time::Duration::from_secs(60),
            crate::cache::bypass(),
            || async { Ok::<_, ApiError>(json!(read_directory(&state).await?)) },
        )
        .await?;
    Ok(Json(
        value.as_array().ok_or_else(ApiError::internal)?.clone(),
    ))
}

async fn read_directory(state: &AppState) -> Result<Vec<Value>, ApiError> {
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let rows=sqlx::query("SELECT id,name,revision,created_at,updated_at FROM authorization_groups ORDER BY lower(name) COLLATE \"C\",name COLLATE \"C\",id").fetch_all(&mut *tx).await?;
    let users =
        sqlx::query("SELECT group_id,user_id FROM authorization_group_users ORDER BY user_id")
            .fetch_all(&mut *tx)
            .await?;
    let nodes =
        sqlx::query("SELECT group_id,node_id FROM authorization_group_nodes ORDER BY node_id")
            .fetch_all(&mut *tx)
            .await?;
    let mut user_map = std::collections::BTreeMap::<String, Vec<String>>::new();
    let mut node_map = std::collections::BTreeMap::<String, Vec<String>>::new();
    for row in users {
        user_map
            .entry(row.get("group_id"))
            .or_default()
            .push(row.get("user_id"));
    }
    for row in nodes {
        node_map
            .entry(row.get("group_id"))
            .or_default()
            .push(row.get("node_id"));
    }
    let groups = rows
        .iter()
        .map(|row| {
            let id: String = row.get("id");
            let user_ids = user_map.remove(&id).unwrap_or_default();
            let node_ids = node_map.remove(&id).unwrap_or_default();
            group_json(&GroupInfo {
                id,
                name: row.get("name"),
                revision: row.get("revision"),
                user_ids,
                node_ids,
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            })
        })
        .collect();
    tx.commit().await?;
    Ok(groups)
}

pub async fn get(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let group = load_group(&state.pool, &group_id)
        .await?
        .ok_or_else(|| ApiError::not_found("authorization group"))?;
    Ok(Json(group_json(&group)))
}

#[cfg(test)]
fn normalized_proposal(mut proposal: GroupProposal) -> Result<GroupProposal, ApiError> {
    proposal.name = validate_name(&proposal.name)?.to_owned();
    normalize_ids(&mut proposal.user_ids, "user_ids")?;
    normalize_ids(&mut proposal.node_ids, "node_ids")?;
    Ok(proposal)
}

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

async fn load_group(pool: &sqlx::PgPool, group_id: &str) -> Result<Option<GroupInfo>, sqlx::Error> {
    let mut tx = crate::read_snapshot::begin(pool).await?;
    let group = load_group_tx(&mut tx, group_id).await?;
    tx.commit().await?;
    Ok(group)
}

async fn load_group_tx(
    tx: &mut sqlx::PgConnection,
    group_id: &str,
) -> Result<Option<GroupInfo>, sqlx::Error> {
    let Some(row) = sqlx::query(
        "SELECT g.id,g.name,g.revision,g.created_at,g.updated_at,ARRAY(SELECT user_id FROM authorization_group_users WHERE group_id=g.id ORDER BY user_id) AS user_ids,ARRAY(SELECT node_id FROM authorization_group_nodes WHERE group_id=g.id ORDER BY node_id) AS node_ids FROM authorization_groups g WHERE g.id=$1",
    )
    .bind(group_id)
    .fetch_optional(&mut *tx)
    .await?
    else {
        return Ok(None);
    };
    Ok(Some(GroupInfo {
        id: row.get("id"),
        name: row.get("name"),
        revision: row.get("revision"),
        user_ids: row.get("user_ids"),
        node_ids: row.get("node_ids"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }))
}

fn group_json(group: &GroupInfo) -> Value {
    json!({
        "id":group.id,
        "name":group.name,
        "revision":group.revision,
        "user_ids":group.user_ids,
        "node_ids":group.node_ids,
        "user_count":group.user_ids.len(),
        "node_count":group.node_ids.len(),
        "created_at":group.created_at,
        "updated_at":group.updated_at,
    })
}

async fn user_revision(tx: &mut Transaction<'_, Postgres>, user_id: &str) -> Result<i64, ApiError> {
    sqlx::query_scalar("SELECT revision FROM users WHERE id=$1")
        .bind(user_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("user"))
}

async fn validate_proposal_entities(
    tx: &mut Transaction<'_, Postgres>,
    proposal: &GroupProposal,
) -> Result<(), ApiError> {
    validate_ids_exist(tx, "users", &proposal.user_ids, "user").await?;
    validate_ids_exist(tx, "nodes", &proposal.node_ids, "node").await?;
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

async fn group_ids_for_users(
    tx: &mut Transaction<'_, Postgres>,
    users: &[String],
) -> Result<Vec<String>, sqlx::Error> {
    if users.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar(
        "SELECT DISTINCT group_id FROM authorization_group_users WHERE user_id=ANY($1::text[]) ORDER BY group_id",
    )
    .bind(users)
    .fetch_all(&mut **tx)
    .await
}

async fn effective_pairs(
    tx: &mut Transaction<'_, Postgres>,
    users: &[String],
    excluded_group: Option<&str>,
) -> Result<HashSet<Pair>, sqlx::Error> {
    if users.is_empty() {
        return Ok(HashSet::new());
    }
    let rows = sqlx::query("SELECT gu.user_id,gn.node_id FROM authorization_group_users gu JOIN authorization_group_nodes gn ON gn.group_id=gu.group_id WHERE gu.user_id=ANY($1::text[]) AND ($2::text IS NULL OR gu.group_id<>$2)")
        .bind(users)
        .bind(excluded_group)
        .fetch_all(&mut **tx)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.get("user_id"), row.get("node_id")))
        .collect())
}

async fn pairs_for_user_groups(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    groups: &[String],
) -> Result<HashSet<Pair>, sqlx::Error> {
    if groups.is_empty() {
        return Ok(HashSet::new());
    }
    let nodes: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT node_id FROM authorization_group_nodes WHERE group_id=ANY($1::text[]) ORDER BY node_id",
    )
    .bind(groups)
    .fetch_all(&mut **tx)
    .await?;
    Ok(nodes
        .into_iter()
        .map(|node| (user_id.to_owned(), node))
        .collect())
}

fn add_group_pairs(pairs: &mut HashSet<Pair>, users: &[String], nodes: &[String]) {
    for user in users {
        for node in nodes {
            pairs.insert((user.clone(), node.clone()));
        }
    }
}

fn pair_diff(before: &HashSet<Pair>, after: &HashSet<Pair>) -> (Vec<Pair>, Vec<Pair>) {
    let mut additions: Vec<_> = after.difference(before).cloned().collect();
    let mut removals: Vec<_> = before.difference(after).cloned().collect();
    additions.sort();
    removals.sort();
    (additions, removals)
}

fn authorization_transaction_error(error: anyhow::Error) -> ApiError {
    if crate::db::RetryableTransactionError::retryable_transaction(&error) {
        ApiError::from(error)
    } else {
        ApiError::conflict(error.to_string())
    }
}

/// Lock only the authorization graph touched by this request, in the same order
/// as node/user deletion. New relationships are checked again by SERIALIZABLE.
async fn lock_authorization_graph_on(
    tx: &mut Transaction<'_, Postgres>,
    users: &[String],
    groups: &[String],
    nodes: &[String],
    prepared: &crate::credentials::authorization::Prepared,
) -> Result<(), ApiError> {
    let mut group_ids = groups.to_vec();
    group_ids.extend(group_ids_for_users(tx, users).await?);
    sort_dedup(&mut group_ids);
    sqlx::query("SELECT id FROM authorization_groups WHERE id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(&group_ids)
        .fetch_all(&mut **tx)
        .await?;
    let mut node_ids = nodes.to_vec();
    node_ids.extend(sqlx::query_scalar::<_,String>("SELECT node_id FROM authorization_group_nodes WHERE group_id=ANY($1) UNION SELECT node_id FROM node_assignments WHERE user_id=ANY($2)").bind(&group_ids).bind(users).fetch_all(&mut **tx).await?);
    sort_dedup(&mut node_ids);
    // Authorization changes do not update node configuration. Prevent deletion
    // without serializing against monitoring's non-key node updates. Changes to
    // trust and authorization inputs are still rechecked under SERIALIZABLE.
    sqlx::query("SELECT id FROM nodes WHERE id=ANY($1) ORDER BY id FOR KEY SHARE")
        .bind(&node_ids)
        .fetch_all(&mut **tx)
        .await?;
    if users.is_empty() && prepared.plan.identities.is_empty() {
        return Ok(());
    }
    sqlx::query("SELECT id FROM users WHERE id=ANY($1) ORDER BY id FOR NO KEY UPDATE")
        .bind(users)
        .fetch_all(&mut **tx)
        .await?;
    let cas: Vec<String> = prepared
        .plan
        .identities
        .iter()
        .map(|identity| identity.ca_credential_id.clone())
        .collect();
    sqlx::query("SELECT id FROM credentials WHERE owner_user_id=ANY($1) OR id=ANY($2) ORDER BY id FOR UPDATE").bind(users).bind(&cas).fetch_all(&mut **tx).await?;
    sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=ANY($1) ORDER BY credential_id FOR UPDATE").bind(&cas).fetch_all(&mut **tx).await?;
    sqlx::query("SELECT id FROM mtls_user_identities WHERE user_id=ANY($1) ORDER BY id FOR UPDATE")
        .bind(users)
        .fetch_all(&mut **tx)
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn commit_group_on(
    tx: &mut Transaction<'_, Postgres>,
    state: &AppState,
    action: &str,
    group_id: Option<&str>,
    expected_revision: Option<i64>,
    proposal: GroupProposal,
    prepared: crate::credentials::authorization::Prepared,
    creation_id: &str,
    assignments: PreparedAssignments,
) -> Result<Value, ApiError> {
    let mut users = proposal.user_ids.clone();
    if let Some(group) = group_id {
        users.extend(
            sqlx::query_scalar::<_, String>(
                "SELECT user_id FROM authorization_group_users WHERE group_id=$1",
            )
            .bind(group)
            .fetch_all(&mut **tx)
            .await?,
        );
    }
    lock_authorization_graph_on(
        tx,
        &users,
        &group_id.map(str::to_owned).into_iter().collect::<Vec<_>>(),
        &proposal.node_ids,
        &prepared,
    )
    .await?;
    let current = if let Some(group_id) = group_id {
        Some(
            load_group_tx(tx, group_id)
                .await?
                .ok_or_else(|| ApiError::not_found("authorization group"))?,
        )
    } else {
        None
    };
    if let Some(group) = &current {
        check_revision(group.revision, expected_revision)?;
    } else if expected_revision.is_some() {
        return Err(ApiError::bad_request(
            "expected_revision is not valid when creating a group",
        ));
    }

    if action != "delete" {
        validate_proposal_entities(tx, &proposal).await?;
    }
    let mut affected_users = current
        .as_ref()
        .map(|group| group.user_ids.clone())
        .unwrap_or_default();
    if action != "delete" {
        affected_users.extend(proposal.user_ids.clone());
    }
    normalize_ids(&mut affected_users, "user_ids")?;
    let old_pairs = effective_pairs(tx, &affected_users, None).await?;
    let mut planned_pairs = if action == "create" {
        old_pairs.clone()
    } else {
        effective_pairs(tx, &affected_users, group_id).await?
    };
    if action != "delete" {
        add_group_pairs(&mut planned_pairs, &proposal.user_ids, &proposal.node_ids);
    }
    let (planned_additions, planned_removals) = pair_diff(&old_pairs, &planned_pairs);
    let timestamp = now();
    let target_group_id = if action == "create" {
        Some(creation_id.to_owned())
    } else {
        group_id.map(str::to_owned)
    };
    let group_changed = action == "create"
        || action == "delete"
        || current.as_ref().is_some_and(|old| {
            old.name != proposal.name
                || (action == "update"
                    && (old.user_ids != proposal.user_ids || old.node_ids != proposal.node_ids))
        });

    match action {
        "create" => {
            let id = target_group_id.as_deref().expect("create group ID");
            sqlx::query("INSERT INTO authorization_groups(id,name,revision,created_at,updated_at) VALUES($1,$2,1,$3,$3)")
                .bind(id)
                .bind(&proposal.name)
                .bind(timestamp)
                .execute(&mut **tx)
                .await?;
            write_group_relations(
                tx,
                id,
                &[],
                &proposal.user_ids,
                &[],
                &proposal.node_ids,
                timestamp,
            )
            .await?;
        }
        "update" => {
            let id = target_group_id.as_deref().expect("update group ID");
            let current = current.as_ref().expect("update group exists");
            if group_changed {
                write_group_relations(
                    tx,
                    id,
                    &current.user_ids,
                    &proposal.user_ids,
                    &current.node_ids,
                    &proposal.node_ids,
                    timestamp,
                )
                .await?;
                sqlx::query("UPDATE authorization_groups SET name=$1,revision=revision+1,updated_at=$2 WHERE id=$3 AND revision=$4")
                    .bind(&proposal.name)
                    .bind(timestamp)
                    .bind(id)
                    .bind(current.revision)
                    .execute(&mut **tx)
                    .await?;
            }
        }
        "delete" => {
            let id = target_group_id.as_deref().expect("delete group ID");
            sqlx::query("DELETE FROM authorization_groups WHERE id=$1 AND revision=$2")
                .bind(id)
                .bind(current.as_ref().expect("delete group exists").revision)
                .execute(&mut **tx)
                .await?;
        }
        _ => {
            return Err(ApiError::bad_request(
                "unsupported authorization group action",
            ));
        }
    }

    let effective_after = effective_pairs(tx, &affected_users, None).await?;
    let (actual_additions, actual_removals) = pair_diff(&old_pairs, &effective_after);
    if actual_additions != planned_additions || actual_removals != planned_removals {
        return Err(ApiError::conflict(
            "authorization sources changed while saving; retry the change",
        ));
    }
    let mut managed_identities = crate::credentials::authorization::commit_prepared(
        tx,
        &state.secrets,
        prepared,
        &actual_additions,
    )
    .await
    .map_err(authorization_transaction_error)?;
    let (_, revocation_job_ids, touched_users) =
        reconcile_assignments(tx, &affected_users, &effective_after, &assignments, false).await?;
    crate::credentials::authorization::annotate_results_on(tx, &mut managed_identities).await?;
    let mut revision_users: BTreeSet<String> = if group_changed {
        affected_users.iter().cloned().collect()
    } else {
        BTreeSet::new()
    };
    revision_users.extend(touched_users);
    bump_user_revisions(tx, &revision_users, timestamp).await?;
    if action != "delete" {
        audit_group(
            tx,
            if action == "create" {
                "authorization_group.created"
            } else {
                "authorization_group.updated"
            },
            target_group_id.as_deref().expect("group ID"),
            json!({
                    "action":action,
                    "user_ids":proposal.user_ids,
                    "node_ids":proposal.node_ids,
                    "additions_count":actual_additions.len(),
                    "removals_count":actual_removals.len(),
            "managed_identities":managed_identities,
                }),
            timestamp,
        )
        .await?;
    } else {
        audit_group(
            tx,
            "authorization_group.deleted",
            group_id.expect("delete group ID"),
            json!({
                    "revision":expected_revision,
                    "additions_count":actual_additions.len(),
                    "removals_count":actual_removals.len(),
            "managed_identities":managed_identities,
                }),
            timestamp,
        )
        .await?;
    }
    let response = json!({"revocation_job_ids":revocation_job_ids});
    Ok(response)
}

async fn write_group_relations(
    tx: &mut Transaction<'_, Postgres>,
    group_id: &str,
    old_users: &[String],
    new_users: &[String],
    old_nodes: &[String],
    new_nodes: &[String],
    timestamp: DateTime<Utc>,
) -> Result<(), ApiError> {
    let old_users: BTreeSet<_> = old_users.iter().cloned().collect();
    let new_users: BTreeSet<_> = new_users.iter().cloned().collect();
    let removed_users: Vec<_> = old_users.difference(&new_users).cloned().collect();
    let added_users: Vec<_> = new_users.difference(&old_users).cloned().collect();
    if !removed_users.is_empty() {
        sqlx::query("DELETE FROM authorization_group_users WHERE group_id=$1 AND user_id=ANY($2)")
            .bind(group_id)
            .bind(&removed_users)
            .execute(&mut **tx)
            .await?;
    }
    if !added_users.is_empty() {
        sqlx::query("INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT $1,user_id,$3 FROM unnest($2::text[]) AS u(user_id) ORDER BY user_id")
            .bind(group_id).bind(&added_users).bind(timestamp).execute(&mut **tx).await?;
    }
    let old_nodes: BTreeSet<_> = old_nodes.iter().cloned().collect();
    let new_nodes: BTreeSet<_> = new_nodes.iter().cloned().collect();
    let removed_nodes: Vec<_> = old_nodes.difference(&new_nodes).cloned().collect();
    let added_nodes: Vec<_> = new_nodes.difference(&old_nodes).cloned().collect();
    if !removed_nodes.is_empty() {
        sqlx::query("DELETE FROM authorization_group_nodes WHERE group_id=$1 AND node_id=ANY($2)")
            .bind(group_id)
            .bind(&removed_nodes)
            .execute(&mut **tx)
            .await?;
    }
    if !added_nodes.is_empty() {
        sqlx::query("INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT $1,node_id,$3 FROM unnest($2::text[]) AS n(node_id) ORDER BY node_id")
            .bind(group_id).bind(&added_nodes).bind(timestamp).execute(&mut **tx).await?;
    }
    Ok(())
}

async fn reconcile_assignments(
    tx: &mut Transaction<'_, Postgres>,
    affected_users: &[String],
    effective_after: &HashSet<Pair>,
    prepared: &PreparedAssignments,
    include_created_credentials: bool,
) -> Result<(Vec<Value>, Vec<String>, BTreeSet<String>), ApiError> {
    let rows = if affected_users.is_empty() {
        Vec::new()
    } else {
        sqlx::query("SELECT user_id,node_id FROM node_assignments WHERE user_id=ANY($1::text[]) ORDER BY user_id,node_id")
            .bind(affected_users)
            .fetch_all(&mut **tx)
            .await?
    };
    let materialized: HashSet<Pair> = rows
        .into_iter()
        .map(|row| (row.get("user_id"), row.get("node_id")))
        .collect();
    let (mut additions, mut removals) = pair_diff(&materialized, effective_after);
    additions.sort();
    removals.sort();
    let changed_pairs: Vec<Pair> = additions.iter().chain(removals.iter()).cloned().collect();
    let mut created = Vec::new();
    let mut touched = BTreeSet::new();
    if !additions.is_empty() {
        let mut grant_users = Vec::with_capacity(additions.len());
        let mut grant_nodes = Vec::with_capacity(additions.len());
        let mut digests = Vec::with_capacity(additions.len());
        let mut ciphertexts = Vec::with_capacity(additions.len());
        for (user_id, node_id) in &additions {
            let credential = prepared.get(user_id, node_id)?;
            grant_users.push(user_id.clone());
            grant_nodes.push(node_id.clone());
            digests.push(credential.digest.clone());
            ciphertexts.push(credential.ciphertext.clone());
            if include_created_credentials {
                created.push(
                    json!({"user_id":user_id,"node_id":node_id,"hy2_credential":credential.plaintext}),
                );
            }
            touched.insert(user_id.clone());
        }
        sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) SELECT user_id,node_id,digest,ciphertext,$5 FROM unnest($1::text[],$2::text[],$3::text[],$4::text[]) AS a(user_id,node_id,digest,ciphertext) ORDER BY user_id,node_id")
            .bind(&grant_users).bind(&grant_nodes).bind(&digests).bind(&ciphertexts).bind(prepared.timestamp).execute(&mut **tx).await?;
        crate::kick_requests::clear_reason_for_pairs(tx, &additions, "authorization_group_removed")
            .await?;
    }
    let jobs = if removals.is_empty() {
        Vec::new()
    } else {
        let users: Vec<_> = removals.iter().map(|(u, _)| u.clone()).collect();
        let nodes: Vec<_> = removals.iter().map(|(_, n)| n.clone()).collect();
        sqlx::query("DELETE FROM node_assignments a USING unnest($1::text[],$2::text[]) r(user_id,node_id) WHERE a.user_id=r.user_id AND a.node_id=r.node_id")
            .bind(&users).bind(&nodes).execute(&mut **tx).await?;
        touched.extend(users);
        crate::kick_requests::enqueue_pairs(tx, &removals, "authorization_group_removed").await?
    };
    crate::credentials::authorization::reconcile_on(tx, affected_users, &changed_pairs, None)
        .await?;
    Ok((created, jobs, touched))
}

async fn bump_user_revisions(
    tx: &mut Transaction<'_, Postgres>,
    users: &BTreeSet<String>,
    timestamp: DateTime<Utc>,
) -> Result<(), ApiError> {
    if !users.is_empty() {
        let users: Vec<_> = users.iter().cloned().collect();
        sqlx::query("UPDATE users SET revision=revision+1,updated_at=$2 WHERE id=ANY($1)")
            .bind(&users)
            .bind(timestamp)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

async fn audit_group(
    tx: &mut Transaction<'_, Postgres>,
    action: &str,
    group_id: &str,
    detail: Value,
    timestamp: DateTime<Utc>,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,$2,$3,'authorization_group',$4,$5,$6)")
        .bind(Uuid::new_v4().to_string())
        .bind(super::audit_actor())
        .bind(action)
        .bind(group_id)
        .bind(detail)
        .bind(timestamp)
        .execute(&mut **tx)
        .await?;
    Ok(())
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

#[allow(clippy::too_many_arguments)]
async fn commit_membership_on(
    tx: &mut Transaction<'_, Postgres>,
    state: &AppState,
    user_id: &str,
    expected_revision: i64,
    proposal: MembershipProposal,
    prepared: crate::credentials::authorization::Prepared,
    assignments: PreparedAssignments,
) -> Result<Value, ApiError> {
    lock_authorization_graph_on(
        tx,
        &[user_id.to_owned()],
        &proposal.group_ids,
        &[],
        &prepared,
    )
    .await?;
    let current_revision = user_revision(tx, user_id).await?;
    check_revision(current_revision, Some(expected_revision))?;
    validate_group_ids(tx, &proposal.group_ids).await?;
    let current_group_ids: Vec<String> = sqlx::query_scalar(
        "SELECT group_id FROM authorization_group_users WHERE user_id=$1 ORDER BY group_id",
    )
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await?;
    let current_set: BTreeSet<_> = current_group_ids.iter().cloned().collect();
    let target_set: BTreeSet<_> = proposal.group_ids.iter().cloned().collect();
    let old_pairs = effective_pairs(tx, std::slice::from_ref(&user_id.to_owned()), None).await?;
    let planned_pairs = pairs_for_user_groups(tx, user_id, &proposal.group_ids).await?;
    let (planned_additions, planned_removals) = pair_diff(&old_pairs, &planned_pairs);
    let timestamp = now();
    let removed_groups: Vec<_> = current_set.difference(&target_set).cloned().collect();
    let added_groups: Vec<_> = target_set.difference(&current_set).cloned().collect();
    if !removed_groups.is_empty() {
        sqlx::query("DELETE FROM authorization_group_users WHERE group_id=ANY($1) AND user_id=$2")
            .bind(&removed_groups)
            .bind(user_id)
            .execute(&mut **tx)
            .await?;
    }
    if !added_groups.is_empty() {
        sqlx::query("INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT group_id,$2,$3 FROM unnest($1::text[]) AS g(group_id) ORDER BY group_id")
            .bind(&added_groups).bind(user_id).bind(timestamp).execute(&mut **tx).await?;
    }
    let changed_groups: Vec<_> = removed_groups.into_iter().chain(added_groups).collect();
    if !changed_groups.is_empty() {
        sqlx::query(
            "UPDATE authorization_groups SET revision=revision+1,updated_at=$2 WHERE id=ANY($1)",
        )
        .bind(changed_groups)
        .bind(timestamp)
        .execute(&mut **tx)
        .await?;
    }
    let effective_after = effective_pairs(tx, &[user_id.to_owned()], None).await?;
    let (actual_additions, actual_removals) = pair_diff(&old_pairs, &effective_after);
    if actual_additions != planned_additions || actual_removals != planned_removals {
        return Err(ApiError::conflict(
            "authorization sources changed while saving; retry the change",
        ));
    }
    let mut managed_identities = crate::credentials::authorization::commit_prepared(
        tx,
        &state.secrets,
        prepared,
        &actual_additions,
    )
    .await
    .map_err(authorization_transaction_error)?;
    let (created_credentials, revocation_job_ids, touched_users) = reconcile_assignments(
        tx,
        &[user_id.to_owned()],
        &effective_after,
        &assignments,
        true,
    )
    .await?;
    crate::credentials::authorization::annotate_results_on(tx, &mut managed_identities).await?;
    if current_set != target_set || !touched_users.is_empty() {
        bump_user_revisions(tx, &BTreeSet::from([user_id.to_owned()]), timestamp).await?;
    }
    audit_group(
        tx,
        "authorization_group.user_membership_updated",
        user_id,
        json!({
            "previous_group_ids":current_group_ids,
            "group_ids":proposal.group_ids,
            "additions_count":actual_additions.len(),
            "removals_count":actual_removals.len(),
        "managed_identities":managed_identities,
        }),
        timestamp,
    )
    .await?;
    let revision = if current_set != target_set || !touched_users.is_empty() {
        current_revision + 1
    } else {
        current_revision
    };
    let assignments = assignments_json(tx, user_id).await?;
    let response = json!({
        "user_id":user_id,
        "additions_count":actual_additions.len(),
        "removals_count":actual_removals.len(),
        "revision":revision,
        "group_ids":proposal.group_ids,
        "assignments":assignments,
        "created_credentials":created_credentials,
        "managed_identities":managed_identities,
        "revocation_job_ids":revocation_job_ids,
    });
    Ok(response)
}

async fn assignments_json(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT a.node_id,a.created_at,coalesce((SELECT jsonb_agg(jsonb_build_object('id',g.id,'name',g.name) ORDER BY lower(g.name) COLLATE \"C\",g.name COLLATE \"C\",g.id) FROM authorization_group_users gu JOIN authorization_group_nodes gn ON gn.group_id=gu.group_id JOIN authorization_groups g ON g.id=gu.group_id WHERE gu.user_id=a.user_id AND gn.node_id=a.node_id),'[]'::jsonb) AS source_groups FROM node_assignments a WHERE a.user_id=$1 ORDER BY a.node_id",
    )
    .bind(user_id)
    .fetch_all(&mut **tx)
    .await?;
    let mut assignments = Vec::with_capacity(rows.len());
    let mut identities =
        crate::credentials::authorization::assignment_refs_on(tx, &[user_id.to_owned()]).await?;
    for row in rows {
        let node_id: String = row.get("node_id");
        let source_groups: Value = row.get("source_groups");
        assignments.push(json!({
            "managed_identity_ids":identities.remove(&(user_id.to_owned(),node_id.clone())).unwrap_or_default(),
            "node_id":node_id,
            "created_at":row.get::<DateTime<Utc>,_>("created_at"),


            "source_groups":source_groups,
        }));
    }
    Ok(assignments)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) async fn fixture() -> AppState {
        let state = crate::kick_requests::tests::fixture().await;
        sqlx::query("UPDATE nodes SET desired_config_enc=$1 WHERE id='node'")
            .bind(state.secrets.encrypt("{}").unwrap())
            .execute(&state.pool)
            .await
            .unwrap();
        state
    }

    pub(super) async fn create_group(state: &AppState, name: &str) -> Value {
        let request = json!({"name":name,"user_ids":["user"],"node_ids":["node"]});
        let (_, Json(result)) = create(
            State(state.clone()),
            Json(serde_json::from_value(request).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(result, json!({"revocation_job_ids":[]}));
        group_named(state, name).await
    }

    #[tokio::test]
    async fn overlapping_sources_preserve_credentials_and_regrant_clears_pending_removal() {
        let state = fixture().await;
        let first = create_group(&state, "First").await;
        let hash: String = sqlx::query_scalar("SELECT credential_hash FROM node_assignments")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let second = create_group(&state, "Second").await;
        for (index, group) in [&first, &second].into_iter().enumerate() {
            let id = group["id"].as_str().unwrap();
            let Json(receipt) = delete(
                State(state.clone()),
                Path(id.into()),
                Json(DeleteGroupRequest {
                    expected_revision: 1,
                }),
            )
            .await
            .unwrap();
            assert_eq!(receipt.as_object().unwrap().len(), 1);
            assert_eq!(
                receipt["revocation_job_ids"].as_array().unwrap().len(),
                index
            );
            if index == 0 {
                let current: String =
                    sqlx::query_scalar("SELECT credential_hash FROM node_assignments")
                        .fetch_one(&state.pool)
                        .await
                        .unwrap();
                assert_eq!(current, hash);
            }
        }
        create_group(&state, "Regrant").await;
        let stale: i64 = sqlx::query_scalar("SELECT count(*) FROM kick_requests WHERE state NOT IN ('completed','cancelled') AND reasons ? 'authorization_group_removed'").fetch_one(&state.pool).await.unwrap();
        assert_eq!(stale, 0);
    }

    #[tokio::test]
    async fn bulk_revocations_create_one_job_per_node() {
        let state = fixture().await;
        let users: Vec<String> = (0..1000).map(|i| format!("bulk-{i:05}")).collect();
        sqlx::query("INSERT INTO users(id,name,created_at,updated_at) SELECT u,u,now(),now() FROM unnest($1::text[]) u")
            .bind(&users).execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) SELECT u,'node',u,'unused',now() FROM unnest($1::text[]) u")
            .bind(&users).execute(&state.pool).await.unwrap();
        let proposal = GroupProposal {
            name: "Group".into(),
            user_ids: users.clone(),
            node_ids: vec![],
        };
        let prepared = prepare_group_assignments(&state, "update", None, &proposal)
            .await
            .unwrap();
        let mut tx = state.pool.begin().await.unwrap();
        let (created, jobs, touched) =
            reconcile_assignments(&mut tx, &users, &HashSet::new(), &prepared, false)
                .await
                .unwrap();
        assert!(created.is_empty());
        assert_eq!(jobs.len(), 1);
        assert_eq!(touched.len(), 1000);
        tx.commit().await.unwrap();
        let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM node_assignments),(SELECT count(*) FROM jobs WHERE kind='kick'),(SELECT count(*) FROM kick_requests WHERE state='active')")
            .fetch_one(&state.pool).await.unwrap();
        assert_eq!(counts, (0, 1, 1000));
    }

    #[tokio::test]
    async fn member_and_node_changes_save_without_previews_and_report_actual_effects() {
        let state = fixture().await;
        let created = create_group(&state, "Group").await;
        let id = created["id"].as_str().unwrap();
        for (revision, users, nodes, additions, removals) in [
            (1, vec!["user"], vec![], 0, 1),
            (2, vec!["user"], vec!["node"], 1, 0),
            (3, vec![], vec!["node"], 0, 1),
            (4, vec!["user"], vec!["node"], 1, 0),
        ] {
            let Json(receipt) = update(
                State(state.clone()),
                Path(id.into()),
                Json(FixtureGroupReplace {
                    expected_revision: revision,
                    name: "Group".into(),
                    user_ids: users.into_iter().map(str::to_owned).collect(),
                    node_ids: nodes.into_iter().map(str::to_owned).collect(),
                }),
            )
            .await
            .unwrap();
            assert_eq!(receipt.as_object().unwrap().len(), 1);
            assert_eq!(group_named(&state, "Group").await["revision"], revision + 1);
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM node_assignments")
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(count, additions);
            if removals > 0 {
                assert!(!receipt["revocation_job_ids"].as_array().unwrap().is_empty());
            }
        }
        assert!(
            update(
                State(state.clone()),
                Path(id.into()),
                Json(FixtureGroupReplace {
                    expected_revision: 1,
                    name: "Stale".into(),
                    user_ids: vec![],
                    node_ids: vec![],
                })
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn authorization_requests_reject_obsolete_preview_tokens() {
        let mut create = json!({"name":"Direct","user_ids":[],"node_ids":[]});
        assert!(serde_json::from_value::<FixtureGroupCreate>(create.clone()).is_ok());
        create["preview_token"] = json!("obsolete");
        assert!(serde_json::from_value::<FixtureGroupCreate>(create).is_err());
        let mut update = json!({"expected_revision":1,"name":"Direct","user_ids":[],"node_ids":[]});
        assert!(serde_json::from_value::<FixtureGroupReplace>(update.clone()).is_ok());
        update["preview_token"] = json!("obsolete");
        assert!(serde_json::from_value::<FixtureGroupReplace>(update).is_err());
        let mut delete = json!({"expected_revision":1});
        assert!(serde_json::from_value::<DeleteGroupRequest>(delete.clone()).is_ok());
        delete["preview_token"] = json!("obsolete");
        assert!(serde_json::from_value::<DeleteGroupRequest>(delete).is_err());
        let mut membership = json!({"expected_revision":1,"group_ids":[]});
        assert!(serde_json::from_value::<MembershipUpdateRequest>(membership.clone()).is_ok());
        membership["preview_token"] = json!("obsolete");
        assert!(serde_json::from_value::<MembershipUpdateRequest>(membership).is_err());
    }

    #[test]
    fn migration_sized_groups_are_not_subject_to_an_arbitrary_member_cap() {
        let proposal = GroupProposal {
            name: "Large".into(),
            user_ids: (0..601).map(|n| format!("user-{n}")).collect(),
            node_ids: vec![],
        };
        assert_eq!(normalized_proposal(proposal).unwrap().user_ids.len(), 601);
    }
}

#[cfg(test)]
mod transaction_tests;

pub(super) mod v2;

mod assignment_preparation;
use assignment_preparation::*;

// Direct domain fixtures are test-only; production writes use protocol-2 handlers.
#[cfg(test)]
#[path = "authorization_groups_test_support.rs"]
mod test_support;
#[cfg(test)]
pub(crate) use test_support::*;

#[cfg(test)]
use crate::db;
