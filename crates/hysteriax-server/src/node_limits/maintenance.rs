//! Bounded package evaluation and restriction/restore dispatch. These operations
//! only touch PostgreSQL; remote kick execution remains a separate leased job.
use super::*;
use crate::background::maintenance::{self as work_queue, Work};

pub const NODE_BATCH: i64 = 64;
const EVALUATION_LANES: usize = 4;
pub const USER_BATCH: i64 = 128;
pub const KIND: &str = "node-limit-dispatch";
const SCAN_KIND: &str = "node-limit-scan";

pub async fn scan_work(pool: &PgPool, work: &Work) -> Result<usize> {
    let began = std::time::Instant::now();
    let mut total = 0;
    // Amortize admission/renewal setup across four short result transactions.
    // The due timestamp is each page's durable checkpoint; no node lock crosses
    // a page boundary, and takeover simply resumes the remaining due rows.
    for page in 0..4 {
        let work = work.clone();
        let (count, finished) = db::write(pool, move |tx| {
            let work = work.clone();
            Box::pin(async move {
                work.guard_on(tx).await?;
                let nodes: Vec<String> = sqlx::query_scalar(include_str!("due_nodes.sql"))
                    .bind(NODE_BATCH)
                    .fetch_all(&mut **tx)
                    .await?;
                let now = sqlx::query_scalar("SELECT clock_timestamp()")
                    .fetch_one(&mut **tx)
                    .await?;
                evaluation::evaluate_many(tx, &nodes, now).await?;
                let finished = nodes.len() < NODE_BATCH as usize || page == 3;
                if finished {
                    work.finish_on(
                        tx,
                        false,
                        if nodes.len() == NODE_BATCH as usize {
                            0
                        } else {
                            1
                        },
                        None,
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>((nodes.len(), finished))
            })
        })
        .await?;
        total += count;
        if finished {
            break;
        }
        tokio::task::yield_now().await;
    }
    tracing::debug!(
        nodes = total,
        elapsed_ms = began.elapsed().as_millis() as u64,
        "due node packages evaluated"
    );
    Ok(total)
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

pub async fn pass(pool: &PgPool, owner: &str) -> Result<usize> {
    let keys: Vec<String> = (0..EVALUATION_LANES)
        .map(|lane| format!("node-limits:evaluate:{lane}"))
        .collect();
    work_queue::ensure_many(pool, &keys, SCAN_KIND, json!({})).await?;
    let mut tasks = tokio::task::JoinSet::new();
    for key in keys {
        let pool = pool.clone();
        let owner = owner.to_owned();
        tasks.spawn(async move {
            let Some(work) =
                work_queue::claim_id(&pool, SCAN_KIND, &owner, &format!("maintenance:{key}"))
                    .await?
            else {
                return Ok(0);
            };
            crate::execution::supervise(pool.clone(), work.leases.clone(), async {
                match scan_work(&pool, &work).await {
                    Ok(count) => Ok(count),
                    Err(error) => {
                        tracing::warn!(%error,"due node package work deferred");
                        work.retry(&pool).await?;
                        Ok::<_, anyhow::Error>(0)
                    }
                }
            })
            .await
            .unwrap_or(Ok(0))
        });
    }
    let mut processed = 0;
    while let Some(result) = tasks.join_next().await {
        processed += result??;
    }
    if let Some(work) = work_queue::claim(pool, KIND, owner).await? {
        crate::execution::supervise(pool.clone(), work.leases.clone(), async {
            if let Err(error) = dispatch_work(pool, &work).await {
                tracing::warn!(%error,"node restriction work deferred");
                work.retry(pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .transpose()?;
    }
    Ok(processed)
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
#[path = "maintenance_tests.rs"]
mod tests;
