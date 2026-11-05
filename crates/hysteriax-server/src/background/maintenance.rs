//! Durable scheduling for bounded, database-only maintenance batches.
//! Remote and node-owned work must use their additional resource leases.
use crate::{
    db,
    execution::{self, Execution},
};
use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};

#[derive(Clone, Debug)]
pub struct Work {
    pub id: String,
    pub payload: Value,
    pub leases: Vec<Execution>,
}

/// A recurring key has one durable work row and one monotonically increasing
/// work lease. A live running owner is never reset by another scheduler.
pub async fn ensure(pool: &PgPool, key: &str, kind: &str, payload: Value) -> Result<()> {
    let (id, kind) = (format!("maintenance:{key}"), kind.to_owned());
    db::write(pool,move |tx| {
        let (id,kind,payload)=(id.clone(),kind.clone(),payload.clone());
        Box::pin(async move {
            // ON CONFLICT DO UPDATE waits for the conflicting tuple before
            // evaluating its WHERE clause. Re-admitting a healthy worker must
            // not consume a pool connection waiting for its guarded batch.
            let existing:Option<bool>=sqlx::query_scalar("SELECT kind=$2 AND state IN ('complete','failed') AND available_at<=clock_timestamp() FROM background_work WHERE id=$1")
                .bind(&id).bind(&kind).fetch_optional(&mut **tx).await?;
            match existing {
                Some(false) => {},
                Some(true) => {
                    sqlx::query("UPDATE background_work w SET state='queued',finished_at=NULL,error_code=NULL FROM (SELECT id FROM background_work WHERE id=$1 AND kind=$2 AND state IN ('complete','failed') AND available_at<=clock_timestamp() FOR UPDATE SKIP LOCKED) due WHERE w.id=due.id")
                        .bind(&id).bind(&kind).execute(&mut **tx).await?;
                },
                None => {
                    sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING")
                        .bind(&id).bind(&kind).bind(payload).execute(&mut **tx).await?;
                },
            }
            Ok(())
        })
    }).await
}

/// Admit a fixed set of recurring lanes in one transaction. As with `ensure`,
/// live owners are read without waiting and completed rows use SKIP LOCKED.
pub async fn ensure_many(pool: &PgPool, keys: &[String], kind: &str, payload: Value) -> Result<()> {
    let ids: Vec<String> = keys
        .iter()
        .map(|key| format!("maintenance:{key}"))
        .collect();
    let kind = kind.to_owned();
    db::write(pool,move |tx| {
        let (ids,kind,payload)=(ids.clone(),kind.clone(),payload.clone());
        Box::pin(async move {
            sqlx::query("WITH due AS MATERIALIZED (SELECT id FROM background_work WHERE id=ANY($1::text[]) AND kind=$2 AND state IN ('complete','failed') AND available_at<=clock_timestamp() ORDER BY id FOR UPDATE SKIP LOCKED), updated AS (UPDATE background_work w SET state='queued',finished_at=NULL,error_code=NULL FROM due WHERE w.id=due.id) INSERT INTO background_work(id,kind,payload) SELECT candidate.id,$2,$3 FROM unnest($1::text[]) AS candidate(id) WHERE NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id=candidate.id) ON CONFLICT(id) DO NOTHING")
                .bind(&ids).bind(&kind).bind(&payload).execute(&mut **tx).await?;
            Ok::<_,anyhow::Error>(())
        })
    }).await
}

pub async fn claim(pool: &PgPool, kind: &str, owner: &str) -> Result<Option<Work>> {
    claim_selected(pool, kind, owner, None).await
}

/// Claim an exact durable work without consuming another operation's candidate.
/// Uses the same resource acquisition, exclusion and takeover rules as `claim`.
pub async fn claim_id(pool: &PgPool, kind: &str, owner: &str, id: &str) -> Result<Option<Work>> {
    claim_selected(pool, kind, owner, Some(id.to_owned())).await
}

