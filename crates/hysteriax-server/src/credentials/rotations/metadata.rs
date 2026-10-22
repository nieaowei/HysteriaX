//! Publication metadata is normalized under an individual node execution scope.
//! Snapshot bytes and IDs are prepared before the pure database retry boundary.
use super::*;
use crate::background::maintenance::{self, Work};
const ADMIT: &str = "mtls-node-publication-admission";
const KIND: &str = "mtls-node-publication-metadata";
const MEMBERS: &str = "SELECT node_id FROM mtls_node_trust WHERE ca_id=$1 AND (desired OR deployed) UNION SELECT node_id FROM mtls_live_activation_trust WHERE ca_id=$1";
const NODE: &str = "SELECT desired_revision,desired_config_enc,deployed_revision,deployed_config_enc,mtls_activation_id,mtls_generation,deployed_content_sha256,state,mtls_state,ssh_credential_id,ssh_credential_version FROM nodes WHERE id=$1";

pub(super) async fn enqueue_on(
    tx: &mut Transaction<'_, Postgres>,
    rotation: &str,
    ca: &str,
    version: i64,
) -> Result<()> {
    sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING")
        .bind(format!("node-publication-admission:{rotation}")).bind(ADMIT).bind(json!({"rotation_id":rotation,"ca_id":ca,"ca_version":version,"cursor":"","lease_resources":[format!("ca:{ca}")]})).execute(&mut **tx).await?;
    Ok(())
}
pub(super) async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {_=shutdown.wait_for(|stop|*stop)=>return,_=interval.tick()=>{}}
        if let Err(error) = scheduled_pass(&state).await {
            tracing::warn!(%error,"node publication metadata deferred");
        }
    }
}
async fn scheduled_pass(state: &AppState) -> Result<()> {
    if let Some(work) = maintenance::claim(&state.pool, ADMIT, &state.instance_id).await? {
        execute_admit(&state.pool, &work).await;
    }
    if let Some(work) = maintenance::claim(&state.pool, KIND, &state.instance_id).await? {
        execute_node(state, &work).await;
    }
    Ok(())
}
pub(super) async fn pass(state: &AppState, rotation: &str) -> Result<()> {
    if let Some(work) = maintenance::claim_id(
        &state.pool,
        ADMIT,
        &state.instance_id,
        &format!("node-publication-admission:{rotation}"),
    )
    .await?
    {
        execute_admit(&state.pool, &work).await;
    }
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await?;
    let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM background_work WHERE kind=$1 AND payload->>'rotation_id'=$2 AND cycle_id IS NULL AND node_id IS NULL AND state IN ('queued','running') AND available_at<=$3 ORDER BY available_at,id LIMIT $4")
        .bind(KIND).bind(rotation).bind(now).bind(PAGE).fetch_all(&state.pool).await?;
    for id in ids {
        if let Some(work) =
            maintenance::claim_id(&state.pool, KIND, &state.instance_id, &id).await?
        {
            execute_node(state, &work).await;
        }
    }
    Ok(())
}
pub(super) async fn pending_on<'e, E>(pool: E, rotation: &str) -> Result<bool>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_work WHERE kind=ANY($1::text[]) AND payload->>'rotation_id'=$2 AND cycle_id IS NULL AND node_id IS NULL AND state<>'complete')")
        .bind([ADMIT,KIND]).bind(rotation).fetch_one(pool).await?)
}
async fn execute_admit(pool: &PgPool, work: &Work) {
    let _ = crate::execution::supervise(pool.clone(), work.leases.clone(), async {
        if let Err(error) = admit(pool, work).await {
            tracing::warn!(%error,"node metadata admission deferred");
            work.retry(pool).await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
}
async fn execute_node(state: &AppState, work: &Work) {
    let _ = crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
        let result = async {
            let prepared = prepare(state, work).await?;
            commit(state, work, prepared).await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error,"node metadata preparation deferred");
            work.retry(&state.pool).await?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
}
fn rotation_target(work: &Work) -> Result<(String, String, i64)> {
    let rotation = work.payload["rotation_id"]
        .as_str()
        .context("missing metadata rotation")?
        .to_owned();
    let ca = work.payload["ca_id"]
        .as_str()
        .context("missing metadata CA")?
        .to_owned();
    let version = work.payload["ca_version"]
        .as_i64()
        .context("missing metadata generation")?;
    ensure!(
        work.leases.iter().any(|l| l.resource == format!("ca:{ca}")),
        "missing metadata CA resource"
    );
    Ok((rotation, ca, version))
}
async fn current_on(
    tx: &mut Transaction<'_, Postgres>,
    rotation: &str,
    ca: &str,
    version: i64,
) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_rotations r JOIN mtls_authorities a ON a.credential_id=r.ca_id JOIN credentials c ON c.id=r.ca_id JOIN mtls_ca_generations g ON g.ca_id=r.ca_id AND g.ca_version=r.ca_version WHERE r.id=$1 AND r.ca_id=$2 AND r.ca_version=$3 AND r.outcome IS NULL AND r.published_at IS NOT NULL AND g.published_at IS NOT NULL AND g.state='published' AND g.expires_at>clock_timestamp() AND a.published_version=$3 AND NOT c.archived)")
        .bind(rotation).bind(ca).bind(version).fetch_one(&mut **tx).await?)
}
async fn admit(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::serializable(pool,move|tx| {let work=work.clone();Box::pin(async move {
        work.guard_on(tx).await?;let (rotation,ca,version)=rotation_target(&work)?;
        ensure!(work.id==format!("node-publication-admission:{rotation}"),"wrong metadata admission scope");
        sqlx::query("SELECT id FROM credentials WHERE id=$1 FOR KEY SHARE").bind(&ca).fetch_optional(&mut **tx).await?;
        sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE").bind(&ca).fetch_optional(&mut **tx).await?;
        if !current_on(tx,&rotation,&ca,version).await? {return work.finish_on(tx,false,0,Some("node_metadata_superseded")).await;}
        let cursor=work.payload["cursor"].as_str().context("missing metadata cursor")?;
        let nodes:Vec<String>=sqlx::query_scalar(&format!("SELECT n.id FROM nodes n WHERE n.id IN ({MEMBERS}) AND n.id>$2 ORDER BY n.id LIMIT $3"))
            .bind(&ca).bind(cursor).bind(PAGE).fetch_all(&mut **tx).await?;
        for node in &nodes {
            sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING")
                .bind(format!("node-publication:{rotation}:{node}")).bind(KIND).bind(json!({"rotation_id":rotation,"ca_id":ca,"ca_version":version,"node_id":node,"lease_resources":[format!("ca:{ca}"),format!("node:{node}")]})).execute(&mut **tx).await?;
        }
        let last=nodes.last().map(String::as_str).unwrap_or(cursor);
        let more:bool=sqlx::query_scalar(&format!("SELECT EXISTS(SELECT 1 FROM ({MEMBERS}) m WHERE node_id>$2)")).bind(&ca).bind(last).fetch_one(&mut **tx).await?;
        sqlx::query("UPDATE background_work SET payload=jsonb_set(payload,'{cursor}',to_jsonb($2::text)) WHERE id=$1").bind(&work.id).bind(last).execute(&mut **tx).await?;
        work.finish_on(tx,more,if more {1}else{0},None).await
    })}).await
}
#[derive(Clone)]
struct Target {
    rotation: String,
    ca: String,
    version: i64,
    node: String,
}
impl Target {
    fn from_work(work: &Work) -> Result<Self> {
        let (rotation, ca, version) = rotation_target(work)?;
        let t = Self {
            rotation,
            ca,
            version,
            node: work.payload["node_id"]
                .as_str()
                .context("missing metadata node")?
                .into(),
        };
        ensure!(
            work.id == format!("node-publication:{}:{}", t.rotation, t.node)
                && work
                    .leases
                    .iter()
                    .any(|l| l.resource == format!("node:{}", t.node)),
            "wrong node metadata scope"
        );
        Ok(t)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
struct NodeSource {
    desired_revision: i64,
    desired_config_enc: String,
    deployed_revision: Option<i64>,
    deployed_config_enc: Option<String>,
    mtls_activation_id: Option<String>,
    mtls_generation: i64,
    deployed_content_sha256: Option<String>,
    state: String,
    mtls_state: String,
    ssh_credential_id: Option<String>,
    ssh_credential_version: Option<i64>,
}
#[derive(Clone)]
struct History {
    id: String,
    revision: i64,
    cipher: String,
    digest: String,
    deployed: bool,
}
impl History {
    fn prepare(secrets: &SecretBox, revision: i64, value: &Value, deployed: bool) -> Result<Self> {
        let plain = value.to_string();
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            revision,
            cipher: secrets.encrypt(&plain)?,
            digest: hex::encode(Sha256::digest(plain.as_bytes())),
            deployed,
        })
    }
}
#[derive(Clone)]
struct Live {
    history: History,
    activation: String,
    snapshot_cipher: String,
    config_cipher: String,
}
#[derive(Clone)]
struct Changes {
    desired_revision: i64,
    desired_cipher: String,
    desired_policy: Settings,
    old_policy: Settings,
    live: Option<Live>,
    desired_history: Option<History>,
    parents: Vec<String>,
}
#[derive(Clone)]
struct Candidate {
    node: String,
    ca: String,
    version: i64,
    source: NodeSource,
    activation_snapshot: Option<String>,
    desired_snapshot: Option<(String, String)>,
    changes: Option<Changes>,
}
#[derive(Clone)]
enum Preparation {
    NoSource,
    Superseded(NodeSource),
    Ready(Candidate),
}

