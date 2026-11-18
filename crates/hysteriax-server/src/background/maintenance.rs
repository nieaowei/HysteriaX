//! Durable scheduling for bounded, database-only maintenance batches.
//! Remote and node-owned work must use their additional resource leases.
use crate::{
    db,
    execution::{self, Execution},
};
use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};

#[path = "maintenance_observations.rs"]
mod observations;
pub(crate) use observations::{Poll, run_summaries};

#[path = "maintenance_scheduler.rs"]
mod scheduler;
pub use scheduler::{Demand, Scheduler};

#[derive(Clone, Debug)]
pub struct Work {
    pub id: String,
    pub payload: Value,
    pub leases: Vec<Execution>,
}

/// A recurring key has one durable work row and one monotonically increasing
/// work lease. A live running owner is never reset by another scheduler.
pub async fn ensure(pool: &PgPool, key: &str, kind: &str, payload: Value) -> Result<()> {
    ensure_many(pool, &[key.to_owned()], kind, payload).await
}

/// Admit a fixed set of recurring lanes in one transaction. As with `ensure`,
/// live owners are read without waiting and completed rows use SKIP LOCKED.
pub async fn ensure_many(pool: &PgPool, keys: &[String], kind: &str, payload: Value) -> Result<()> {
    ensure_selected(pool, keys, kind, payload, None).await
}

/// Readiness is only a hint: the trusted business SQL is rechecked while the
/// completed scanner row is locked. Never reset a running owner or queued retry.
pub async fn ensure_urgent(pool: &PgPool, demand: Demand, payload: Value) -> Result<()> {
    ensure_selected(
        pool,
        &[demand.key.to_owned()],
        demand.kind,
        payload,
        Some(demand.predicate),
    )
    .await
}

async fn ensure_selected(
    pool: &PgPool,
    keys: &[String],
    kind: &str,
    payload: Value,
    urgency: Option<&'static str>,
) -> Result<()> {
    let ids: Vec<String> = keys
        .iter()
        .map(|key| format!("maintenance:{key}"))
        .collect();
    let mut poll = observations::Poll::new(kind, "ensure");
    if scheduler::skip_ensure(pool, &ids) {
        poll.coalesced_empty();
        poll.finish(true, false);
        return Ok(());
    }
    let result = ensure_due(pool, ids, kind, payload, &poll, urgency).await;
    poll.finish(result.is_ok(), false);
    if result.is_ok() {
        scheduler::ensured(pool, kind);
    }
    result
}

async fn ensure_due(
    pool: &PgPool,
    ids: Vec<String>,
    kind: &str,
    payload: Value,
    poll: &observations::Poll,
    urgency: Option<&'static str>,
) -> Result<()> {
    let urgent = urgency.unwrap_or("false");
    let eligible = format!(
        "((w.state IN ('complete','failed') AND w.available_at<=(SELECT now FROM cutoff)) OR (w.state='complete' AND ({urgent})))"
    );
    let mut connection = pool.acquire().await?;
    if !execution::has_scope() {
        // This is only a hint. The mutation below rechecks due state after
        // locking; a concurrent claimant cannot have its ownership reset.
        let needed: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("WITH cutoff AS MATERIALIZED (SELECT clock_timestamp() AS now) SELECT EXISTS(SELECT 1 FROM unnest($1::text[]) candidate(id) LEFT JOIN background_work w ON w.id=candidate.id WHERE w.id IS NULL OR (w.kind=$2 AND {eligible}))")))
            .bind(&ids).bind(kind).fetch_one(&mut *connection).await?;
        if !needed {
            poll.preflight_empty();
            return Ok(());
        }
        #[cfg(test)]
        poll_tests::after_preflight("ensure").await;
    }
    poll.transaction();
    let kind = kind.to_owned();
    db::write_after_probe(pool,connection,move |tx| {
        let (ids,kind,payload,eligible)=(ids.clone(),kind.clone(),payload.clone(),eligible.clone());
        Box::pin(async move {
            sqlx::query(sqlx::AssertSqlSafe(format!("WITH cutoff AS MATERIALIZED (SELECT clock_timestamp() AS now), due AS MATERIALIZED (SELECT w.id FROM background_work w WHERE w.id=ANY($1::text[]) AND w.kind=$2 AND {eligible} ORDER BY w.id FOR UPDATE OF w SKIP LOCKED), updated AS (UPDATE background_work w SET state='queued',available_at=LEAST(w.available_at,(SELECT now FROM cutoff)),finished_at=NULL,error_code=NULL FROM due WHERE w.id=due.id) INSERT INTO background_work(id,kind,payload) SELECT candidate.id,$2,$3 FROM unnest($1::text[]) AS candidate(id) WHERE NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id=candidate.id) ON CONFLICT(id) DO NOTHING")))
                .bind(&ids).bind(&kind).bind(&payload).execute(&mut **tx).await?;
            Ok::<_,anyhow::Error>(())
        })
    }).await
}

/// Both queries use the same exclusion rules. The unlocked query is a hint;
/// only the bounded, locked query inside the transaction grants a candidate.
fn candidates_sql(locked: bool) -> String {
    let (columns, tail) = if locked {
        (
            "id,payload",
            "ORDER BY available_at,id LIMIT 32 FOR UPDATE OF w SKIP LOCKED",
        )
    } else {
        ("1", "LIMIT 1")
    };
    format!(
        "WITH cutoff AS MATERIALIZED (SELECT clock_timestamp() AS now) SELECT {columns} FROM background_work w WHERE {} {tail}",
        candidate_filter("$1", "$2::text IS NULL OR w.id=$2")
    )
}

