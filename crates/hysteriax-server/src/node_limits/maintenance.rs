//! Bounded package evaluation and restriction/restore dispatch. These operations
//! only touch PostgreSQL; remote kick execution remains a separate leased job.
use super::*;
use crate::background::maintenance::{self as work_queue, Work};

pub const NODE_BATCH: i64 = 128;
pub const USER_BATCH: i64 = 128;
pub const KIND: &str = "node-limit-dispatch";
const SCAN_KIND: &str = "node-limit-scan";

pub async fn admit_on(tx: &mut Transaction<'_, Postgres>, node: &str, blocked: bool) -> Result<()> {
    let generation:i64=sqlx::query_scalar("INSERT INTO node_limit_epochs(node_id,generation) VALUES($1,1) ON CONFLICT(node_id) DO UPDATE SET generation=node_limit_epochs.generation+1 RETURNING generation")
        .bind(node).fetch_one(&mut **tx).await?;
    sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3)")
        .bind(format!("node-limit:{node}:{generation}")).bind(KIND)
        .bind(json!({"node_id":node,"restriction_generation":generation,"restricted":blocked,"after_user":null}))
        .execute(&mut **tx).await?;
    Ok(())
}

pub async fn scan_work(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::write(pool,move |tx| {
        let work=work.clone();
        Box::pin(async move {
            work.guard_on(tx).await?;
            let after=work.payload["after_node"].as_str();
            // Skipped parents are revisited after wrapping. No permanent scan
            // checkpoint claims a skipped node was evaluated.
            let nodes:Vec<String>=sqlx::query_scalar("SELECT id FROM nodes WHERE state NOT IN ('deleting','delete_failed') AND ($1::text IS NULL OR id>$1) ORDER BY id LIMIT $2 FOR NO KEY UPDATE SKIP LOCKED")
                .bind(after).bind(NODE_BATCH).fetch_all(&mut **tx).await?;
            let now=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
            for node in &nodes {
                ensure(tx,node).await?;
                evaluate(tx,node,now).await?;
            }
            let last=if nodes.len()==NODE_BATCH as usize {nodes.last()} else {None};
            sqlx::query("UPDATE background_work SET payload=jsonb_set(payload,'{after_node}',$2) WHERE id=$1")
                .bind(&work.id).bind(json!(last)).execute(&mut **tx).await?;
            work.finish_on(tx,false,1,None).await
        })
    }).await
}

pub async fn dispatch_work(pool: &PgPool, work: &Work) -> Result<()> {
    let work = work.clone();
    db::serializable::<_,ApiError,_>(pool,move |tx| {
        let work=work.clone();
        Box::pin(async move {
            work.guard_on(tx).await?;
            let node=work.payload["node_id"].as_str().context("missing package node")?;
            let generation=work.payload["restriction_generation"].as_i64().context("missing package episode")?;
            let blocked=work.payload["restricted"].as_bool().context("missing package restriction")?;
            let status:Option<String>=sqlx::query_scalar("SELECT state FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
                .bind(node).fetch_optional(&mut **tx).await?;
            let Some(status)=status else {
                work.finish_on(tx,false,0,Some("node_removed")).await?;
                return Ok(());
            };
            // Expiry/rollover may supersede an earlier restore even when there
            // has been no API write since its admission.
            let now=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
            evaluate(tx,node,now).await?;
            let current:Option<bool>=sqlx::query_scalar("SELECT e.generation=$2 AND p.restricted=$3 FROM node_limit_epochs e JOIN node_packages p ON p.node_id=e.node_id WHERE e.node_id=$1")
                .bind(node).bind(generation).bind(blocked).fetch_optional(&mut **tx).await?;
            if current!=Some(true) {
                work.finish_on(tx,false,0,Some("restriction_superseded")).await?;
                return Ok(());
            }
            let after=work.payload["after_user"].as_str();
            let users:Vec<String>=if blocked {
                sqlx::query_scalar("SELECT user_id FROM node_assignments WHERE node_id=$1 AND ($2::text IS NULL OR user_id>$2) ORDER BY user_id LIMIT $3")
                    .bind(node).bind(after).bind(USER_BATCH).fetch_all(&mut **tx).await?
            } else {
                sqlx::query_scalar("SELECT user_id FROM kick_requests WHERE node_id=$1 AND reasons ? 'node_limit' AND state NOT IN ('completed','cancelled') AND ($2::text IS NULL OR user_id>$2) ORDER BY user_id LIMIT $3")
                    .bind(node).bind(after).bind(USER_BATCH).fetch_all(&mut **tx).await?
            };
            // Match API lock order: node parents, then existing user parents,
            // then per-user obligations/jobs. Never skip a user and pass it.
            sqlx::query("SELECT id FROM users WHERE id=ANY($1::text[]) ORDER BY id FOR NO KEY UPDATE")
                .bind(&users).fetch_all(&mut **tx).await?;
            if blocked && !users.is_empty() {
                if matches!(status.as_str(),"deleting"|"delete_failed") {
                    crate::kick_requests::record_many(tx,node,&users,"node_limit").await?;
                } else {
                    crate::kick_requests::enqueue_many(tx,node,&users,"node_limit").await?;
                }
            } else {
                for user in &users {crate::kick_requests::clear_reason(tx,Some(node),Some(user),"node_limit").await?;}
            }
            let more=users.len()==USER_BATCH as usize;
            if let Some(last)=users.last() {
                sqlx::query("UPDATE background_work SET payload=jsonb_set(payload,'{after_user}',$2) WHERE id=$1")
                    .bind(&work.id).bind(json!(last)).execute(&mut **tx).await?;
            }
            work.finish_on(tx,more,if more{1}else{0},None).await?;
            Ok(())
        })
    }).await.map_err(|error|anyhow::anyhow!(error.message))
}

pub async fn pass(pool: &PgPool, owner: &str) -> Result<()> {
    work_queue::ensure(
        pool,
        "node-limits:evaluate",
        SCAN_KIND,
        json!({"after_node":null}),
    )
    .await?;
    for kind in [SCAN_KIND, KIND] {
        if let Some(work) = work_queue::claim(pool, kind, owner).await? {
            crate::execution::supervise(pool.clone(), work.leases.clone(), async {
                let result = if kind == SCAN_KIND {
                    scan_work(pool, &work).await
                } else {
                    dispatch_work(pool, &work).await
                };
                if let Err(error) = result {
                    tracing::warn!(%error,kind,"node package work deferred");
                    work.retry(pool).await?;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await
            .transpose()?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub async fn drain_for_test(pool: &PgPool) {
    for _ in 0..1000 {
        sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE kind=$1 AND state='queued'")
            .bind(KIND).execute(pool).await.unwrap();
        let Some(work) = work_queue::claim(pool, KIND, "node-limit-test")
            .await
            .unwrap()
        else {
            return;
        };
        dispatch_work(pool, &work).await.unwrap();
    }
    panic!("node restriction dispatch did not converge");
}

#[cfg(test)]
mod tests;
