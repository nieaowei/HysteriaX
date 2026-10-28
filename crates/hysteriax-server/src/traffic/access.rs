//! Durable access revocation fan-out. Billing and expiry admit one work item
//! atomically with the user's restriction marker; node jobs are bounded pages.
use crate::{background::maintenance, db, error::ApiError};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgPool, Postgres, Row, Transaction};

pub const USER_BATCH: i64 = 128;
pub const NODE_BATCH: i64 = 128;
pub const KIND: &str = "access-revocation";
const DUE_USERS_SQL: &str = "SELECT u.id FROM user_access_checks c JOIN users u ON u.id=c.user_id WHERE c.next_action_at<=$1 AND u.access_kick_enqueued_at IS NULL ORDER BY c.next_action_at,c.user_id LIMIT $2 FOR NO KEY UPDATE OF u SKIP LOCKED";

const INITIALIZATION_KEY: &str = "user-access-checks-v1";
pub async fn assert_ready(pool: &PgPool) -> Result<()> {
    let ready: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_initialization WHERE key=$1)")
            .bind(INITIALIZATION_KEY)
            .fetch_one(pool)
            .await?;
    anyhow::ensure!(
        ready,
        "access check initialization is incomplete; run --migrate-only during maintenance"
    );
    Ok(())
}
pub async fn initialize(pool: &PgPool) -> Result<()> {
    sqlx::query("DELETE FROM background_initialization WHERE key=$1")
        .bind(INITIALIZATION_KEY)
        .execute(pool)
        .await?;
    let mut after: Option<String> = None;
    loop {
        let cursor = after.clone();
        let last:Option<String>=db::serializable::<_,anyhow::Error,_>(pool,move |tx| {
            let cursor=cursor.clone();
            Box::pin(async move {
                let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM users WHERE ($1::text IS NULL OR id>$1) ORDER BY id LIMIT 256 FOR SHARE")
                    .bind(cursor).fetch_all(&mut **tx).await?;
                sqlx::query("SELECT hysteriax_refresh_user_access_check(id,enabled,expires_at,quota_bytes,usage_bytes,access_kick_enqueued_at) FROM users WHERE id=ANY($1::text[]) ORDER BY id")
                    .bind(&ids).execute(&mut **tx).await?;
                Ok(ids.last().cloned())
            })
        }).await?;
        let Some(last) = last else {
            break;
        };
        after = Some(last);
        tokio::task::yield_now().await;
    }
    sqlx::query("INSERT INTO background_initialization(key) VALUES($1) ON CONFLICT(key) DO UPDATE SET completed_at=clock_timestamp()")
        .bind(INITIALIZATION_KEY).execute(pool).await?;
    Ok(())
}

pub async fn admit_on(tx: &mut Transaction<'static, Postgres>, user: &str) -> Result<()> {
    admit_many_on(tx, &[user.to_owned()]).await
}

/// Call after locking every user parent. Only newly marked restrictions receive
/// work, and the next statement sees the generation written by AFTER triggers.
pub async fn admit_many_on(
    tx: &mut Transaction<'static, Postgres>,
    users: &[String],
) -> Result<()> {
    if users.is_empty() {
        return Ok(());
    }
    let marked:Vec<String>=sqlx::query_scalar("UPDATE users SET access_kick_enqueued_at=clock_timestamp(),updated_at=clock_timestamp() WHERE id=ANY($1::text[]) AND access_kick_enqueued_at IS NULL AND (NOT enabled OR expires_at<=clock_timestamp() OR usage_bytes>=quota_bytes) RETURNING id")
        .bind(users).fetch_all(&mut **tx).await?;
    if !marked.is_empty() {
        let inserted=sqlx::query("INSERT INTO background_work(id,kind,payload) SELECT 'access-restriction:'||gen_random_uuid()::text,$1,jsonb_build_object('user_id',u.id,'restriction_marker',u.access_kick_enqueued_at,'restriction_generation',e.generation,'after_node',NULL) FROM users u JOIN user_access_restriction_epochs e ON e.user_id=u.id WHERE u.id=ANY($2::text[]) ORDER BY u.id")
            .bind(KIND).bind(&marked).execute(&mut **tx).await?.rows_affected();
        anyhow::ensure!(
            inserted == marked.len() as u64,
            "restriction generation is missing for a marked user"
        );
    }
    Ok(())
}

