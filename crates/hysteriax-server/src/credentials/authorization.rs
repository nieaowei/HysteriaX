//! Durable identity admission for direct authorization changes.
//! Authorization commits identity requirements and work, never signing material.
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
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

/// A page locks only its linked CA parents after the caller locks the user.
pub(crate) async fn lock_links_on(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    nodes: &[String],
) -> Result<()> {
    let cas: Vec<String> = links_on(tx, nodes)
        .await?
        .into_values()
        .flatten()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if cas.is_empty() {
        return Ok(());
    }
    sqlx::query("SELECT id FROM credentials WHERE id=ANY($1) OR id IN (SELECT credential_id FROM mtls_user_identities WHERE user_id=$2 AND ca_id=ANY($1)) ORDER BY id FOR UPDATE")
        .bind(&cas).bind(user).fetch_all(&mut **tx).await?;
    sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=ANY($1) ORDER BY credential_id FOR UPDATE")
        .bind(&cas).fetch_all(&mut **tx).await?;
    sqlx::query("SELECT id FROM mtls_user_identities WHERE user_id=$1 AND ca_id=ANY($2) ORDER BY id FOR UPDATE")
        .bind(user).bind(&cas).fetch_all(&mut **tx).await?;
    Ok(())
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

#[derive(Clone)]
pub struct Prepared {
    pub plan: Plan,
    // Allocate IDs once so transaction retries preserve candidate identity IDs.
    identity_ids: Vec<String>,
}
pub fn prepare_plan(plan: Plan) -> Prepared {
    let identity_ids = plan
        .identities
        .iter()
        .map(|_| uuid::Uuid::new_v4().to_string())
        .collect();
    Prepared { plan, identity_ids }
}

pub async fn commit_prepared(
    tx: &mut Transaction<'_, Postgres>,
    prepared: Prepared,
    additions: &[Pair],
) -> Result<Value> {
    let current = plan_on(tx, additions).await?;
    ensure!(
        current == prepared.plan,
        "managed identity requirements changed; retry the change"
    );
    let mut result = Vec::new();
    let entries: Vec<_> = prepared
        .plan
        .identities
        .iter()
        .zip(&prepared.identity_ids)
        .collect();
    // Bound each statement independently of group size (100k users / 10k nodes).
    for page in entries.chunks(256) {
        let input: Vec<_> = page
            .iter()
            .map(|(r, id)| {
                json!({
                    "id":id,"user_id":r.user_id,"ca_id":r.ca_credential_id,"issue":r.issue_now
                })
            })
            .collect();
        let rows = sqlx::query(r#"
INSERT INTO mtls_user_identities(id,user_id,ca_id,next_action_at,created_at,updated_at)
SELECT id,user_id,ca_id,CASE WHEN issue THEN clock_timestamp() END,clock_timestamp(),clock_timestamp()
FROM jsonb_to_recordset($1) AS r(id text,user_id text,ca_id text,issue boolean)
ORDER BY user_id,ca_id
ON CONFLICT(user_id,ca_id) DO UPDATE SET archived=false,
 next_action_at=CASE WHEN EXCLUDED.next_action_at IS NOT NULL THEN EXCLUDED.next_action_at ELSE mtls_user_identities.next_action_at END,
 updated_at=clock_timestamp()
RETURNING id,user_id,ca_id
"#).bind(json!(input)).fetch_all(&mut **tx).await?;
        let ids: BTreeMap<Pair, String> = rows
            .into_iter()
            .map(|row| ((row.get("user_id"), row.get("ca_id")), row.get("id")))
            .collect();
        let mut work = Vec::new();
        for (r, _) in page {
            let id = &ids[&(r.user_id.clone(), r.ca_credential_id.clone())];
            if r.issue_now {
                work.push(json!({"id":format!("mtls-identity:{id}"),"payload":{
                    "identity_id":id,"user_id":r.user_id,"ca_id":r.ca_credential_id,
                    "lease_resources":[format!("identity:{id}")]
                }}));
            }
            result.push(
                json!({"user_id":r.user_id,"ca_credential_id":r.ca_credential_id,
                "identity_id":id,"ca_versions":r.ca_versions,"policy_revision":r.policy_revision,
                "issued":false,"pending":r.issue_now || r.pending}),
            );
        }
        if !work.is_empty() {
            sqlx::query(r#"
INSERT INTO background_work(id,kind,payload)
SELECT id,'mtls-identity-renewal',payload FROM jsonb_to_recordset($1) AS r(id text,payload jsonb) ORDER BY id
ON CONFLICT(id) DO UPDATE SET state='queued',owner=NULL,started_at=NULL,finished_at=NULL,
 error_code=NULL,available_at=clock_timestamp()
WHERE background_work.kind=excluded.kind AND background_work.state IN ('complete','failed')
"#).bind(json!(work)).execute(&mut **tx).await?;
        }
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
    let mut identity_ids = Vec::<String>::new();
    let mut archived_states = Vec::<bool>::new();
    let mut credential_ids = Vec::<String>::new();
    let mut credential_states = Vec::<bool>::new();
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
        identity_ids.push(row.get::<String, _>("id"));
        archived_states.push(archived);
        if let Some(credential) = row.get::<Option<String>, _>("credential_id") {
            credential_ids.push(credential);
            credential_states.push(archived);
        }
    }
    if !identity_ids.is_empty() {
        sqlx::query("UPDATE mtls_user_identities i SET archived=p.archived,authorization_revision=i.authorization_revision+1,revision=i.revision+1,next_action_at=CASE WHEN p.archived THEN NULL ELSE i.next_action_at END,updated_at=$3 FROM unnest($1::text[],$2::bool[]) AS p(id,archived) WHERE i.id=p.id")
            .bind(identity_ids).bind(archived_states).bind(Utc::now()).execute(&mut **tx).await?;
    }
    if !credential_ids.is_empty() {
        sqlx::query("UPDATE credentials c SET archived=p.archived,revision=c.revision+1,updated_at=$3 FROM unnest($1::text[],$2::bool[]) AS p(id,archived) WHERE c.id=p.id AND c.archived<>p.archived")
            .bind(credential_ids).bind(credential_states).bind(Utc::now()).execute(&mut **tx).await?;
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