fn candidate_filter(kind: &str, id: &str) -> String {
    format!(
        "w.kind={kind} AND ({id}) AND w.cycle_id IS NULL AND w.node_id IS NULL AND w.state IN ('queued','running') AND w.available_at<=(SELECT now FROM cutoff) AND NOT EXISTS(SELECT 1 FROM resource_leases l WHERE l.resource_key=ANY(ARRAY['work:'||w.id] || ARRAY(SELECT jsonb_array_elements_text(coalesce(w.payload->'lease_resources','[]'::jsonb)))) AND (l.blocked OR (l.owner IS NOT NULL AND l.expires_at>clock_timestamp()) OR EXISTS(SELECT 1 FROM execution_operations o WHERE o.resource_key=l.resource_key AND o.state IN ('dispatching','reconciling','needs_attention'))))"
    )
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
    let mut poll = observations::Poll::new(kind, "claim");
    if scheduler::skip_claim(pool, kind) {
        poll.coalesced_empty();
        poll.finish(true, false);
        return Ok(None);
    }
    let result = claim_due(pool, kind, owner, id, &poll).await;
    poll.finish(
        result.is_ok(),
        result.as_ref().is_ok_and(|work| work.is_some()),
    );
    result
}

#[cfg(test)]
pub(crate) fn poll_summaries() {
    observations::summaries();
}

async fn claim_due(
    pool: &PgPool,
    kind: &str,
    owner: &str,
    id: Option<String>,
    poll: &observations::Poll,
) -> Result<Option<Work>> {
    let mut connection = pool.acquire().await?;
    if !execution::has_scope() {
        let eligible = sqlx::query(sqlx::AssertSqlSafe(candidates_sql(false)))
            .bind(kind)
            .bind(id.as_deref())
            .fetch_optional(&mut *connection)
            .await?
            .is_some();
        if !eligible {
            poll.preflight_empty();
            return Ok(None);
        }
        #[cfg(test)]
        poll_tests::after_preflight("claim").await;
    }
    poll.transaction();
    let (kind, owner) = (kind.to_owned(), owner.to_owned());
    db::write_after_probe(pool,connection,move |tx| {
        let (kind,owner,id)=(kind.clone(),owner.clone(),id.clone());
        Box::pin(async move {
            let rows=sqlx::query(sqlx::AssertSqlSafe(candidates_sql(true)))
                .bind(&kind).bind(id).fetch_all(&mut **tx).await?;
            for row in rows {
                let id:String=row.get("id");
                let payload:Value=row.get("payload");
                let mut resources:Vec<String>=serde_json::from_value(payload.get("lease_resources").cloned().unwrap_or_else(||serde_json::json!([])))?;
                anyhow::ensure!(resources.len()<=8 && resources.iter().all(|key|["node:","ca:","identity:"].iter().any(|prefix|key.starts_with(prefix) && key.len()>prefix.len()) && key.len()<=256),"unsupported maintenance resource scope");
                resources.push(format!("work:{id}"));resources.sort();resources.dedup();
                sqlx::query("SAVEPOINT maintenance_candidate").execute(&mut **tx).await?;
                let Some(leases)=execution::acquire_all(tx,&resources,&owner,&id).await? else {
                    sqlx::query("ROLLBACK TO SAVEPOINT maintenance_candidate").execute(&mut **tx).await?;
                    sqlx::query("RELEASE SAVEPOINT maintenance_candidate").execute(&mut **tx).await?;
                    continue;
                };
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
        execution::guard_all(tx, &self.leases).await?;
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
        execution::require_finalization_attempt(tx)?;
        self.guard_on(tx).await?;
        let lease = self.primary()?;
        let changed=sqlx::query("UPDATE background_work SET state=$4,owner=NULL,finished_at=CASE WHEN $4='complete' THEN clock_timestamp() ELSE NULL END,available_at=clock_timestamp()+make_interval(secs=>$5::double precision),error_code=$6 WHERE id=$1 AND owner=$2 AND generation=$3 AND state='running'")
            .bind(&self.id).bind(&lease.owner).bind(lease.generation).bind(if more{"queued"}else{"complete"}).bind(next_seconds as f64).bind(error_code).execute(&mut **tx).await?.rows_affected();
        anyhow::ensure!(changed == 1, "maintenance ownership was lost");
        for lease in &self.leases {
            anyhow::ensure!(
                lease.release(tx).await?,
                "maintenance resource has an unverified remote operation"
            );
        }
        execution::finish_current_on(
            tx,
            &format!("work:{}", self.id),
            if more {
                execution::Disposition::Yielded
            } else {
                execution::Disposition::Complete
            },
        )?;
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

#[cfg(test)]
#[path = "maintenance_poll_tests.rs"]
mod poll_tests;

#[cfg(test)]
pub(super) async fn sampling_preflight() {
    poll_tests::after_preflight("sampling-claim").await;
}
#[cfg(test)]
pub(super) use poll_tests::{PREFLIGHT_PAUSE, Statements, recorded};
