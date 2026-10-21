//! Managed identity work attached to an authorization preview. A preview only
//! reads metadata; all keys/signatures are prepared before the commit write lock.
use super::identities::{self, PreparedIdentity};
use crate::security::SecretBox;
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::{BTreeMap, BTreeSet};

pub type Pair = (String, String);
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub user_id: String,
    pub ca_credential_id: String,
    pub ca_versions: Vec<i64>,
    pub policy_revision: i64,
    pub issue_now: bool,
    pub pending: bool,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub identities: Vec<Requirement>,
    pub automatic_issue_count: usize,
    pub pending_issue_count: usize,
}

async fn links_on(
    connection: &mut sqlx::PgConnection,
    nodes: &[String],
) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let rows=sqlx::query("SELECT DISTINCT node_id,ca_id FROM (SELECT t.node_id,t.ca_id FROM mtls_node_trust t WHERE t.node_id=ANY($1) AND (t.desired OR t.deployed) UNION SELECT node_id,ca_id FROM mtls_live_activation_trust WHERE node_id=ANY($1)) links ORDER BY node_id,ca_id")
        .bind(nodes).fetch_all(connection).await?;
    let mut output = BTreeMap::<String, BTreeSet<String>>::new();
    for row in rows {
        output
            .entry(row.get("node_id"))
            .or_default()
            .insert(row.get("ca_id"));
    }
    Ok(output)
}