async fn claim_selected(
    pool: &PgPool,
    kind: &str,
    owner: &str,
    id: Option<String>,
) -> Result<Option<Work>> {
    let (kind, owner) = (kind.to_owned(), owner.to_owned());
    db::write(pool,move |tx| {
        let (kind,owner,id)=(kind.clone(),owner.clone(),id.clone());
        Box::pin(async move {
            let now:chrono::DateTime<chrono::Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut **tx).await?;
            let rows=sqlx::query("SELECT id,payload FROM background_work w WHERE kind=$1 AND ($3::text IS NULL OR w.id=$3) AND cycle_id IS NULL AND node_id IS NULL AND state IN ('queued','running') AND available_at<=$2 AND NOT EXISTS(SELECT 1 FROM resource_leases l WHERE l.resource_key=ANY(ARRAY['work:'||w.id] || ARRAY(SELECT jsonb_array_elements_text(coalesce(w.payload->'lease_resources','[]'::jsonb)))) AND (l.blocked OR (l.owner IS NOT NULL AND l.expires_at>clock_timestamp()) OR EXISTS(SELECT 1 FROM execution_operations o WHERE o.resource_key=l.resource_key AND o.state IN ('dispatching','reconciling','needs_attention')))) ORDER BY available_at,id LIMIT 32 FOR UPDATE OF w SKIP LOCKED")
                .bind(&kind).bind(now).bind(id).fetch_all(&mut **tx).await?;
            for row in rows {
                let id:String=row.get("id");
                let payload:Value=row.get("payload");
                let mut resources:Vec<String>=serde_json::from_value(payload.get("lease_resources").cloned().unwrap_or_else(||serde_json::json!([])))?;
                anyhow::ensure!(resources.len()<=8 && resources.iter().all(|key|["node:","ca:","identity:"].iter().any(|prefix|key.starts_with(prefix) && key.len()>prefix.len()) && key.len()<=256),"unsupported maintenance resource scope");
                resources.push(format!("work:{id}"));resources.sort();resources.dedup();
                sqlx::query("SAVEPOINT maintenance_candidate").execute(&mut **tx).await?;
                let mut leases=Vec::new();
                for resource in &resources {
                    if let Some(lease)=execution::acquire(tx,resource,&owner,&id).await? {leases.push(lease);} else {break;}
                }
                if leases.len()!=resources.len() {
                    sqlx::query("ROLLBACK TO SAVEPOINT maintenance_candidate").execute(&mut **tx).await?;
                    sqlx::query("RELEASE SAVEPOINT maintenance_candidate").execute(&mut **tx).await?;
                    continue;
                }
                let generation=leases.iter().find(|lease|lease.resource==format!("work:{id}")).context("missing work lease")?.generation;
                sqlx::query("UPDATE background_work SET state='running',owner=$2,generation=$3,started_at=clock_timestamp(),finished_at=NULL WHERE id=$1")
                    .bind(&id).bind(&owner).bind(generation).execute(&mut **tx).await?;
                sqlx::query("RELEASE SAVEPOINT maintenance_candidate").execute(&mut **tx).await?;
                return Ok(Some(Work{id,payload,leases}));
            }
            Ok(None)
        })
    }).await
}

impl Work {
    fn primary(&self) -> Result<&Execution> {
        self.leases
            .iter()
            .find(|lease| lease.resource == format!("work:{}", self.id))
            .context("missing maintenance work lease")
    }
    pub async fn guard_on(&self, tx: &mut Transaction<'static, Postgres>) -> Result<()> {
        for lease in &self.leases {
            lease.guard(tx).await?;
        }
        let lease = self.primary()?;
        let own:Option<bool>=sqlx::query_scalar("SELECT true FROM background_work WHERE id=$1 AND owner=$2 AND generation=$3 AND state='running' FOR UPDATE")
            .bind(&self.id).bind(&lease.owner).bind(lease.generation).fetch_optional(&mut **tx).await?;
        anyhow::ensure!(own == Some(true), "maintenance ownership was lost");
        Ok(())
    }

    /// Call in the same transaction as the batch's changes. Incomplete work
    /// yields its lease and resumes after a short delay; complete work schedules
    /// its next run using database time, independently of any instance's clock.
    pub async fn finish_on(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        more: bool,
        next_seconds: i64,
        error_code: Option<&str>,
    ) -> Result<()> {
        self.guard_on(tx).await?;
        let lease = self.primary()?;
        let changed=sqlx::query("UPDATE background_work SET state=$4,owner=NULL,finished_at=CASE WHEN $4='complete' THEN clock_timestamp() ELSE NULL END,available_at=clock_timestamp()+make_interval(secs=>$5::double precision),error_code=$6 WHERE id=$1 AND owner=$2 AND generation=$3 AND state='running'")
            .bind(&self.id).bind(&lease.owner).bind(lease.generation).bind(if more{"queued"}else{"complete"}).bind(next_seconds as f64).bind(error_code).execute(&mut **tx).await?.rows_affected();
        anyhow::ensure!(changed == 1, "maintenance ownership was lost");
        for lease in &self.leases {
            lease.release(tx).await?;
        }
        Ok(())
    }
    pub async fn retry(&self, pool: &PgPool) -> Result<()> {
        let work = self.clone();
        db::write(pool, move |tx| {
            let work = work.clone();
            Box::pin(async move {
                work.finish_on(tx, true, 5, Some("maintenance_deferred"))
                    .await
            })
        })
        .await
    }
}

#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "maintenance_scope_tests.rs"]
mod scope_tests;

#[cfg(test)]
#[path = "maintenance_admission_tests.rs"]
mod admission_tests;
