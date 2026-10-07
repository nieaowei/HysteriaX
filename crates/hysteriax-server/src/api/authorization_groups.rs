use std::collections::{BTreeMap, BTreeSet, HashSet};

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{
    api::{enqueue_job_with_payload_in_tx, generate_token, now},
    db,
    error::ApiError,
    security::token_digest,
    state::AppState,
};

const PREVIEW_TTL_MINUTES: i64 = 10;
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
struct MTLSBinding {
    user_id: String,
    node_id: String,
    credential_id: String,
    credential_version: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct GroupProposal {
    name: String,
    user_ids: Vec<String>,
    node_ids: Vec<String>,
    #[serde(default)]
    mtls_bindings: Vec<MTLSBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupPreviewRequest {
    action: String,
    expected_revision: Option<i64>,
    name: Option<String>,
    user_ids: Option<Vec<String>>,
    node_ids: Option<Vec<String>>,
    #[serde(default)]
    mtls_bindings: Vec<MTLSBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateGroupRequest {
    name: String,
    user_ids: Vec<String>,
    node_ids: Vec<String>,
    preview_token: String,
    #[serde(default)]
    mtls_bindings: Vec<MTLSBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateGroupRequest {
    expected_revision: i64,
    name: String,
    user_ids: Vec<String>,
    node_ids: Vec<String>,
    preview_token: String,
    #[serde(default)]
    mtls_bindings: Vec<MTLSBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteGroupRequest {
    expected_revision: i64,
    preview_token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct MembershipProposal {
    group_ids: Vec<String>,
    #[serde(default)]
    mtls_bindings: Vec<MTLSBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MembershipPreviewRequest {
    expected_revision: i64,
    group_ids: Vec<String>,
    #[serde(default)]
    mtls_bindings: Vec<MTLSBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct MembershipUpdateRequest {
    expected_revision: i64,
    group_ids: Vec<String>,
    preview_token: String,
    #[serde(default)]
    mtls_bindings: Vec<MTLSBinding>,
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
    let rows = sqlx::query(
        "SELECT id FROM authorization_groups ORDER BY lower(name) COLLATE \"C\", name COLLATE \"C\", id",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut groups = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.get("id");
        let group = load_group(&state.pool, &id)
            .await?
            .ok_or_else(|| ApiError::not_found("authorization group"))?;
        groups.push(group_json(&group));
    }
    Ok(Json(groups))
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

pub(crate) async fn preview_create(
    State(state): State<AppState>,
    Json(input): Json<GroupPreviewRequest>,
) -> Result<Json<Value>, ApiError> {
    if input.action != "create" {
        return Err(ApiError::bad_request(
            "create preview requires action=create",
        ));
    }
    if input.expected_revision.is_some() {
        return Err(ApiError::bad_request(
            "expected_revision is not valid when creating a group",
        ));
    }
    let proposal = proposal_from_preview(input)?;
    let mut tx = db::begin_write(&state.pool).await?;
    validate_proposal_entities(&mut tx, &proposal).await?;
    let affected = proposal.user_ids.clone();
    let old_pairs = effective_pairs(&mut tx, &affected, None).await?;
    let mut new_pairs = old_pairs.clone();
    add_group_pairs(&mut new_pairs, &proposal.user_ids, &proposal.node_ids);
    let (missing_mtls, guard, result, request_json) = preview_data(
        &state, &mut tx, "create", None, None, &affected, &proposal, &old_pairs, &new_pairs,
    )
    .await?;
    let token = store_preview(&mut tx, "create", None, None, request_json, guard, &result).await?;
    tx.commit().await?;
    Ok(Json(preview_json("create", token, result, missing_mtls)))
}

pub(crate) async fn preview_group(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
    Json(input): Json<GroupPreviewRequest>,
) -> Result<Json<Value>, ApiError> {
    if !matches!(input.action.as_str(), "update" | "delete") {
        return Err(ApiError::bad_request(
            "group preview action must be update or delete",
        ));
    }
    let action = input.action.clone();
    let expected_revision = input.expected_revision;
    if expected_revision.is_none() {
        return Err(ApiError::bad_request("expected_revision is required"));
    }
    let proposal = if action == "update" {
        Some(proposal_from_preview(input.clone())?)
    } else {
        if input.name.is_some()
            || input.user_ids.is_some()
            || input.node_ids.is_some()
            || !input.mtls_bindings.is_empty()
        {
            return Err(ApiError::bad_request(
                "delete preview accepts only action and expected_revision",
            ));
        }
        None
    };
    let mut tx = db::begin_write(&state.pool).await?;
    let current = load_group_tx(&mut tx, &group_id)
        .await?
        .ok_or_else(|| ApiError::not_found("authorization group"))?;
    check_revision(current.revision, expected_revision)?;
    if let Some(proposal) = &proposal {
        validate_proposal_entities(&mut tx, proposal).await?;
    }
    let mut affected = current.user_ids.clone();
    if let Some(proposal) = &proposal {
        affected.extend(proposal.user_ids.clone());
    }
    sort_dedup(&mut affected);
    let old_pairs = effective_pairs(&mut tx, &affected, None).await?;
    let mut new_pairs = effective_pairs(&mut tx, &affected, Some(&group_id)).await?;
    if let Some(proposal) = &proposal {
        add_group_pairs(&mut new_pairs, &proposal.user_ids, &proposal.node_ids);
    }
    let proposal_for_snapshot = proposal.unwrap_or(GroupProposal {
        name: current.name.clone(),
        user_ids: Vec::new(),
        node_ids: Vec::new(),
        mtls_bindings: Vec::new(),
    });
    let (missing_mtls, guard, result, request_json) = preview_data(
        &state,
        &mut tx,
        &action,
        Some(&group_id),
        Some(expected_revision.unwrap_or_default()),
        &affected,
        &proposal_for_snapshot,
        &old_pairs,
        &new_pairs,
    )
    .await?;
    let token = store_preview(
        &mut tx,
        &action,
        Some(&group_id),
        None,
        request_json,
        guard,
        &result,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(preview_json(&action, token, result, missing_mtls)))
}

pub(crate) async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateGroupRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let proposal = normalized_proposal(GroupProposal {
        name: input.name,
        user_ids: input.user_ids,
        node_ids: input.node_ids,
        mtls_bindings: input.mtls_bindings,
    })?;
    let response =
        commit_group_mutation(&state, "create", None, None, &input.preview_token, proposal).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

pub(crate) async fn update(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
    Json(input): Json<UpdateGroupRequest>,
) -> Result<Json<Value>, ApiError> {
    let proposal = normalized_proposal(GroupProposal {
        name: input.name,
        user_ids: input.user_ids,
        node_ids: input.node_ids,
        mtls_bindings: input.mtls_bindings,
    })?;
    let response = commit_group_mutation(
        &state,
        "update",
        Some(&group_id),
        Some(input.expected_revision),
        &input.preview_token,
        proposal,
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn delete(
    State(state): State<AppState>,
    Path(group_id): Path<String>,
    Json(input): Json<DeleteGroupRequest>,
) -> Result<Json<Value>, ApiError> {
    let proposal = GroupProposal {
        name: String::new(),
        user_ids: Vec::new(),
        node_ids: Vec::new(),
        mtls_bindings: Vec::new(),
    };
    let response = commit_group_mutation(
        &state,
        "delete",
        Some(&group_id),
        Some(input.expected_revision),
        &input.preview_token,
        proposal,
    )
    .await?;
    Ok(Json(response))
}

pub(crate) async fn preview_memberships(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<MembershipPreviewRequest>,
) -> Result<Json<Value>, ApiError> {
    if input.expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let proposal = normalized_membership(MembershipProposal {
        group_ids: input.group_ids,
        mtls_bindings: input.mtls_bindings,
    })?;
    let mut tx = db::begin_write(&state.pool).await?;
    let revision = user_revision(&mut tx, &user_id).await?;
    check_revision(revision, Some(input.expected_revision))?;
    validate_group_ids(&mut tx, &proposal.group_ids).await?;
    let old_pairs = effective_pairs(&mut tx, std::slice::from_ref(&user_id), None).await?;
    let new_pairs = pairs_for_user_groups(&mut tx, &user_id, &proposal.group_ids).await?;
    let guard = guard_snapshot(
        &mut tx,
        std::slice::from_ref(&user_id),
        None,
        &proposal.group_ids,
        &[],
        &proposal.mtls_bindings,
    )
    .await?;
    let (additions, removals) = pair_diff(&old_pairs, &new_pairs);
    let missing_mtls =
        missing_mtls_bindings(&state, &mut tx, &additions, &proposal.mtls_bindings).await?;
    validate_mtls_bindings(&mut tx, &additions, &proposal.mtls_bindings).await?;
    let request_json = json!({
        "expected_revision": input.expected_revision,
        "group_ids": proposal.group_ids,
        "mtls_bindings": proposal.mtls_bindings,
    });
    let result = diff_json(&additions, &removals);
    let token = store_preview(
        &mut tx,
        "update_memberships",
        None,
        Some(&user_id),
        request_json,
        guard,
        &result,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(preview_json(
        "update_memberships",
        token,
        result,
        missing_mtls,
    )))
}

pub(crate) async fn update_memberships(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(input): Json<MembershipUpdateRequest>,
) -> Result<Json<Value>, ApiError> {
    let proposal = normalized_membership(MembershipProposal {
        group_ids: input.group_ids,
        mtls_bindings: input.mtls_bindings,
    })?;
    let response = commit_membership(
        &state,
        &user_id,
        input.expected_revision,
        &input.preview_token,
        proposal,
    )
    .await?;
    Ok(Json(response))
}

fn proposal_from_preview(input: GroupPreviewRequest) -> Result<GroupProposal, ApiError> {
    normalized_proposal(GroupProposal {
        name: input
            .name
            .ok_or_else(|| ApiError::bad_request("name is required"))?,
        user_ids: input
            .user_ids
            .ok_or_else(|| ApiError::bad_request("user_ids is required"))?,
        node_ids: input
            .node_ids
            .ok_or_else(|| ApiError::bad_request("node_ids is required"))?,
        mtls_bindings: input.mtls_bindings,
    })
}

fn normalized_proposal(mut proposal: GroupProposal) -> Result<GroupProposal, ApiError> {
    proposal.name = validate_name(&proposal.name)?.to_owned();
    normalize_ids(&mut proposal.user_ids, "user_ids")?;
    normalize_ids(&mut proposal.node_ids, "node_ids")?;
    normalize_bindings(&mut proposal.mtls_bindings)?;
    Ok(proposal)
}

fn normalized_membership(mut proposal: MembershipProposal) -> Result<MembershipProposal, ApiError> {
    normalize_ids(&mut proposal.group_ids, "group_ids")?;
    normalize_bindings(&mut proposal.mtls_bindings)?;
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

fn normalize_bindings(values: &mut Vec<MTLSBinding>) -> Result<(), ApiError> {
    values.sort_by(|left, right| {
        (&left.user_id, &left.node_id).cmp(&(&right.user_id, &right.node_id))
    });
    for pair in values.windows(2) {
        if pair[0].user_id == pair[1].user_id && pair[0].node_id == pair[1].node_id {
            return Err(ApiError::bad_request(
                "mtls_bindings contains duplicate user/node pairs",
            ));
        }
    }
    if values.iter().any(|binding| {
        binding.user_id.trim().is_empty()
            || binding.node_id.trim().is_empty()
            || binding.credential_id.trim().is_empty()
            || binding.credential_version < 1
    }) {
        return Err(ApiError::bad_request(
            "mtls_bindings contains an invalid entry",
        ));
    }
    Ok(())
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
    let mut tx = pool.begin().await?;
    let group = load_group_tx(&mut tx, group_id).await?;
    tx.commit().await?;
    Ok(group)
}

async fn load_group_tx(
    tx: &mut Transaction<'_, Postgres>,
    group_id: &str,
) -> Result<Option<GroupInfo>, sqlx::Error> {
    let Some(row) = sqlx::query(
        "SELECT id,name,revision,created_at,updated_at FROM authorization_groups WHERE id=$1",
    )
    .bind(group_id)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(None);
    };
    let user_ids = sqlx::query_scalar::<_, String>(
        "SELECT user_id FROM authorization_group_users WHERE group_id=$1 ORDER BY user_id",
    )
    .bind(group_id)
    .fetch_all(&mut **tx)
    .await?;
    let node_ids = sqlx::query_scalar::<_, String>(
        "SELECT node_id FROM authorization_group_nodes WHERE group_id=$1 ORDER BY node_id",
    )
    .bind(group_id)
    .fetch_all(&mut **tx)
    .await?;
    Ok(Some(GroupInfo {
        id: row.get("id"),
        name: row.get("name"),
        revision: row.get("revision"),
        user_ids,
        node_ids,
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
    let existing: HashSet<String> = sqlx::query_scalar(&query)
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

async fn load_groups_by_ids(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[String],
) -> Result<Vec<GroupInfo>, ApiError> {
    let mut groups = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(group) = load_group_tx(tx, id).await? {
            groups.push(group);
        }
    }
    Ok(groups)
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

fn pairs_json(pairs: &[Pair]) -> Vec<Value> {
    pairs
        .iter()
        .map(|(user_id, node_id)| json!({"user_id":user_id,"node_id":node_id}))
        .collect()
}

fn diff_json(additions: &[Pair], removals: &[Pair]) -> Value {
    json!({
        "additions_count": additions.len(),
        "removals_count": removals.len(),
        "additions": pairs_json(additions),
        "removals": pairs_json(removals),
    })
}

fn preview_json(action: &str, token: String, result: Value, missing: Vec<Pair>) -> Value {
    json!({
        "preview_token":token,
        "action":action,
        "additions_count":result["additions_count"],
        "removals_count":result["removals_count"],
        "additions":result["additions"],
        "removals":result["removals"],
        "missing_mtls":pairs_json(&missing),
    })
}

async fn preview_data(
    state: &AppState,
    tx: &mut Transaction<'_, Postgres>,
    action: &str,
    group_id: Option<&str>,
    _expected_revision: Option<i64>,
    affected_users: &[String],
    proposal: &GroupProposal,
    old_pairs: &HashSet<Pair>,
    new_pairs: &HashSet<Pair>,
) -> Result<(Vec<Pair>, Value, Value, Value), ApiError> {
    let (additions, removals) = pair_diff(old_pairs, new_pairs);
    let bindings = if action == "delete" {
        Vec::new()
    } else {
        proposal.mtls_bindings.clone()
    };
    validate_mtls_bindings(tx, &additions, &bindings).await?;
    let missing = missing_mtls_bindings(state, tx, &additions, &bindings).await?;
    let guard = guard_snapshot(
        tx,
        affected_users,
        group_id,
        &[],
        if action == "delete" {
            &[]
        } else {
            &proposal.node_ids
        },
        &bindings,
    )
    .await?;
    let result = diff_json(&additions, &removals);
    let request_json = json!({
        "action":action,
        "expected_revision":_expected_revision,
        "name":if action == "delete" { Value::Null } else { json!(proposal.name) },
        "user_ids":if action == "delete" { json!([]) } else { json!(proposal.user_ids) },
        "node_ids":if action == "delete" { json!([]) } else { json!(proposal.node_ids) },
        "mtls_bindings":bindings,
    });
    Ok((missing, guard, result, request_json))
}

async fn guard_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    affected_users: &[String],
    target_group: Option<&str>,
    proposed_groups: &[String],
    proposed_nodes: &[String],
    bindings: &[MTLSBinding],
) -> Result<Value, ApiError> {
    let mut group_ids = group_ids_for_users(tx, affected_users).await?;
    group_ids.extend(proposed_groups.iter().cloned());
    if let Some(group_id) = target_group {
        group_ids.push(group_id.to_owned());
    }
    group_ids.sort();
    group_ids.dedup();
    let groups = load_groups_by_ids(tx, &group_ids).await?;

    let mut user_ids = affected_users.to_vec();
    for group in &groups {
        user_ids.extend(group.user_ids.iter().cloned());
    }
    user_ids.sort();
    user_ids.dedup();
    let user_rows = if user_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query("SELECT id,revision FROM users WHERE id=ANY($1::text[]) ORDER BY id")
            .bind(&user_ids)
            .fetch_all(&mut **tx)
            .await?
    };
    let users: Vec<Value> = user_rows
        .iter()
        .map(|row| json!({"id":row.get::<String,_>("id"),"revision":row.get::<i64,_>("revision")}))
        .collect();

    let mut node_ids = proposed_nodes.to_vec();
    for group in &groups {
        node_ids.extend(group.node_ids.iter().cloned());
    }
    let assignment_rows = if user_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query("SELECT user_id,node_id,created_at,mtls_credential_id,mtls_credential_version FROM node_assignments WHERE user_id=ANY($1::text[]) ORDER BY user_id,node_id")
            .bind(&user_ids)
            .fetch_all(&mut **tx)
            .await?
    };
    let mut assignments = Vec::with_capacity(assignment_rows.len());
    let mut credential_ids = Vec::new();
    for row in assignment_rows {
        let mtls_id: Option<String> = row.get("mtls_credential_id");
        if let Some(id) = &mtls_id {
            credential_ids.push(id.clone());
        }
        let node_id: String = row.get("node_id");
        node_ids.push(node_id.clone());
        assignments.push(json!({
            "user_id":row.get::<String,_>("user_id"),
            "node_id":node_id,
            "created_at":row.get::<DateTime<Utc>,_>("created_at"),
            "mtls_credential_id":mtls_id,
            "mtls_credential_version":row.get::<Option<i64>,_>("mtls_credential_version"),
        }));
    }
    for binding in bindings {
        credential_ids.push(binding.credential_id.clone());
        node_ids.push(binding.node_id.clone());
    }
    node_ids.sort();
    node_ids.dedup();
    let nodes = if node_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query("SELECT id,desired_revision,md5(desired_config_enc) AS config_digest FROM nodes WHERE id=ANY($1::text[]) ORDER BY id")
            .bind(&node_ids)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|row| json!({
                "id":row.get::<String,_>("id"),
                "desired_revision":row.get::<i64,_>("desired_revision"),
                "config_digest":row.get::<String,_>("config_digest"),
            }))
            .collect::<Vec<_>>()
    };
    credential_ids.sort();
    credential_ids.dedup();
    let credentials = if credential_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query("SELECT id,kind,owner_user_id,revision,latest_version,archived FROM credentials WHERE id=ANY($1::text[]) ORDER BY id")
            .bind(&credential_ids)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|row| json!({
                "id":row.get::<String,_>("id"),
                "kind":row.get::<String,_>("kind"),
                "owner_user_id":row.get::<Option<String>,_>("owner_user_id"),
                "revision":row.get::<i64,_>("revision"),
                "latest_version":row.get::<i64,_>("latest_version"),
                "archived":row.get::<bool,_>("archived"),
            }))
            .collect::<Vec<_>>()
    };
    Ok(json!({
        "users":users,
        "groups":groups.iter().map(group_json).collect::<Vec<_>>(),
        "nodes":nodes,
        "assignments":assignments,
        "credentials":credentials,
    }))
}

async fn missing_mtls_bindings(
    state: &AppState,
    tx: &mut Transaction<'_, Postgres>,
    additions: &[Pair],
    bindings: &[MTLSBinding],
) -> Result<Vec<Pair>, ApiError> {
    let selected: HashSet<Pair> = bindings
        .iter()
        .map(|binding| (binding.user_id.clone(), binding.node_id.clone()))
        .collect();
    let mut missing = Vec::new();
    for pair @ (_, node_id) in additions {
        if !selected.contains(pair) && node_requires_mtls(state, tx, node_id).await? {
            missing.push(pair.clone());
        }
    }
    Ok(missing)
}

async fn node_requires_mtls(
    state: &AppState,
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
) -> Result<bool, ApiError> {
    let config_enc: Option<String> =
        sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id=$1")
            .bind(node_id)
            .fetch_optional(&mut **tx)
            .await?;
    let config_enc = config_enc.ok_or_else(|| ApiError::not_found(&format!("node {node_id}")))?;
    let config_json = state.secrets.decrypt(&config_enc)?;
    let config: Value = serde_json::from_str(&config_json).map_err(|_| ApiError::internal())?;
    Ok(config
        .get("tls")
        .and_then(Value::as_object)
        .and_then(|tls| tls.get("clientCA"))
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty()))
}

async fn validate_mtls_bindings(
    tx: &mut Transaction<'_, Postgres>,
    additions: &[Pair],
    bindings: &[MTLSBinding],
) -> Result<(), ApiError> {
    let additions: HashSet<Pair> = additions.iter().cloned().collect();
    for binding in bindings {
        if !additions.contains(&(binding.user_id.clone(), binding.node_id.clone())) {
            return Err(ApiError::bad_request(
                "mtls_bindings must refer only to newly authorized user/node pairs",
            ));
        }
        let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials c JOIN credential_versions v ON v.credential_id=c.id WHERE c.id=$1 AND v.version=$2 AND c.kind='tls_identity' AND c.owner_user_id=$3 AND c.archived=FALSE)")
            .bind(&binding.credential_id)
            .bind(binding.credential_version)
            .bind(&binding.user_id)
            .fetch_one(&mut **tx)
            .await?;
        if !valid {
            return Err(ApiError::bad_request(
                "invalid, archived, or incorrectly owned mTLS credential",
            ));
        }
    }
    Ok(())
}

async fn store_preview(
    tx: &mut Transaction<'_, Postgres>,
    action: &str,
    group_id: Option<&str>,
    user_id: Option<&str>,
    request_json: Value,
    guard_json: Value,
    result_json: &Value,
) -> Result<String, ApiError> {
    let token = Uuid::new_v4().to_string();
    let timestamp = now();
    sqlx::query("DELETE FROM authorization_group_previews WHERE expires_at <= $1")
        .bind(timestamp)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO authorization_group_previews(token,action,group_id,user_id,request_json,guard_json,result_json,created_at,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(&token)
        .bind(action)
        .bind(group_id)
        .bind(user_id)
        .bind(request_json)
        .bind(guard_json)
        .bind(result_json)
        .bind(timestamp)
        .bind(timestamp + Duration::minutes(PREVIEW_TTL_MINUTES))
        .execute(&mut **tx)
        .await?;
    Ok(token)
}

#[derive(Debug)]
struct PreviewRow {
    action: String,
    group_id: Option<String>,
    user_id: Option<String>,
    request_json: Value,
    guard_json: Value,
    result_json: Value,
    expires_at: DateTime<Utc>,
    consumed_at: Option<DateTime<Utc>>,
}

async fn load_preview(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
) -> Result<PreviewRow, ApiError> {
    let row = sqlx::query("SELECT action,group_id,user_id,request_json,guard_json,result_json,expires_at,consumed_at FROM authorization_group_previews WHERE token=$1 FOR UPDATE")
        .bind(token)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::conflict("preview token is unknown; preview the change again"))?;
    let preview = PreviewRow {
        action: row.get("action"),
        group_id: row.get("group_id"),
        user_id: row.get("user_id"),
        request_json: row.get("request_json"),
        guard_json: row.get("guard_json"),
        result_json: row.get("result_json"),
        expires_at: row.get("expires_at"),
        consumed_at: row.get("consumed_at"),
    };
    if preview.consumed_at.is_some() || preview.expires_at <= now() {
        return Err(ApiError::conflict(
            "preview token has expired or was already used; preview the change again",
        ));
    }
    Ok(preview)
}

fn ensure_preview_matches(
    preview: &PreviewRow,
    action: &str,
    group_id: Option<&str>,
    user_id: Option<&str>,
    request: &Value,
    guard: &Value,
) -> Result<(), ApiError> {
    if preview.action != action
        || preview.group_id.as_deref() != group_id
        || preview.user_id.as_deref() != user_id
        || preview.request_json != *request
    {
        return Err(ApiError::conflict(
            "preview does not match this change; preview the change again",
        ));
    }
    if preview.guard_json != *guard {
        return Err(ApiError::conflict(
            "related users, groups, nodes, or credentials changed after preview; refresh the preview",
        ));
    }
    Ok(())
}

async fn mark_preview_used(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE authorization_group_previews SET consumed_at=$2 WHERE token=$1 AND consumed_at IS NULL")
        .bind(token)
        .bind(now())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn commit_group_mutation(
    state: &AppState,
    action: &str,
    group_id: Option<&str>,
    expected_revision: Option<i64>,
    preview_token: &str,
    proposal: GroupProposal,
) -> Result<Value, ApiError> {
    let mut tx = db::begin_write(&state.pool).await?;
    let preview = load_preview(&mut tx, preview_token).await?;
    if preview.action != action
        || preview.group_id.as_deref() != group_id
        || preview.user_id.is_some()
    {
        return Err(ApiError::conflict(
            "preview does not match this change; preview the change again",
        ));
    }
    let current = if let Some(group_id) = group_id {
        Some(
            load_group_tx(&mut tx, group_id)
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
        validate_proposal_entities(&mut tx, &proposal).await?;
    }
    let mut affected_users = current
        .as_ref()
        .map(|group| group.user_ids.clone())
        .unwrap_or_default();
    if action != "delete" {
        affected_users.extend(proposal.user_ids.clone());
    }
    normalize_ids(&mut affected_users, "user_ids")?;
    let old_pairs = effective_pairs(&mut tx, &affected_users, None).await?;
    let mut planned_pairs = if action == "create" {
        old_pairs.clone()
    } else {
        effective_pairs(&mut tx, &affected_users, group_id).await?
    };
    if action != "delete" {
        add_group_pairs(&mut planned_pairs, &proposal.user_ids, &proposal.node_ids);
    }
    let (planned_additions, planned_removals) = pair_diff(&old_pairs, &planned_pairs);
    let bindings = if action == "delete" {
        Vec::new()
    } else {
        proposal.mtls_bindings.clone()
    };
    validate_mtls_bindings(&mut tx, &planned_additions, &bindings).await?;
    let missing = missing_mtls_bindings(state, &mut tx, &planned_additions, &bindings).await?;
    if !missing.is_empty() {
        return Err(mtls_required_error(&missing));
    }
    let guard = guard_snapshot(
        &mut tx,
        &affected_users,
        group_id,
        &[],
        if action == "delete" {
            &[]
        } else {
            &proposal.node_ids
        },
        &bindings,
    )
    .await?;
    let request_json = group_request_json(action, expected_revision, &proposal);
    ensure_preview_matches(&preview, action, group_id, None, &request_json, &guard)?;

    let timestamp = now();
    let target_group_id = if action == "create" {
        Some(Uuid::new_v4().to_string())
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
                .execute(&mut *tx)
                .await?;
            write_group_relations(
                &mut tx,
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
                    &mut tx,
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
                    .execute(&mut *tx)
                    .await?;
            }
        }
        "delete" => {
            let id = target_group_id.as_deref().expect("delete group ID");
            mark_preview_used(&mut tx, preview_token).await?;
            sqlx::query("DELETE FROM authorization_groups WHERE id=$1 AND revision=$2")
                .bind(id)
                .bind(current.as_ref().expect("delete group exists").revision)
                .execute(&mut *tx)
                .await?;
        }
        _ => {
            return Err(ApiError::bad_request(
                "unsupported authorization group action",
            ));
        }
    }

    let effective_after = effective_pairs(&mut tx, &affected_users, None).await?;
    let (actual_additions, actual_removals) = pair_diff(&old_pairs, &effective_after);
    if actual_additions != planned_additions
        || actual_removals != planned_removals
        || preview.result_json != diff_json(&actual_additions, &actual_removals)
    {
        return Err(ApiError::conflict(
            "authorization sources changed while applying the preview; preview the change again",
        ));
    }
    let (created_credentials, revocation_job_ids, touched_users) =
        reconcile_assignments(state, &mut tx, &affected_users, &effective_after, &bindings).await?;
    let mut revision_users: BTreeSet<String> = if group_changed {
        affected_users.iter().cloned().collect()
    } else {
        BTreeSet::new()
    };
    revision_users.extend(touched_users);
    bump_user_revisions(&mut tx, &revision_users, timestamp).await?;
    if action != "delete" {
        audit_group(
            &mut tx,
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
            }),
            timestamp,
        )
        .await?;
    } else {
        audit_group(
            &mut tx,
            "authorization_group.deleted",
            group_id.expect("delete group ID"),
            json!({
                "revision":expected_revision,
                "additions_count":actual_additions.len(),
                "removals_count":actual_removals.len(),
            }),
            timestamp,
        )
        .await?;
    }
    if action != "delete" {
        mark_preview_used(&mut tx, preview_token).await?;
    }
    let group = if action == "delete" {
        Value::Null
    } else {
        let row = load_group_tx(&mut tx, target_group_id.as_deref().expect("group ID"))
            .await?
            .ok_or_else(|| ApiError::not_found("authorization group"))?;
        group_json(&row)
    };
    let response = json!({
        "group":group,
        "additions_count":actual_additions.len(),
        "removals_count":actual_removals.len(),
        "created_credentials":created_credentials,
        "revocation_job_ids":revocation_job_ids,
    });
    tx.commit().await?;
    Ok(response)
}

fn group_request_json(
    action: &str,
    expected_revision: Option<i64>,
    proposal: &GroupProposal,
) -> Value {
    json!({
        "action":action,
        "expected_revision":expected_revision,
        "name":if action == "delete" { Value::Null } else { json!(proposal.name) },
        "user_ids":if action == "delete" { json!([]) } else { json!(proposal.user_ids) },
        "node_ids":if action == "delete" { json!([]) } else { json!(proposal.node_ids) },
        "mtls_bindings":if action == "delete" { json!([]) } else { json!(proposal.mtls_bindings) },
    })
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
    let old_users: HashSet<_> = old_users.iter().cloned().collect();
    let new_users: HashSet<_> = new_users.iter().cloned().collect();
    for user in old_users.difference(&new_users) {
        sqlx::query("DELETE FROM authorization_group_users WHERE group_id=$1 AND user_id=$2")
            .bind(group_id)
            .bind(user)
            .execute(&mut **tx)
            .await?;
    }
    for user in new_users.difference(&old_users) {
        sqlx::query(
            "INSERT INTO authorization_group_users(group_id,user_id,created_at) VALUES($1,$2,$3)",
        )
        .bind(group_id)
        .bind(user)
        .bind(timestamp)
        .execute(&mut **tx)
        .await?;
    }
    let old_nodes: HashSet<_> = old_nodes.iter().cloned().collect();
    let new_nodes: HashSet<_> = new_nodes.iter().cloned().collect();
    for node in old_nodes.difference(&new_nodes) {
        sqlx::query("DELETE FROM authorization_group_nodes WHERE group_id=$1 AND node_id=$2")
            .bind(group_id)
            .bind(node)
            .execute(&mut **tx)
            .await?;
    }
    for node in new_nodes.difference(&old_nodes) {
        sqlx::query(
            "INSERT INTO authorization_group_nodes(group_id,node_id,created_at) VALUES($1,$2,$3)",
        )
        .bind(group_id)
        .bind(node)
        .bind(timestamp)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn reconcile_assignments(
    state: &AppState,
    tx: &mut Transaction<'_, Postgres>,
    affected_users: &[String],
    effective_after: &HashSet<Pair>,
    bindings: &[MTLSBinding],
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
    validate_mtls_bindings(tx, &additions, bindings).await?;
    let missing = missing_mtls_bindings(state, tx, &additions, bindings).await?;
    if !missing.is_empty() {
        return Err(mtls_required_error(&missing));
    }
    let binding_map: BTreeMap<Pair, &MTLSBinding> = bindings
        .iter()
        .map(|binding| ((binding.user_id.clone(), binding.node_id.clone()), binding))
        .collect();
    let mut created = Vec::new();
    let mut touched = BTreeSet::new();
    for (user_id, node_id) in additions {
        let credential = generate_token();
        let credential_hash = token_digest(&credential);
        let credential_enc = state.secrets.encrypt(&credential)?;
        let selected = binding_map.get(&(user_id.clone(), node_id.clone()));
        let mtls_id = selected.map(|binding| binding.credential_id.as_str());
        let mtls_version = selected.map(|binding| binding.credential_version);
        sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,mtls_credential_id,mtls_credential_version,created_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(&user_id)
            .bind(&node_id)
            .bind(credential_hash)
            .bind(credential_enc)
            .bind(mtls_id)
            .bind(mtls_version)
            .bind(now())
            .execute(&mut **tx)
            .await?;
        crate::kick_requests::clear_reason(
            tx,
            Some(&node_id),
            Some(&user_id),
            "authorization_group_removed",
        )
        .await?;
        created.push(json!({
            "user_id":user_id,
            "node_id":node_id,
            "hy2_credential":credential,
        }));
        touched.insert(user_id);
    }
    let mut jobs = BTreeSet::new();
    for (user_id, node_id) in removals {
        sqlx::query("DELETE FROM node_assignments WHERE user_id=$1 AND node_id=$2")
            .bind(&user_id)
            .bind(&node_id)
            .execute(&mut **tx)
            .await?;
        let job_id = enqueue_job_with_payload_in_tx(
            tx,
            "kick",
            Some(&node_id),
            None,
            json!({"user_id":user_id,"kick_reason":"authorization_group_removed"}),
        )
        .await?;
        jobs.insert(job_id);
        touched.insert(user_id);
    }
    Ok((created, jobs.into_iter().collect(), touched))
}

async fn bump_user_revisions(
    tx: &mut Transaction<'_, Postgres>,
    users: &BTreeSet<String>,
    timestamp: DateTime<Utc>,
) -> Result<(), ApiError> {
    for user_id in users {
        sqlx::query("UPDATE users SET revision=revision+1,updated_at=$2 WHERE id=$1")
            .bind(user_id)
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
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,'admin',$2,'authorization_group',$3,$4,$5)")
        .bind(Uuid::new_v4().to_string())
        .bind(action)
        .bind(group_id)
        .bind(detail)
        .bind(timestamp)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn mtls_required_error(missing: &[Pair]) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "mtls_binding_required",
        format!(
            "a valid user-owned mTLS credential is required for: {}",
            missing
                .iter()
                .map(|(user, node)| format!("{user}/{node}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

pub(crate) async fn before_node_delete(
    tx: &mut Transaction<'_, Postgres>,
    node_id: &str,
    timestamp: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
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

async fn commit_membership(
    state: &AppState,
    user_id: &str,
    expected_revision: i64,
    preview_token: &str,
    proposal: MembershipProposal,
) -> Result<Value, ApiError> {
    if expected_revision < 1 {
        return Err(ApiError::bad_request("expected_revision must be positive"));
    }
    let mut tx = db::begin_write(&state.pool).await?;
    let preview = load_preview(&mut tx, preview_token).await?;
    if preview.action != "update_memberships"
        || preview.group_id.is_some()
        || preview.user_id.as_deref() != Some(user_id)
    {
        return Err(ApiError::conflict(
            "preview does not match this change; preview the change again",
        ));
    }
    let current_revision = user_revision(&mut tx, user_id).await?;
    check_revision(current_revision, Some(expected_revision))?;
    validate_group_ids(&mut tx, &proposal.group_ids).await?;
    let current_group_ids: Vec<String> = sqlx::query_scalar(
        "SELECT group_id FROM authorization_group_users WHERE user_id=$1 ORDER BY group_id",
    )
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await?;
    let current_set: BTreeSet<_> = current_group_ids.iter().cloned().collect();
    let target_set: BTreeSet<_> = proposal.group_ids.iter().cloned().collect();
    let old_pairs =
        effective_pairs(&mut tx, std::slice::from_ref(&user_id.to_owned()), None).await?;
    let planned_pairs = pairs_for_user_groups(&mut tx, user_id, &proposal.group_ids).await?;
    let (planned_additions, planned_removals) = pair_diff(&old_pairs, &planned_pairs);
    validate_mtls_bindings(&mut tx, &planned_additions, &proposal.mtls_bindings).await?;
    let missing =
        missing_mtls_bindings(state, &mut tx, &planned_additions, &proposal.mtls_bindings).await?;
    if !missing.is_empty() {
        return Err(mtls_required_error(&missing));
    }
    let request_json = json!({
        "expected_revision":expected_revision,
        "group_ids":proposal.group_ids,
        "mtls_bindings":proposal.mtls_bindings,
    });
    let guard = guard_snapshot(
        &mut tx,
        &[user_id.to_owned()],
        None,
        &proposal.group_ids,
        &[],
        &proposal.mtls_bindings,
    )
    .await?;
    ensure_preview_matches(
        &preview,
        "update_memberships",
        None,
        Some(user_id),
        &request_json,
        &guard,
    )?;

    let timestamp = now();
    for group_id in current_set.difference(&target_set) {
        sqlx::query("DELETE FROM authorization_group_users WHERE group_id=$1 AND user_id=$2")
            .bind(group_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE authorization_groups SET revision=revision+1,updated_at=$2 WHERE id=$1",
        )
        .bind(group_id)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
    }
    for group_id in target_set.difference(&current_set) {
        sqlx::query(
            "INSERT INTO authorization_group_users(group_id,user_id,created_at) VALUES($1,$2,$3)",
        )
        .bind(group_id)
        .bind(user_id)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE authorization_groups SET revision=revision+1,updated_at=$2 WHERE id=$1",
        )
        .bind(group_id)
        .bind(timestamp)
        .execute(&mut *tx)
        .await?;
    }
    let effective_after = effective_pairs(&mut tx, &[user_id.to_owned()], None).await?;
    let (actual_additions, actual_removals) = pair_diff(&old_pairs, &effective_after);
    if actual_additions != planned_additions
        || actual_removals != planned_removals
        || preview.result_json != diff_json(&actual_additions, &actual_removals)
    {
        return Err(ApiError::conflict(
            "authorization sources changed while applying the preview; preview the change again",
        ));
    }
    let (created_credentials, revocation_job_ids, touched_users) = reconcile_assignments(
        state,
        &mut tx,
        &[user_id.to_owned()],
        &effective_after,
        &proposal.mtls_bindings,
    )
    .await?;
    if current_set != target_set || !touched_users.is_empty() {
        bump_user_revisions(&mut tx, &BTreeSet::from([user_id.to_owned()]), timestamp).await?;
    }
    audit_group(
        &mut tx,
        "authorization_group.user_membership_updated",
        user_id,
        json!({
            "previous_group_ids":current_group_ids,
            "group_ids":proposal.group_ids,
            "additions_count":actual_additions.len(),
            "removals_count":actual_removals.len(),
        }),
        timestamp,
    )
    .await?;
    mark_preview_used(&mut tx, preview_token).await?;
    let revision = if current_set != target_set || !touched_users.is_empty() {
        current_revision + 1
    } else {
        current_revision
    };
    let assignments = assignments_json(&mut tx, user_id).await?;
    let response = json!({
        "user_id":user_id,
        "revision":revision,
        "group_ids":proposal.group_ids,
        "assignments":assignments,
        "created_credentials":created_credentials,
        "revocation_job_ids":revocation_job_ids,
    });
    tx.commit().await?;
    Ok(response)
}

async fn assignments_json(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query("SELECT node_id,mtls_credential_id,mtls_credential_version,created_at FROM node_assignments WHERE user_id=$1 ORDER BY node_id")
        .bind(user_id)
        .fetch_all(&mut **tx)
        .await?;
    let mut assignments = Vec::with_capacity(rows.len());
    for row in rows {
        let node_id: String = row.get("node_id");
        let source_groups = sqlx::query_scalar::<_, Value>("SELECT COALESCE(jsonb_agg(jsonb_build_object('id',g.id,'name',g.name) ORDER BY lower(g.name) COLLATE \"C\",g.name COLLATE \"C\",g.id),'[]'::jsonb) FROM authorization_group_users gu JOIN authorization_group_nodes gn ON gn.group_id=gu.group_id JOIN authorization_groups g ON g.id=gu.group_id WHERE gu.user_id=$1 AND gn.node_id=$2")
            .bind(user_id)
            .bind(&node_id)
            .fetch_one(&mut **tx)
            .await?;
        assignments.push(json!({
            "node_id":node_id,
            "created_at":row.get::<DateTime<Utc>,_>("created_at"),
            "mtls_credential_id":row.get::<Option<String>,_>("mtls_credential_id"),
            "mtls_credential_version":row.get::<Option<i64>,_>("mtls_credential_version"),
            "source_groups":source_groups,
        }));
    }
    Ok(assignments)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture() -> AppState {
        let state = crate::kick_requests::tests::fixture().await;
        sqlx::query("UPDATE nodes SET desired_config_enc=$1 WHERE id='node'")
            .bind(state.secrets.encrypt("{}").unwrap())
            .execute(&state.pool)
            .await
            .unwrap();
        state
    }

    async fn create_group(state: &AppState, name: &str) -> Value {
        let draft = json!({"action":"create","name":name,"user_ids":["user"],"node_ids":["node"]});
        let Json(preview) = preview_create(
            State(state.clone()),
            Json(serde_json::from_value(draft.clone()).unwrap()),
        )
        .await
        .unwrap();
        let mut request = draft;
        request.as_object_mut().unwrap().remove("action");
        request["preview_token"] = preview["preview_token"].clone();
        let (_, Json(result)) = create(
            State(state.clone()),
            Json(serde_json::from_value(request).unwrap()),
        )
        .await
        .unwrap();
        result
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
        assert_eq!(second["additions_count"], 0);
        assert_eq!(second["created_credentials"], json!([]));
        for (index, group) in [&first["group"], &second["group"]].into_iter().enumerate() {
            let id = group["id"].as_str().unwrap();
            let Json(preview) = preview_group(
                State(state.clone()),
                Path(id.into()),
                Json(
                    serde_json::from_value(json!({"action":"delete","expected_revision":1}))
                        .unwrap(),
                ),
            )
            .await
            .unwrap();
            let Json(receipt) = delete(
                State(state.clone()),
                Path(id.into()),
                Json(DeleteGroupRequest {
                    expected_revision: 1,
                    preview_token: preview["preview_token"].as_str().unwrap().into(),
                }),
            )
            .await
            .unwrap();
            assert_eq!(receipt["removals_count"], index);
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
    async fn stale_and_replayed_previews_cannot_write() {
        let state = fixture().await;
        let draft =
            json!({"action":"create","name":"Previewed","user_ids":["user"],"node_ids":["node"]});
        let Json(preview) = preview_create(
            State(state.clone()),
            Json(serde_json::from_value(draft.clone()).unwrap()),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE authorization_group_previews SET expires_at=now()-interval '1 second' WHERE token=$1")
            .bind(preview["preview_token"].as_str().unwrap()).execute(&state.pool).await.unwrap();
        let expired = json!({"name":"Previewed","user_ids":["user"],"node_ids":["node"],"preview_token":preview["preview_token"]});
        assert_eq!(
            create(
                State(state.clone()),
                Json(serde_json::from_value(expired).unwrap())
            )
            .await
            .unwrap_err()
            .status,
            StatusCode::CONFLICT
        );
        let Json(preview) = preview_create(
            State(state.clone()),
            Json(serde_json::from_value(draft.clone()).unwrap()),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE users SET revision=revision+1 WHERE id='user'")
            .execute(&state.pool)
            .await
            .unwrap();
        let request = json!({"name":"Previewed","user_ids":["user"],"node_ids":["node"],"preview_token":preview["preview_token"]});
        assert_eq!(
            create(
                State(state.clone()),
                Json(serde_json::from_value(request).unwrap())
            )
            .await
            .unwrap_err()
            .status,
            StatusCode::CONFLICT
        );
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM authorization_groups")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        let Json(preview) = preview_create(
            State(state.clone()),
            Json(serde_json::from_value(draft).unwrap()),
        )
        .await
        .unwrap();
        let request = json!({"name":"Previewed","user_ids":["user"],"node_ids":["node"],"preview_token":preview["preview_token"]});
        let _ = create(
            State(state.clone()),
            Json(serde_json::from_value(request.clone()).unwrap()),
        )
        .await
        .unwrap();
        assert_eq!(
            create(State(state), Json(serde_json::from_value(request).unwrap()))
                .await
                .unwrap_err()
                .status,
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn preview_rejects_changed_node_config_and_archived_personal_binding_atomically() {
        let state = fixture().await;
        let draft =
            json!({"action":"create","name":"Guarded","user_ids":["user"],"node_ids":["node"]});
        let Json(preview) = preview_create(
            State(state.clone()),
            Json(serde_json::from_value(draft.clone()).unwrap()),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE nodes SET desired_revision=desired_revision+1,desired_config_enc=$1 WHERE id='node'").bind(state.secrets.encrypt(r#"{"bandwidth":{"up":"1 mbps"}}"#).unwrap()).execute(&state.pool).await.unwrap();
        let request = json!({"name":"Guarded","user_ids":["user"],"node_ids":["node"],"preview_token":preview["preview_token"]});
        assert_eq!(
            create(
                State(state.clone()),
                Json(serde_json::from_value(request).unwrap())
            )
            .await
            .unwrap_err()
            .status,
            StatusCode::CONFLICT
        );
        sqlx::raw_sql("INSERT INTO credentials(id,name,kind,owner_user_id,created_at,updated_at) VALUES('personal','Personal','tls_identity','user',now(),now()); INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('personal',1,'encrypted',now());").execute(&state.pool).await.unwrap();
        let mut draft = draft;
        draft["mtls_bindings"] = json!([{"user_id":"user","node_id":"node","credential_id":"personal","credential_version":1}]);
        let Json(preview) = preview_create(
            State(state.clone()),
            Json(serde_json::from_value(draft.clone()).unwrap()),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE credentials SET archived=true,revision=revision+1 WHERE id='personal'")
            .execute(&state.pool)
            .await
            .unwrap();
        draft.as_object_mut().unwrap().remove("action");
        draft["preview_token"] = preview["preview_token"].clone();
        assert!(
            create(
                State(state.clone()),
                Json(serde_json::from_value(draft).unwrap())
            )
            .await
            .is_err()
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM authorization_groups")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM node_assignments")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn migration_merges_identical_sets_without_replacing_credentials_or_empty_users() {
        let state = fixture().await;
        sqlx::raw_sql("DROP TABLE authorization_group_previews,authorization_group_users,authorization_group_nodes,authorization_groups;
            INSERT INTO users(id,name,created_at,updated_at) VALUES('peer','Peer',now(),now()),('empty','Empty',now(),now());
            INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('user','node','first','enc-first',now()),('peer','node','second','enc-second',now());")
            .execute(&state.pool).await.unwrap();
        sqlx::raw_sql("INSERT INTO credentials(id,name,kind,owner_user_id,created_at,updated_at) VALUES('cert-a','A','tls_identity','user',now(),now()),('cert-b','B','tls_identity','peer',now(),now());
            INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('cert-a',1,'payload-a',now()),('cert-b',1,'payload-b',now());
            UPDATE node_assignments SET mtls_credential_id=CASE user_id WHEN 'user' THEN 'cert-a' ELSE 'cert-b' END,mtls_credential_version=1;
            INSERT INTO subscription_credentials(id,user_id,token_hash,token_enc,created_at) VALUES('subscription','user','subscription-hash','subscription-enc',now());
            INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) VALUES('traffic','node','user','instance',10,20,3,4,now());")
            .execute(&state.pool).await.unwrap();
        let bindings_before: Vec<(String,Option<String>,Option<i64>)> = sqlx::query_as("SELECT user_id,mtls_credential_id,mtls_credential_version FROM node_assignments ORDER BY user_id").fetch_all(&state.pool).await.unwrap();
        let before: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT user_id,credential_hash,credential_enc FROM node_assignments ORDER BY user_id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../migrations/0010_authorization_groups.sql"
        ))
        .execute(&state.pool)
        .await
        .unwrap();
        let groups: i64 = sqlx::query_scalar("SELECT count(*) FROM authorization_groups")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let members: i64 = sqlx::query_scalar("SELECT count(*) FROM authorization_group_users")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!((groups, members), (1, 2));
        let after: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT user_id,credential_hash,credential_enc FROM node_assignments ORDER BY user_id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(before, after);
        let bindings_after: Vec<(String,Option<String>,Option<i64>)> = sqlx::query_as("SELECT user_id,mtls_credential_id,mtls_credential_version FROM node_assignments ORDER BY user_id").fetch_all(&state.pool).await.unwrap();
        assert_eq!(bindings_before, bindings_after);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT token_enc FROM subscription_credentials")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            "subscription-enc"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT delta_tx+delta_rx FROM traffic_records")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            7
        );
        crate::db::run_migrations(&state.pool).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM authorization_groups")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            1
        );
    }

    #[test]
    fn migration_sized_groups_are_not_subject_to_an_arbitrary_member_cap() {
        let proposal = GroupProposal {
            name: "Large".into(),
            user_ids: (0..601).map(|n| format!("user-{n}")).collect(),
            node_ids: vec![],
            mtls_bindings: vec![],
        };
        assert_eq!(normalized_proposal(proposal).unwrap().user_ids.len(), 601);
    }
}