async fn prepare(state: &AppState, work: &Work) -> Result<Preparation> {
    let t = Target::from_work(work)?;
    let mut tx = crate::read_snapshot::transaction(&state.pool).await?;
    let source: Option<NodeSource> = sqlx::query_as(NODE)
        .bind(&t.node)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(source) = source.filter(|s| !matches!(s.state.as_str(), "deleting" | "delete_failed"))
    else {
        tx.commit().await?;
        return Ok(Preparation::NoSource);
    };
    if !current_on(&mut tx, &t.rotation, &t.ca, t.version).await? {
        tx.commit().await?;
        return Ok(Preparation::NoSource);
    }
    let history: Option<(String, String)> = sqlx::query_as(
        "SELECT id,config_enc FROM config_versions WHERE node_id=$1 AND revision=$2",
    )
    .bind(&t.node)
    .bind(source.desired_revision)
    .fetch_optional(&mut *tx)
    .await?;
    let activation: Option<String> = sqlx::query_scalar(
        "SELECT snapshot_enc FROM mtls_activations WHERE id=$1 AND phase='committed'",
    )
    .bind(&source.mtls_activation_id)
    .fetch_optional(&mut *tx)
    .await?;
    let proof = proof_on(&mut tx, &t.node, &t.ca, t.version).await?;
    tx.commit().await?;
    let mut desired: Value =
        serde_json::from_str(&state.secrets.decrypt(&source.desired_config_enc)?)?;
    let old_policy = Settings::from_config(&desired)?;
    let live = source
        .deployed_config_enc
        .as_ref()
        .map(|cipher| -> Result<Value> {
            Ok(serde_json::from_str(&state.secrets.decrypt(cipher)?)?)
        })
        .transpose()?;
    let desired_ca = old_policy.authority().is_some_and(|(id, _)| id == t.ca);
    let live_policy = live.as_ref().map(Settings::from_config).transpose()?;
    let live_ca = live_policy
        .as_ref()
        .and_then(Settings::authority)
        .is_some_and(|(id, _)| id == t.ca);
    let needed = old_policy
        .authority()
        .is_some_and(|(id, v)| id == t.ca && v != t.version)
        || live_policy
            .as_ref()
            .and_then(Settings::authority)
            .is_some_and(|(id, v)| id == t.ca && v != t.version);
    let mut candidate = Candidate {
        node: t.node.clone(),
        ca: t.ca.clone(),
        version: t.version,
        source: source.clone(),
        activation_snapshot: None,
        desired_snapshot: None,
        changes: None,
    };
    if !needed {
        return Ok(Preparation::Ready(candidate));
    }
    if !proof {
        return Ok(Preparation::Superseded(source));
    }
    if desired_ca {
        desired["mtls"]["ca_credential_version"] = json!(t.version);
    }
    let mut next = source.desired_revision;
    let live_change = if live_ca {
        next += 1;
        let activation_cipher = activation.context("published node proof is missing")?;
        let mut snapshot: Value =
            serde_json::from_str(&state.secrets.decrypt(&activation_cipher)?)?;
        snapshot["server_config"]["mtls"]["ca_credential_version"] = json!(t.version);
        candidate.activation_snapshot = Some(activation_cipher);
        Some(Live {
            history: History::prepare(&state.secrets, next, &snapshot, true)?,
            activation: source
                .mtls_activation_id
                .clone()
                .context("missing active proof")?,
            snapshot_cipher: state.secrets.encrypt(&snapshot.to_string())?,
            config_cipher: state
                .secrets
                .encrypt(&snapshot["server_config"].to_string())?,
        })
    } else {
        None
    };
    let desired_history = if !live_ca || source.deployed_revision != Some(source.desired_revision) {
        next += 1;
        let (id, cipher) = history.context("desired snapshot is missing")?;
        let mut snapshot: Value = serde_json::from_str(&state.secrets.decrypt(&cipher)?)?;
        snapshot["server_config"] = desired.clone();
        candidate.desired_snapshot = Some((id, cipher));
        Some(History::prepare(&state.secrets, next, &snapshot, false)?)
    } else {
        None
    };
    let mut parents = std::collections::BTreeSet::from([t.ca]);
    if let Some(ssh) = &source.ssh_credential_id {
        parents.insert(ssh.clone());
    }
    for value in std::iter::once(&desired).chain(live.iter()) {
        for (_, reference) in crate::credentials::references(value)? {
            parents.insert(reference.id);
        }
    }
    candidate.changes = Some(Changes {
        desired_revision: next,
        desired_cipher: state.secrets.encrypt(&desired.to_string())?,
        desired_policy: Settings::from_config(&desired)?,
        old_policy,
        live: live_change,
        desired_history,
        parents: parents.into_iter().collect(),
    });
    Ok(Preparation::Ready(candidate))
}
async fn insert_history(
    tx: &mut Transaction<'_, Postgres>,
    node: &str,
    h: &History,
    now: DateTime<Utc>,
) -> Result<()> {
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,deployed_success,created_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(&h.id).bind(node).bind(h.revision).bind(&h.cipher).bind(&h.digest).bind(h.deployed).bind(now).execute(&mut **tx).await?;
    Ok(())
}
async fn commit(state: &AppState, work: &Work, prepared: Preparation) -> Result<()> {
    let (work, secrets) = (work.clone(), state.secrets.clone());
    db::serializable(&state.pool,move|tx| {let (work,secrets,prepared)=(work.clone(),secrets.clone(),prepared.clone());Box::pin(async move {
        work.guard_on(tx).await?;let t=Target::from_work(&work)?;
        sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE").bind(&t.node).fetch_optional(&mut **tx).await?;
        let source:Option<NodeSource>=sqlx::query_as(NODE).bind(&t.node).fetch_optional(&mut **tx).await?;
        let Some(source)=source.filter(|s|!matches!(s.state.as_str(),"deleting"|"delete_failed")) else {return work.finish_on(tx,false,0,Some("node_metadata_removed")).await;};
        let parents=if let Preparation::Ready(c)=&prepared {c.changes.as_ref().map(|p|p.parents.clone()).unwrap_or_else(||vec![t.ca.clone()])}else{vec![t.ca.clone()]};
        sqlx::query("SELECT id FROM credentials WHERE id=ANY($1::text[]) ORDER BY id FOR KEY SHARE").bind(parents).fetch_all(&mut **tx).await?;
        sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1 FOR UPDATE").bind(&t.ca).fetch_optional(&mut **tx).await?;
        if !current_on(tx,&t.rotation,&t.ca,t.version).await? {return work.finish_on(tx,false,0,Some("node_metadata_superseded")).await;}
        let candidate=match prepared {
            Preparation::NoSource=>return work.finish_on(tx,true,1,Some("node_metadata_preparation_required")).await,
            Preparation::Superseded(before)=>{
                return if before==source {work.finish_on(tx,false,0,Some("node_metadata_proof_changed")).await}else{work.finish_on(tx,true,1,Some("node_metadata_preparation_required")).await};
            }
            Preparation::Ready(c)=>c,
        };
        ensure!(candidate.node==t.node && candidate.ca==t.ca && candidate.version==t.version,"prepared metadata scope changed");
        if candidate.source!=source {return work.finish_on(tx,true,1,Some("node_metadata_preparation_required")).await;}
        let Some(changes)=candidate.changes else {return work.finish_on(tx,false,0,None).await;};
        if !proof_on(tx,&t.node,&t.ca,t.version).await? {return work.finish_on(tx,false,0,Some("node_metadata_proof_changed")).await;}
        if let Some(cipher)=candidate.activation_snapshot {
            let actual:Option<String>=sqlx::query_scalar("SELECT snapshot_enc FROM mtls_activations WHERE id=$1 AND phase='committed'").bind(&source.mtls_activation_id).fetch_optional(&mut **tx).await?;
            if actual.as_deref()!=Some(cipher.as_str()) {return work.finish_on(tx,true,1,Some("node_metadata_preparation_required")).await;}
        }
        if let Some((id,cipher))=candidate.desired_snapshot {
            let actual:Option<String>=sqlx::query_scalar("SELECT config_enc FROM config_versions WHERE id=$1 AND node_id=$2 AND revision=$3").bind(id).bind(&t.node).bind(source.desired_revision).fetch_optional(&mut **tx).await?;
            if actual.as_deref()!=Some(cipher.as_str()) {return work.finish_on(tx,true,1,Some("node_metadata_preparation_required")).await;}
        }
        let now:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
        let mut histories=Vec::new();
        if let Some(live)=&changes.live {
            insert_history(tx,&t.node,&live.history,now).await?;histories.push(live.history.id.clone());
            sqlx::query("INSERT INTO mtls_deployment_snapshots(node_id,revision,activation_id,snapshot_enc,created_at) VALUES($1,$2,$3,$4,$5)")
                .bind(&t.node).bind(live.history.revision).bind(&live.activation).bind(&live.snapshot_cipher).bind(now).execute(&mut **tx).await?;
            sqlx::query("UPDATE nodes SET deployed_revision=$2,deployed_config_enc=$3 WHERE id=$1").bind(&t.node).bind(live.history.revision).bind(&live.config_cipher).execute(&mut **tx).await?;
        }
        if let Some(history)=&changes.desired_history {insert_history(tx,&t.node,history,now).await?;histories.push(history.id.clone());}
        sqlx::query("UPDATE nodes SET desired_revision=$2,desired_config_enc=$3,updated_at=$4 WHERE id=$1")
            .bind(&t.node).bind(changes.desired_revision).bind(&changes.desired_cipher).bind(now).execute(&mut **tx).await?;
        // The CA identity and effective authorization set did not change. Only
        // its selected version/revision changes; do not rescan all assigned users.
        if changes.old_policy!=changes.desired_policy {
            if let Some((ca,version))=changes.old_policy.authority() {sqlx::query("UPDATE mtls_node_trust SET desired=false,revision=revision+1 WHERE node_id=$1 AND ca_id=$2 AND ca_version=$3 AND desired").bind(&t.node).bind(ca).bind(version).execute(&mut **tx).await?;}
            if let Some((ca,version))=changes.desired_policy.authority() {sqlx::query("INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,desired) VALUES($1,$2,$3,true) ON CONFLICT(node_id,ca_id,ca_version) DO UPDATE SET desired=true,revision=mtls_node_trust.revision+1").bind(&t.node).bind(ca).bind(version).execute(&mut **tx).await?;}
        }
        if let Some((ca,_))=changes.desired_policy.authority() {
            sqlx::query("INSERT INTO mtls_rotation_nodes(rotation_id,node_id,expected_revision) SELECT id,$2,$3 FROM mtls_rotations WHERE ca_id=$1 AND phase<>'complete' ON CONFLICT(rotation_id,node_id) DO UPDATE SET expected_revision=$3,trust_prepared=false,probe_switched=false")
                .bind(ca).bind(&t.node).bind(changes.desired_revision).execute(&mut **tx).await?;
        }
        crate::credentials::reference_index::flush_node_sources_on(tx,&secrets,&t.node,&histories).await?;
        work.finish_on(tx,false,0,None).await
    })}).await
}

#[cfg(test)]
mod tests;
