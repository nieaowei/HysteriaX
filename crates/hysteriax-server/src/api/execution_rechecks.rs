//! Explicit remote verification; no caller can skip a receiver-side fence.
use super::idempotency::{RequestIdentity, StoredResponse};
use super::*;
use crate::{
    execution::{self, Execution, remote::RemoteStep},
    ssh::{
        self,
        journal::{
            Endpoint,
            recheck::{self, Proof, Request as FenceRequest},
        },
    },
};
use axum::extract::OriginalUri;
use sha2::{Digest, Sha256};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Input {
    pub expected_epoch: String,
    pub expected_generation: i64,
}
#[derive(Default, Deserialize)]
pub(crate) struct ListInput {
    work_id: Option<String>,
    after: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Operation {
    operation_id: String,
    resource_key: String,
    owner: String,
    generation: i64,
    epoch: String,
    work_id: String,
    step: String,
    intent: Value,
}
impl Operation {
    fn step(&self) -> RemoteStep {
        RemoteStep {
            operation_id: self.operation_id.clone(),
            resource: self.resource_key.clone(),
            owner: self.owner.clone(),
            generation: self.generation,
            epoch: self.epoch.clone(),
            work: self.work_id.clone(),
            step: self.step.clone(),
            kind: self.intent["kind"].as_str().unwrap_or("").into(),
            request_digest: self.intent["request_digest"].as_str().unwrap_or("").into(),
            predecessor: self.intent["verification"]["expected_head"]
                .as_str()
                .map(str::to_owned),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Coordinate {
    resource: String,
    owner: Option<String>,
    generation: i64,
    epoch: String,
    work: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Data {
    original: Operation,
    node: String,
    endpoint: Endpoint,
    work_hash: String,
    coordinates: Vec<Coordinate>,
    epoch: String,
    target_generation: i64,
    fence: Option<FenceRequest>,
}
struct Prepared {
    id: String,
    data: Data,
    lease: Execution,
}
enum Preparation {
    Verified,
    Unsupported,
    Ready(Box<Prepared>),
}
const OP: &str = "SELECT operation_id,resource_key,owner,generation,epoch,work_id,step,intent,state FROM execution_operations WHERE operation_id=$1";
fn operation(row: &sqlx::postgres::PgRow) -> Operation {
    Operation {
        operation_id: row.get("operation_id"),
        resource_key: row.get("resource_key"),
        owner: row.get("owner"),
        generation: row.get("generation"),
        epoch: row.get("epoch"),
        work_id: row.get("work_id"),
        step: row.get("step"),
        intent: row.get("intent"),
    }
}
fn coordinate(r: &sqlx::postgres::PgRow) -> Coordinate {
    Coordinate {
        resource: r.get("resource_key"),
        owner: r.get("owner"),
        generation: r.get("generation"),
        epoch: r.get("epoch"),
        work: r.get("work_id"),
    }
}
async fn epoch_on(tx: &mut Transaction<'_, Postgres>) -> Result<String, ApiError> {
    Ok(sqlx::query_scalar("SELECT hysteriax_execution_epoch()")
        .fetch_one(&mut **tx)
        .await?)
}
async fn work_on(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<(String, String), ApiError> {
    let source:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('kind',kind,'node_id',node_id,'target_revision',target_revision,'payload',payload_json,'attempts',attempts,'resource_key',resource_key) FROM jobs WHERE id=$1 FOR SHARE")
        .bind(id).fetch_optional(&mut **tx).await?;
    let value = if let Some(v) = source {
        v
    } else {
        sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('kind',kind,'node_id',node_id,'cycle_id',cycle_id,'payload',payload) FROM background_work WHERE id=$1 FOR SHARE")
            .bind(id).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::conflict("execution source is missing"))?
    };
    let node = value["node_id"]
        .as_str()
        .ok_or_else(|| ApiError::conflict("execution has no SSH node"))?
        .to_owned();
    Ok((
        node,
        hex::encode(Sha256::digest(
            serde_json::to_vec(&value).map_err(|_| ApiError::internal())?,
        )),
    ))
}
async fn endpoint_on(tx: &mut Transaction<'_, Postgres>, node: &str) -> Result<Endpoint, ApiError> {
    let value:Value=sqlx::query_scalar("SELECT jsonb_build_object('host',ssh_host,'port',ssh_port,'username',ssh_username,'fingerprint',ssh_host_fingerprint) FROM nodes WHERE id=$1 FOR SHARE")
        .bind(node).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::not_found("node"))?;
    serde_json::from_value(value)
        .map_err(|_| ApiError::conflict("node SSH identity requires confirmation"))
}
async fn load_on(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
) -> Result<(Operation, String), ApiError> {
    let row = sqlx::query(OP)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| ApiError::not_found("execution operation"))?;
    Ok((operation(&row), row.get("state")))
}
async fn lock_keys(
    tx: &mut Transaction<'_, Postgres>,
    keys: &[String],
) -> Result<Vec<Coordinate>, ApiError> {
    let mut rows = Vec::new();
    for key in keys {
        let row = sqlx::query("SELECT * FROM resource_leases WHERE resource_key=$1 FOR UPDATE")
            .bind(key)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| ApiError::conflict("execution resource is missing"))?;
        rows.push(coordinate(&row));
    }
    Ok(rows)
}
fn check_input(op: &Operation, input: &Input) -> Result<(), ApiError> {
    if op.epoch != input.expected_epoch || op.generation != input.expected_generation {
        return Err(ApiError::conflict(
            "execution operation changed; reload before verification",
        ));
    }
    Ok(())
}

pub(crate) async fn list(
    State(state): State<AppState>,
    Query(input): Query<ListInput>,
) -> Result<(HeaderMap, Json<Value>), ApiError> {
    if input.work_id.as_ref().is_some_and(|v| v.len() > 200)
        || input.after.as_ref().is_some_and(|v| v.len() > 200)
    {
        return Err(ApiError::bad_request("invalid execution filter"));
    }
    let mut tx = crate::read_snapshot::begin(&state.pool).await?;
    let rows=sqlx::query("SELECT operation_id,resource_key,generation,epoch,work_id,step,state,intent->>'kind' AS kind,intent->'verification'->>'protocol' AS protocol FROM execution_operations WHERE state IN ('dispatching','reconciling','needs_attention') AND ($1::text IS NULL OR work_id=$1) AND operation_id>$2 ORDER BY operation_id LIMIT 101")
        .bind(input.work_id).bind(input.after.unwrap_or_default()).fetch_all(&mut *tx).await?;
    let more = rows.len() > 100;
    let mut items = Vec::new();
    for r in rows.into_iter().take(100) {
        items.push(json!({"operation_id":r.get::<String,_>("operation_id"),"resource_key":r.get::<String,_>("resource_key"),"work_id":r.get::<String,_>("work_id"),"generation":r.get::<i64,_>("generation"),"epoch":r.get::<String,_>("epoch"),"step":r.get::<String,_>("step"),"state":r.get::<String,_>("state"),"kind":r.get::<Option<String>,_>("kind"),"protocol":r.get::<Option<String>,_>("protocol")}));
    }
    tx.commit().await?;
    let next = if more {
        items
            .last()
            .map(|v| v["operation_id"].clone())
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok((headers, Json(json!({"items":items,"next_cursor":next}))))
}

async fn prepare(
    state: &AppState,
    id: &str,
    input: &Input,
    namespace: String,
    digest: String,
    nonce: String,
    actor: String,
) -> Result<Preparation, ApiError> {
    let (id, input, owner) = (id.to_owned(), input.clone(), state.instance_id.clone());
    db::serializable(&state.pool,move |tx| {
        let (id,input,namespace,digest,nonce,owner,actor)=(id.clone(),input.clone(),namespace.clone(),digest.clone(),nonce.clone(),owner.clone(),actor.clone());
        Box::pin(async move {
            let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM admin_tokens WHERE id=$1 AND revoked_at IS NULL)").bind(actor).fetch_one(&mut **tx).await?;
            if !active {return Err(ApiError::new(StatusCode::UNAUTHORIZED,"unauthorized","administrator token is invalid"));}
            let epoch=epoch_on(tx).await?;let slot=format!("{namespace}:{epoch}");
            sqlx::query("INSERT INTO execution_recheck_requests(namespace,request_digest) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(&slot).bind(&digest).execute(&mut **tx).await?;
            let bound:String=sqlx::query_scalar("SELECT request_digest FROM execution_recheck_requests WHERE namespace=$1 FOR UPDATE").bind(&slot).fetch_one(&mut **tx).await?;
            if bound!=digest {return Err(ApiError::conflict("Idempotency-Key was reserved for another verification"));}
            let (op,status)=load_on(tx,&id).await?;check_input(&op,&input)?;
            if status=="verified" {return Ok(Preparation::Verified);}
            if let Some(row)=sqlx::query("SELECT operation_id,request_digest FROM execution_rechecks WHERE request_namespace=$1").bind(&slot).fetch_optional(&mut **tx).await?
                && (row.get::<String,_>("operation_id")!=id || row.get::<String,_>("request_digest")!=digest) {return Err(ApiError::conflict("Idempotency-Key was reserved for another verification"));}
            let node_resource=op.resource_key.clone();let verify_key=format!("verify:{node_resource}");
            let previous=sqlx::query("SELECT id,prepared FROM execution_rechecks WHERE operation_id=$1 AND state IN ('prepared','dispatching')").bind(&id).fetch_optional(&mut **tx).await?;
            let previous=previous.map(|r|Ok::<_,ApiError>((r.get::<String,_>("id"),serde_json::from_value::<Data>(r.get("prepared")).map_err(|_|ApiError::internal())?))).transpose()?;
            let (previous,obsolete)=match previous {
                Some((old_id,data)) if data.epoch!=epoch => (None,Some(old_id)),
                other => (other,None),
            };
            let check_id=previous.as_ref().map(|v|v.0.clone()).unwrap_or(nonce);
            let mut keys:Vec<String>=sqlx::query_scalar("SELECT resource_key FROM resource_leases WHERE work_id=$1 OR resource_key=$2").bind(&op.work_id).bind(&node_resource).fetch_all(&mut **tx).await?;
            keys.push(verify_key.clone());keys.sort();keys.dedup();
            let mut lease=None;
            for key in &keys {
                if key==&verify_key {lease=execution::acquire(tx,key,&owner,&check_id).await?;if lease.is_none(){return Err(ApiError::conflict("another verification owns this resource"));}}
                else {sqlx::query("SELECT resource_key FROM resource_leases WHERE resource_key=$1 FOR UPDATE").bind(key).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::conflict("execution resource changed"))?;}
            }
            let lease=lease.unwrap();
            sqlx::query("SELECT operation_id FROM execution_operations WHERE operation_id=$1 FOR UPDATE").bind(&id).fetch_one(&mut **tx).await?;
            if let Some(old_id)=obsolete {sqlx::query("UPDATE execution_rechecks SET state='obsolete',updated_at=clock_timestamp() WHERE id=$1").bind(old_id).execute(&mut **tx).await?;}
            let (current,state)=load_on(tx,&id).await?;if current!=op {return Err(ApiError::conflict("execution intent changed"));}
            if state=="verified" {lease.release(tx).await?;return Ok(Preparation::Verified);}
            let target=sqlx::query("SELECT *,expires_at>clock_timestamp() AS live FROM resource_leases WHERE resource_key=$1").bind(&node_resource).fetch_one(&mut **tx).await?;
            if target.get::<Option<bool>,_>("live")==Some(true) {return Err(ApiError::conflict("wait for the previous execution lease to expire"));}
            if target.get::<Option<String>,_>("owner").as_deref()!=Some(op.owner.as_str()) || target.get::<Option<String>,_>("work_id").as_deref()!=Some(op.work_id.as_str()) {return Err(ApiError::conflict("execution resource ownership changed"));}
            if target.get::<i64,_>("generation")<op.generation {return Err(ApiError::conflict("execution generation moved backwards"));}
            if op.intent["kind"]!="ssh-command" || op.intent["verification"]["protocol"]!="ssh-journal-v2" || op.intent["verification"]["preparation"]=="unverified" || !node_resource.starts_with("node:") {
                lease.release(tx).await?;return Ok(Preparation::Unsupported);
            }
            let node=node_resource.strip_prefix("node:").unwrap().to_owned();
            let endpoint:Endpoint=serde_json::from_value(op.intent["verification"]["endpoint"].clone()).map_err(|_|ApiError::conflict("SSH operation has no endpoint proof"))?;
            if endpoint_on(tx,&node).await?!=endpoint {return Err(ApiError::conflict("node SSH endpoint changed"));}
            let (work_node,work_hash)=work_on(tx,&op.work_id).await?;
            if work_node!=node {return Err(ApiError::conflict("SSH operation has another node source"));}
            let data=if let Some((_,data))=previous {
                if data.original!=op || data.node!=node || data.work_hash!=work_hash || data.endpoint!=endpoint {return Err(ApiError::conflict("verification source changed"));}
                data
            } else {
                let next=target.get::<i64,_>("generation").checked_add(1).ok_or_else(||ApiError::conflict("execution generation exhausted"))?;
                sqlx::query("UPDATE resource_leases SET generation=$2,epoch=$3,blocked=true,expires_at=clock_timestamp() WHERE resource_key=$1").bind(&node_resource).bind(next).bind(&epoch).execute(&mut **tx).await?;
                let old_keys:Vec<String>=keys.iter().filter(|k|**k!=verify_key).cloned().collect();
                let coordinates=lock_keys(tx,&old_keys).await?;
                let data=Data {original:op,node,endpoint,work_hash,coordinates,epoch:epoch.clone(),target_generation:next,fence:None};
                sqlx::query("INSERT INTO execution_rechecks(id,request_namespace,request_digest,operation_id,prepared,state) VALUES($1,$2,$3,$4,$5,'prepared')")
                    .bind(&check_id).bind(&slot).bind(&digest).bind(&id).bind(serde_json::to_value(&data).map_err(|_|ApiError::internal())?).execute(&mut **tx).await?;
                data
            };
            let prepared=Prepared {id:check_id,data,lease};
            validate_on(tx,&prepared).await?;
            Ok(Preparation::Ready(Box::new(prepared)))
        })
    }).await
}

async fn validate_on(tx: &mut Transaction<'_, Postgres>, p: &Prepared) -> Result<(), ApiError> {
    let mut keys: Vec<String> = p
        .data
        .coordinates
        .iter()
        .map(|c| c.resource.clone())
        .collect();
    keys.push(p.lease.resource.clone());
    keys.sort();
    keys.dedup();
    let current = lock_keys(tx, &keys).await?;
    p.lease.guard(tx).await?;
    for old in &p.data.coordinates {
        if !current.contains(old) {
            return Err(ApiError::conflict("verification execution source changed"));
        }
    }
    sqlx::query("SELECT operation_id FROM execution_operations WHERE operation_id=$1 FOR UPDATE")
        .bind(&p.data.original.operation_id)
        .fetch_one(&mut **tx)
        .await?;
    let (op, _) = load_on(tx, &p.data.original.operation_id).await?;
    if op != p.data.original || epoch_on(tx).await? != p.data.epoch {
        return Err(ApiError::conflict(
            "verification intent or maintenance epoch changed",
        ));
    }
    if endpoint_on(tx, &p.data.node).await? != p.data.endpoint {
        return Err(ApiError::conflict("verification endpoint changed"));
    }
    let (node, hash) = work_on(tx, &op.work_id).await?;
    if node != p.data.node || hash != p.data.work_hash {
        return Err(ApiError::conflict("verification business source changed"));
    }
    Ok(())
}

pub(crate) async fn verify(
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
        "execution.reverify",
        &headers,
        json!({"method":"POST","uri":uri.to_string(),"id":id,"body":body}),
    )?;
    if let Some(response) = idempotency::replay(&state, &identity).await? {
        return response.into_http();
    }
    let input: Input = serde_json::from_value(body)
        .map_err(|_| ApiError::bad_request("invalid verification request"))?;
    if input.expected_generation < 1 || uuid::Uuid::parse_str(&input.expected_epoch).is_err() {
        return Err(ApiError::bad_request("invalid execution incarnation"));
    }
    let prepared = prepare(
        &state,
        &id,
        &input,
        identity.reservation_key(&state.secrets)?,
        identity.operation_key(&state.secrets)?,
        Uuid::new_v4().to_string(),
        actor.id.clone(),
    )
    .await?;
    let mut prepared = match prepared {
        Preparation::Verified => {
            return final_result(&state, identity, id, input, None, None, "verified").await;
        }
        Preparation::Unsupported => {
            return final_result(
                &state,
                identity,
                id,
                input,
                None,
                None,
                "unsupported_remote_proof",
            )
            .await;
        }
        Preparation::Ready(p) => *p,
    };
    let remote = async {
        let node =
            crate::deployment::load_ssh_node(&state.pool, &state.secrets, &prepared.data.node)
                .await?;
        anyhow::ensure!(
            node.host == prepared.data.endpoint.host
                && node.port == prepared.data.endpoint.port
                && node.username == prepared.data.endpoint.username
                && node.host_fingerprint.as_deref()
                    == Some(prepared.data.endpoint.fingerprint.as_str()),
            "SSH endpoint changed"
        );
        let session = match ssh::connect(&node).await? {
            ssh::FingerprintResult::Trusted(s) => s,
            _ => anyhow::bail!("SSH identity is not confirmed"),
        };
        let receipt = if prepared.data.fence.is_none() {
            Some(
                session
                    .inspect_command(
                        &prepared.data.original.step(),
                        &prepared.data.endpoint,
                        4 * 1024 * 1024,
                    )
                    .await?,
            )
        } else {
            None
        };
        Ok::<_, anyhow::Error>((session, receipt))
    };
    let observed =
        execution::supervise(state.pool.clone(), vec![prepared.lease.clone()], remote).await;
    let (session, receipt) = match observed {
        Some(Ok(v)) => v,
        _ => {
            return final_result(
                &state,
                identity,
                id,
                input,
                Some(prepared),
                None,
                "remote_verification_unavailable",
            )
            .await;
        }
    };
    if prepared.data.fence.is_none() {
        let request = FenceRequest {
            id: prepared.id.clone(),
            original: prepared.data.original.step(),
            receipt_sha256: recheck::digest(&receipt.unwrap()).map_err(|_| ApiError::internal())?,
            target_epoch: prepared.data.epoch.clone(),
            target_generation: prepared.data.target_generation,
        };
        prepared.data.fence = Some(request);
        let saved = prepared.data.clone();
        let p = prepared.clone_for_write();
        db::serializable(&state.pool,move |tx| {let (p,saved)=(p.clone_for_write(),saved.clone());Box::pin(async move {validate_on(tx,&p).await?;sqlx::query("UPDATE execution_rechecks SET prepared=$2,state='dispatching',updated_at=clock_timestamp() WHERE id=$1 AND state='prepared'").bind(&p.id).bind(serde_json::to_value(saved).map_err(|_|ApiError::internal())?).execute(&mut **tx).await?;Ok::<_,ApiError>(())})}).await?;
    }
    let result = execution::supervise(
        state.pool.clone(),
        vec![prepared.lease.clone()],
        session.fence_command(
            prepared.data.fence.as_ref().unwrap(),
            &prepared.data.endpoint,
        ),
    )
    .await;
    let proof = match result {
        Some(Ok(p)) => Some(p),
        _ => None,
    };
    let reason = if proof.is_some() {
        "verified"
    } else {
        "remote_verification_unavailable"
    };
    final_result(&state, identity, id, input, Some(prepared), proof, reason).await
}
impl Prepared {
    fn clone_for_write(&self) -> Self {
        Self {
            id: self.id.clone(),
            data: self.data.clone(),
            lease: self.lease.clone(),
        }
    }
}

async fn final_result(
    state: &AppState,
    identity: RequestIdentity,
    id: String,
    input: Input,
    p: Option<Prepared>,
    proof: Option<Proof>,
    reason: &'static str,
) -> Result<Response, ApiError> {
    let actor = identity.actor_id().to_owned();
    let p = p.map(std::sync::Arc::new);
    let audit = Uuid::new_v4().to_string();
    idempotency::execute(state,identity,move |tx| {
        let (id,input,p,proof,actor,audit)=(id.clone(),input.clone(),p.clone(),proof.clone(),actor.clone(),audit.clone());
        Box::pin(async move {
            if let Some(p)=&p {validate_on(tx,p).await?;}
            let (observed,_)=load_on(tx,&id).await?;check_input(&observed,&input)?;
            if p.is_none() {
                let mut keys:Vec<String>=sqlx::query_scalar("SELECT resource_key FROM resource_leases WHERE work_id=$1 OR resource_key=$2 ORDER BY resource_key").bind(&observed.work_id).bind(&observed.resource_key).fetch_all(&mut **tx).await?;keys.sort();lock_keys(tx,&keys).await?;
                sqlx::query("SELECT operation_id FROM execution_operations WHERE operation_id=$1 FOR UPDATE").bind(&id).fetch_one(&mut **tx).await?;
            }
            let (op,mut status)=load_on(tx,&id).await?;
            if op!=observed {return Err(ApiError::conflict("execution intent changed"));}
            if p.is_none() && status!="verified" {
                let safe:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM resource_leases WHERE resource_key=$1 AND owner=$2 AND work_id=$3 AND generation>=$4 AND (expires_at IS NULL OR expires_at<=clock_timestamp()))")
                    .bind(&op.resource_key).bind(&op.owner).bind(&op.work_id).bind(op.generation).fetch_one(&mut **tx).await?;
                if !safe {return Err(ApiError::conflict("execution resource ownership changed or is live"));}
            }
            if let Some(p)=&p {
                if let Some(proof)=&proof {
                    if Some(&proof.request)!=p.data.fence.as_ref() {return Err(ApiError::conflict("verification proof changed"));}
                    recheck::parse(&serde_json::to_vec(proof).map_err(|_|ApiError::internal())?,p.data.fence.as_ref().unwrap()).map_err(|_|ApiError::conflict("invalid verification proof"))?;
                    sqlx::query("UPDATE execution_operations SET state='verified',evidence=$2,updated_at=clock_timestamp() WHERE operation_id=$1 AND state IN ('dispatching','reconciling','needs_attention')").bind(&id).bind(serde_json::to_value(proof).map_err(|_|ApiError::internal())?).execute(&mut **tx).await?;
                    sqlx::query("UPDATE execution_rechecks SET state='verified',proof=$2,updated_at=clock_timestamp() WHERE id=$1").bind(&p.id).bind(serde_json::to_value(proof).map_err(|_|ApiError::internal())?).execute(&mut **tx).await?;status="verified".into();
                } else {
                    sqlx::query("UPDATE execution_operations SET state='needs_attention',updated_at=clock_timestamp() WHERE operation_id=$1 AND state<>'verified'").bind(&id).execute(&mut **tx).await?;
                    if p.data.fence.is_none() {sqlx::query("UPDATE execution_rechecks SET state='needs_attention',updated_at=clock_timestamp() WHERE id=$1").bind(&p.id).execute(&mut **tx).await?;}
                    status="needs_attention".into();
                }
                p.lease.release(tx).await?;
            } else if status!="verified" {
                sqlx::query("UPDATE execution_operations SET state='needs_attention',updated_at=clock_timestamp() WHERE operation_id=$1").bind(&id).execute(&mut **tx).await?;status="needs_attention".into();
            }
            let unknown=if p.is_none() && status=="verified" {
                sqlx::query_scalar::<_,bool>("SELECT coalesce((SELECT blocked FROM resource_leases WHERE resource_key=$1),false)").bind(&op.resource_key).fetch_one(&mut **tx).await?
            } else {
                sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM execution_operations WHERE (work_id=$1 OR resource_key=$2) AND state IN ('dispatching','reconciling','needs_attention'))").bind(&op.work_id).bind(&op.resource_key).fetch_one(&mut **tx).await?
            };
            if unknown && (p.is_some() || status!="verified") {
                sqlx::query("UPDATE resource_leases SET blocked=true WHERE work_id=$1 OR resource_key=$2").bind(&op.work_id).bind(&op.resource_key).execute(&mut **tx).await?;
            } else if let Some(p)=&p {
                for old in &p.data.coordinates {sqlx::query("UPDATE resource_leases SET blocked=false,owner=NULL,work_id=NULL,expires_at=NULL WHERE resource_key=$1 AND NOT EXISTS(SELECT 1 FROM execution_operations WHERE resource_key=$1 AND state IN ('dispatching','reconciling','needs_attention'))").bind(&old.resource).execute(&mut **tx).await?;}
            }
            let retry_allowed=status=="verified" && !unknown && !sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM resource_leases WHERE (work_id=$1 OR resource_key=$2) AND (blocked OR expires_at>clock_timestamp())) OR EXISTS(SELECT 1 FROM execution_operations WHERE work_id=$1 AND state IN ('dispatching','reconciling','needs_attention'))").bind(&op.work_id).bind(&op.resource_key).fetch_one(&mut **tx).await?;
            let value=json!({"operation_id":id,"recheck_id":p.as_ref().map(|p|p.id.clone()),"state":status,"resource_blocked":unknown,"retry_allowed":retry_allowed,"reason":reason});
            sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES($1,$2,'execution.reverified','execution_operation',$3,$4,clock_timestamp())")
                .bind(audit).bind(actor).bind(&id).bind(&value).execute(&mut **tx).await?;
            if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=$1)").bind(&op.work_id).fetch_one(&mut **tx).await? {crate::kick_requests::event(tx,&op.work_id,"job.reverified",value.clone()).await?;}
            Ok(StoredResponse::json(StatusCode::OK,value))
        })
    }).await?.into_http()
}

#[cfg(test)]
mod tests;