pub async fn plan_on(connection: &mut sqlx::PgConnection, additions: &[Pair]) -> Result<Plan> {
    if additions.is_empty() {
        return Ok(Plan::default());
    }
    let nodes: Vec<String> = additions
        .iter()
        .map(|pair| pair.1.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let links = links_on(connection, &nodes).await?;
    let mut required = BTreeSet::<(String, String)>::new();
    for (user, node) in additions {
        for ca in links.get(node).into_iter().flatten() {
            required.insert((user.clone(), ca.clone()));
        }
    }
    if required.is_empty() {
        return Ok(Plan::default());
    }
    let users: Vec<String> = required
        .iter()
        .map(|(user, _)| user.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let cas: Vec<String> = required
        .iter()
        .map(|(_, ca)| ca.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let now = Utc::now();
    let eligibility:Vec<(String,bool)>=sqlx::query_as("SELECT id,enabled AND (expires_at IS NULL OR expires_at>$2) AND (quota_bytes IS NULL OR usage_bytes<quota_bytes) FROM users WHERE id=ANY($1)").bind(&users).bind(now).fetch_all(&mut *connection).await?;
    let eligibility: BTreeMap<_, _> = eligibility.into_iter().collect();
    let rows=sqlx::query("SELECT a.credential_id,g.ca_version,a.policy_revision FROM mtls_authorities a JOIN mtls_ca_generations g ON g.ca_id=a.credential_id WHERE a.credential_id=ANY($1) AND (g.ca_version=a.published_version OR g.state='preparing') ORDER BY a.credential_id,g.ca_version")
        .bind(&cas).fetch_all(&mut *connection).await?;
    let mut generations = BTreeMap::<String, Vec<i64>>::new();
    let mut policies = BTreeMap::<String, i64>::new();
    for row in rows {
        policies.insert(row.get("credential_id"), row.get("policy_revision"));
        generations
            .entry(row.get("credential_id"))
            .or_default()
            .push(row.get("ca_version"));
    }
    let healthy:Vec<(String,String,i64)>=sqlx::query_as("SELECT DISTINCT i.user_id,i.ca_id,v.ca_version FROM mtls_user_identities i JOIN mtls_user_versions v ON v.identity_id=i.id JOIN mtls_ca_generations g ON g.ca_id=v.ca_id AND g.ca_version=v.ca_version WHERE i.user_id=ANY($1) AND i.ca_id=ANY($2) AND NOT v.discarded AND v.not_before<=$3 AND v.expires_at>$3 AND (v.next_action_at>$3 OR v.expires_at>=g.expires_at-interval '5 minutes')")
        .bind(&users).bind(&cas).bind(now).fetch_all(&mut *connection).await?;
    let healthy: BTreeSet<_> = healthy.into_iter().collect();
    let mut plan = Plan::default();
    for (user, ca) in required {
        let versions = generations
            .get(&ca)
            .context("CA has no signing generation; update the node configuration")?
            .clone();
        let missing = versions
            .iter()
            .any(|version| !healthy.contains(&(user.clone(), ca.clone(), *version)));
        let eligible = *eligibility.get(&user).context("user no longer exists")?;
        let issue_now = missing && eligible;
        let pending = missing && !eligible;
        plan.automatic_issue_count += usize::from(issue_now);
        plan.pending_issue_count += usize::from(pending);
        plan.identities.push(Requirement {
            user_id: user,
            policy_revision: *policies.get(&ca).context("CA policy missing")?,
            ca_credential_id: ca,
            ca_versions: versions,
            issue_now,
            pending,
        });
    }
    Ok(plan)
}

/// Only public state participates in this guard; encrypted credential payloads,
/// CA keys and signing candidates never enter preview storage or responses.
pub async fn guard_on(
    connection: &mut sqlx::PgConnection,
    nodes: &[String],
    users: &[String],
) -> Result<Value> {
    let links = links_on(connection, nodes).await?;
    let cas: Vec<String> = links
        .values()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let value:Value=sqlx::query_scalar("SELECT jsonb_build_object('authorities',coalesce((SELECT jsonb_agg(to_jsonb(a)||jsonb_build_object('credential_revision',c.revision,'archived',c.archived) ORDER BY a.credential_id) FROM mtls_authorities a JOIN credentials c ON c.id=a.credential_id WHERE a.credential_id=ANY($1)),'[]'::jsonb),'generations',coalesce((SELECT jsonb_agg(to_jsonb(g) ORDER BY g.ca_id,g.ca_version) FROM mtls_ca_generations g WHERE g.ca_id=ANY($1)),'[]'::jsonb),'trust',coalesce((SELECT jsonb_agg(to_jsonb(t) ORDER BY t.node_id,t.ca_id,t.ca_version) FROM mtls_node_trust t WHERE t.node_id=ANY($2)),'[]'::jsonb),'identities',coalesce((SELECT jsonb_agg(to_jsonb(i) ORDER BY i.user_id,i.ca_id) FROM mtls_user_identities i WHERE i.user_id=ANY($3) AND i.ca_id=ANY($1)),'[]'::jsonb),'versions',coalesce((SELECT jsonb_agg(to_jsonb(v)||jsonb_build_object('time_valid',v.not_before<=$4 AND v.expires_at>$4,'renewal_due',v.next_action_at<=$4) ORDER BY v.identity_id,v.credential_version) FROM mtls_user_versions v JOIN mtls_user_identities i ON i.id=v.identity_id WHERE i.user_id=ANY($3) AND i.ca_id=ANY($1)),'[]'::jsonb),'activations',coalesce((SELECT jsonb_agg(jsonb_build_object('id',a.id,'node_id',a.node_id,'generation',a.generation,'phase',a.phase,'digest',a.target_digest,'trust',a.trust) ORDER BY a.node_id,a.generation) FROM mtls_activations a JOIN nodes n ON n.mtls_activation_id=a.id WHERE n.id=ANY($2)),'[]'::jsonb))")
        .bind(cas).bind(nodes).bind(users).bind(Utc::now()).fetch_one(connection).await?;
    Ok(value)
}

#[derive(Clone)]
pub struct Prepared {
    pub plan: Plan,
    identities: Vec<Vec<PreparedIdentity>>,
}
pub async fn prepare_preview(pool: &PgPool, secrets: &SecretBox, token: &str) -> Result<Prepared> {
    let row=sqlx::query("SELECT result_json FROM authorization_group_previews WHERE token=$1 AND consumed_at IS NULL AND expires_at>clock_timestamp()")
        .bind(token).fetch_optional(pool).await?.context("preview expired or was consumed; preview again")?;
    let result: Value = row.get("result_json");
    let plan: Plan = serde_json::from_value(result["mtls_plan"].clone())
        .context("preview predates managed identities; preview again")?;
    let mut identities = Vec::new();
    for requirement in &plan.identities {
        identities.push(
            identities::prepare_versions(
                pool,
                secrets,
                &requirement.user_id,
                &requirement.ca_credential_id,
                &requirement.ca_versions,
            )
            .await?,
        );
    }
    Ok(Prepared { plan, identities })
}

pub async fn commit_prepared(
    tx: &mut Transaction<'_, Postgres>,
    secrets: &SecretBox,
    prepared: Prepared,
    additions: &[Pair],
) -> Result<Value> {
    let current = plan_on(tx, additions).await?;
    ensure!(
        current == prepared.plan,
        "managed identity requirements changed during signing; preview again"
    );
    let mut result = Vec::new();
    for (requirement, candidates) in prepared
        .plan
        .identities
        .into_iter()
        .zip(prepared.identities)
    {
        let versions = identities::commit_versions(tx, secrets, candidates).await?;
        result.push(json!({"user_id":requirement.user_id,"ca_credential_id":requirement.ca_credential_id,"identity_id":versions.first().map(|v|&v.0),"ca_versions":requirement.ca_versions,"policy_revision":requirement.policy_revision,"issued":requirement.issue_now,"pending":requirement.pending}));
    }
    Ok(json!(result))
}

/// Revoke only the last effective CA reference. Historical leaves and their CA
/// versions remain pinned even when an identity is archived.
pub async fn reconcile_on(
    tx: &mut Transaction<'_, Postgres>,
    users: &[String],
    changed: &[Pair],
    exclude_node: Option<&str>,
) -> Result<()> {
    if users.is_empty() {
        return Ok(());
    }
    let nodes:Vec<String>=sqlx::query_scalar("SELECT DISTINCT node_id FROM node_assignments WHERE user_id=ANY($1) AND ($2::text IS NULL OR node_id<>$2)").bind(users).bind(exclude_node).fetch_all(&mut **tx).await?;
    let mut all_nodes: BTreeSet<String> = nodes.iter().cloned().collect();
    all_nodes.extend(changed.iter().map(|pair| pair.1.clone()));
    let links = links_on(tx, &all_nodes.into_iter().collect::<Vec<_>>()).await?;
    let rows:Vec<Pair>=sqlx::query_as("SELECT user_id,node_id FROM node_assignments WHERE user_id=ANY($1) AND ($2::text IS NULL OR node_id<>$2)").bind(users).bind(exclude_node).fetch_all(&mut **tx).await?;
    let mut active = BTreeSet::<(String, String)>::new();
    for (user, node) in rows {
        for ca in links.get(&node).into_iter().flatten() {
            active.insert((user.clone(), ca.clone()));
        }
    }
    let mut touched = BTreeSet::<(String, String)>::new();
    for (user, node) in changed {
        for ca in links.get(node).into_iter().flatten() {
            touched.insert((user.clone(), ca.clone()));
        }
    }
    let identities=sqlx::query("SELECT id,user_id,ca_id,credential_id,archived FROM mtls_user_identities WHERE user_id=ANY($1)").bind(users).fetch_all(&mut **tx).await?;
    for row in identities {
        let key = (
            row.get::<String, _>("user_id"),
            row.get::<String, _>("ca_id"),
        );
        let archived = !active.contains(&key);
        let was_archived: bool = row.get("archived");
        let changed = touched.contains(&key) || archived != was_archived;
        if !changed {
            continue;
        }
        sqlx::query("UPDATE mtls_user_identities SET archived=$2,authorization_revision=authorization_revision+1,revision=revision+1,next_action_at=CASE WHEN $2 THEN NULL ELSE next_action_at END,updated_at=$3 WHERE id=$1")
            .bind(row.get::<String,_>("id")).bind(archived).bind(Utc::now()).execute(&mut **tx).await?;
        if let Some(credential) = row.get::<Option<String>, _>("credential_id") {
            sqlx::query("UPDATE credentials SET archived=$2,revision=revision+1,updated_at=$3 WHERE id=$1 AND archived<>$2").bind(credential).bind(archived).bind(Utc::now()).execute(&mut **tx).await?;
        }
    }
    Ok(())
}

pub async fn reconcile_node(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    removing: bool,
) -> Result<()> {
    let pairs: Vec<Pair> = sqlx::query_as(
        "SELECT user_id,node_id FROM node_assignments WHERE node_id=$1 ORDER BY user_id",
    )
    .bind(node)
    .fetch_all(&mut **tx)
    .await?;
    let users: Vec<String> = pairs.iter().map(|pair| pair.0.clone()).collect();
    reconcile_on(
        tx,
        &users,
        if removing { &pairs } else { &[] },
        removing.then_some(node),
    )
    .await
}

pub async fn assignment_refs_on(
    connection: &mut sqlx::PgConnection,
    users: &[String],
) -> Result<BTreeMap<Pair, Vec<String>>> {
    let rows=sqlx::query("WITH links AS (SELECT node_id,ca_id FROM mtls_node_trust WHERE desired OR deployed UNION SELECT node_id,ca_id FROM mtls_live_activation_trust) SELECT DISTINCT a.user_id,a.node_id,i.id FROM node_assignments a JOIN links l ON l.node_id=a.node_id JOIN mtls_user_identities i ON i.user_id=a.user_id AND i.ca_id=l.ca_id AND NOT i.archived WHERE a.user_id=ANY($1) ORDER BY a.user_id,a.node_id,i.id")
        .bind(users).fetch_all(connection).await?;
    let mut output = BTreeMap::<Pair, Vec<String>>::new();
    for row in rows {
        output
            .entry((row.get("user_id"), row.get("node_id")))
            .or_default()
            .push(row.get("id"));
    }
    Ok(output)
}

pub async fn referenced_nodes_on(
    connection: &mut sqlx::PgConnection,
    user: &str,
    ca: &str,
) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("WITH links AS (SELECT node_id,ca_id FROM mtls_node_trust WHERE desired OR deployed UNION SELECT node_id,ca_id FROM mtls_live_activation_trust) SELECT DISTINCT a.node_id FROM node_assignments a JOIN links l ON l.node_id=a.node_id WHERE a.user_id=$1 AND l.ca_id=$2 ORDER BY a.node_id")
        .bind(user).bind(ca).fetch_all(connection).await?)
}

pub async fn annotate_results_on(
    connection: &mut sqlx::PgConnection,
    results: &mut Value,
) -> Result<()> {
    for identity in results
        .as_array_mut()
        .context("managed identity result must be an array")?
    {
        let user = identity["user_id"]
            .as_str()
            .context("identity owner missing")?;
        let ca = identity["ca_credential_id"]
            .as_str()
            .context("identity CA missing")?;
        identity["affected_node_ids"] = json!(referenced_nodes_on(connection, user, ca).await?);
    }
    Ok(())
}