pub async fn scan_work(pool: &PgPool, work: &maintenance::Work) -> Result<()> {
    let work = work.clone();
    db::write(pool, move |tx| {
        let work = work.clone();
        Box::pin(async move {
            work.guard_on(tx).await?;
            let cutoff: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut **tx)
                .await?;
            let users: Vec<String> = sqlx::query_scalar(DUE_USERS_SQL)
                .bind(cutoff)
                .bind(USER_BATCH)
                .fetch_all(&mut **tx)
                .await?;
            for user in &users {
                admit_on(tx, user).await?;
            }
            work.finish_on(
                tx,
                false,
                if users.len() == USER_BATCH as usize {
                    1
                } else {
                    10
                },
                None,
            )
            .await
        })
    })
    .await
}

pub async fn dispatch_work(pool: &PgPool, work: &maintenance::Work) -> Result<()> {
    let work = work.clone();
    db::serializable::<_,ApiError,_>(pool,move |tx| {
        let work=work.clone();
        Box::pin(async move {
            work.guard_on(tx).await?;
            let user=work.payload["user_id"].as_str().context("missing revocation user")?;
            let marker:DateTime<Utc>=serde_json::from_value(work.payload["restriction_marker"].clone()).map_err(anyhow::Error::from)?;
            let generation=work.payload["restriction_generation"].as_i64().context("missing restriction generation")?;
            let after=work.payload["after_node"].as_str();
            // Do not SKIP a parent then advance past it: every assignment before
            // the persisted cursor must already have its durable obligation.
            // A busy parent defers this user; other user work remains claimable.
            let nodes=sqlx::query("SELECT n.id,n.state FROM node_assignments a JOIN nodes n ON n.id=a.node_id WHERE a.user_id=$1 AND ($2::text IS NULL OR n.id>$2) ORDER BY n.id LIMIT $3 FOR SHARE OF n")
                .bind(user).bind(after).bind(NODE_BATCH).fetch_all(&mut **tx).await?;
            let active:Option<bool>=sqlx::query_scalar("SELECT coalesce(u.access_kick_enqueued_at=$2 AND e.generation=$3 AND (NOT u.enabled OR u.expires_at<=clock_timestamp() OR u.usage_bytes>=u.quota_bytes),false) FROM users u LEFT JOIN user_access_restriction_epochs e ON e.user_id=u.id WHERE u.id=$1 FOR NO KEY UPDATE OF u")
                .bind(user).bind(marker).bind(generation).fetch_optional(&mut **tx).await?;
            if active!=Some(true) {
                work.finish_on(tx,false,0,Some("restriction_superseded")).await?;
                return Ok(());
            }
            for node in &nodes {
                let id:String=node.get("id");
                let payload=json!({"user_id":user,"kick_reason":"access_restricted"});
                if node.get::<String,_>("state")=="deleting" {
                    crate::kick_requests::record_many(tx,&id,&[user.to_owned()],"access_restricted").await?;
                } else {
                    crate::api::enqueue_job_with_payload_in_tx(tx,"kick",Some(&id),None,payload).await?;
                }
            }
            let last=nodes.last().map(|n|n.get::<String,_>("id"));
            let more=if let Some(last)=&last {
                sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM node_assignments WHERE user_id=$1 AND node_id>$2)").bind(user).bind(last).fetch_one(&mut **tx).await?
            } else {false};
            if let Some(last)=last {
                sqlx::query("UPDATE background_work SET payload=jsonb_set(payload,'{after_node}',$2) WHERE id=$1")
                    .bind(&work.id).bind(json!(last)).execute(&mut **tx).await?;
            }
            work.finish_on(tx,more,if more{1}else{0},None).await?;
            Ok(())
        })
    }).await.map_err(|error|anyhow::anyhow!(error.message))
}

pub async fn dispatch_pass(pool: &PgPool, owner: &str) -> Result<()> {
    if let Some(work) = maintenance::claim(pool, KIND, owner).await? {
        crate::execution::supervise(pool.clone(), work.leases.clone(), async {
            if let Err(error) = dispatch_work(pool, &work).await {
                tracing::warn!(%error,"access revocation page deferred");
                work.retry(pool).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .transpose()?;
    }
    Ok(())
}

#[cfg(test)]
pub async fn drain_for_test(pool: &PgPool) {
    let owner = uuid::Uuid::new_v4().to_string();
    for _ in 0..1000 {
        sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE kind=$1 AND state='queued'").bind(KIND).execute(pool).await.unwrap();
        let Some(work) = maintenance::claim(pool, KIND, &owner).await.unwrap() else {
            return;
        };
        dispatch_work(pool, &work).await.unwrap();
    }
    panic!("access test drain did not converge");
}

#[cfg(test)]
#[path = "access_tests.rs"]
mod tests;
