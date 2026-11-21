//! Retire sampling identities only after the execution and billing boundaries
//! are closed. A sweep is resumable across claims and service instances.
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use super::maintenance;
use crate::{db, execution, state::AppState};

const KIND: &str = "sampling-retention";
const KEY: &str = "sampling-history:retention";
const PAGE: usize = 512;
const MAX_BATCHES: usize = 16;
const SLICE: Duration = Duration::from_secs(4);
const IDLE_SECONDS: i64 = 300;

const WORK_PAGE_LOCKED: &str = concat!(
    "WITH scanned AS MATERIALIZED (",
    include_str!("retention_work_page.sql"),
    "),
    locked_work AS MATERIALIZED (
        SELECT w.id FROM scanned s CROSS JOIN LATERAL (
            SELECT id FROM background_work WHERE id = s.id FOR UPDATE SKIP LOCKED
        ) w
    ),
    locked_leases AS MATERIALIZED (
        SELECT w.id FROM locked_work w CROSS JOIN LATERAL (
            SELECT 1 FROM resource_leases WHERE resource_key = 'work:' || w.id
            FOR UPDATE SKIP LOCKED
        ) l
    ),
    locked_batches AS MATERIALIZED (
        SELECT w.id FROM locked_work w CROSS JOIN LATERAL (
            SELECT 1 FROM traffic_sample_batches WHERE sample_id = w.id FOR UPDATE SKIP LOCKED
        ) b
    )
    SELECT s.finished_at, s.id, w.id IS NOT NULL AS work_locked,
        l.id IS NOT NULL AS lease_locked, b.id IS NOT NULL AS batch_locked
    FROM scanned s
    LEFT JOIN locked_work w USING (id)
    LEFT JOIN locked_leases l USING (id)
    LEFT JOIN locked_batches b USING (id)
    ORDER BY s.finished_at, s.id"
);

const CLASSIFIED_WORK: &str = concat!(
    "WITH classified AS MATERIALIZED (",
    include_str!("retention_work.sql"),
    "),
    eligible AS MATERIALIZED (SELECT * FROM classified WHERE skip_reason IS NULL),
    skips AS (
        SELECT skip_reason, count(*) AS n FROM classified WHERE skip_reason IS NOT NULL GROUP BY skip_reason
    )"
);

const WORK_COUNTS: &str = "
    SELECT (SELECT count(*) FROM eligible) AS eligible,
        (SELECT count(resource_key) FROM eligible) AS eligible_leases,
        (SELECT count(*) FROM eligible WHERE discard_batch) AS eligible_batches,
        coalesce((SELECT jsonb_object_agg(skip_reason,n) FROM skips),'{}') AS skipped, ";

const WORK_DELETE: &str = ",
    batches AS (
        DELETE FROM traffic_sample_batches b USING eligible e
        WHERE b.sample_id = e.id AND e.discard_batch RETURNING 1
    ),
    leases AS (
        DELETE FROM resource_leases l USING eligible e
        WHERE l.resource_key = e.resource_key RETURNING 1
    ),
    work AS (
        DELETE FROM background_work w USING eligible e WHERE w.id = e.id RETURNING 1
    )";

#[derive(sqlx::FromRow)]
struct WorkPageEntry {
    finished_at: DateTime<Utc>,
    id: String,
    work_locked: bool,
    lease_locked: bool,
    batch_locked: bool,
}

#[derive(sqlx::FromRow)]
struct WorkPageCounts {
    eligible: i64,
    eligible_leases: i64,
    eligible_batches: i64,
    skipped: sqlx::types::Json<BTreeMap<String, usize>>,
    work_deleted: i64,
    leases_deleted: i64,
    batches_deleted: i64,
}

// Keep the persisted checkpoint format so a running deletion sweep can
// resume across upgrades and older binaries can still read new checkpoints.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum RetentionCheckpoint {
    Delete,
    Observe,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Cursor<T> {
    after: Option<(DateTime<Utc>, T)>,
    done: bool,
}

impl<T: Clone> Cursor<T> {
    fn advance(&mut self, page: &[(DateTime<Utc>, T)]) {
        self.advance_to(page.last().cloned(), page.len());
    }

    fn advance_to(&mut self, after: Option<(DateTime<Utc>, T)>, len: usize) {
        self.after = after;
        self.done = len < PAGE;
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Sweep {
    #[serde(rename = "mode")]
    checkpoint: RetentionCheckpoint,
    cutoff: DateTime<Utc>,
    work: Cursor<String>,
    cycles: Cursor<Uuid>,
    scan_cycles_next: bool,
}

impl Sweep {
    fn new(cutoff: DateTime<Utc>) -> Self {
        Self {
            checkpoint: RetentionCheckpoint::Delete,
            cutoff,
            work: Cursor::default(),
            cycles: Cursor::default(),
            scan_cycles_next: false,
        }
    }

    fn complete(&self) -> bool {
        self.work.done && self.cycles.done
    }
}

#[derive(Default)]
struct Counts {
    scanned: usize,
    eligible: usize,
    work_deleted: u64,
    leases_deleted: u64,
    batches_deleted: u64,
    cycles_deleted: u64,
    skipped: BTreeMap<String, usize>,
}

impl Counts {
    fn skip(&mut self, reason: &str, count: usize) {
        if count > 0 {
            *self.skipped.entry(reason.to_owned()).or_default() += count;
        }
    }

    fn merge(&mut self, other: Self) {
        self.scanned += other.scanned;
        self.eligible += other.eligible;
        self.work_deleted += other.work_deleted;
        self.leases_deleted += other.leases_deleted;
        self.batches_deleted += other.batches_deleted;
        self.cycles_deleted += other.cycles_deleted;
        for (reason, count) in other.skipped {
            self.skip(&reason, count);
        }
    }
}

pub(crate) async fn run(state: AppState) {
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut shutdown = state.shutdown.subscribe();
    tracing::info!("sampling retention worker started");
    loop {
        tokio::select! {
            biased;
            _ = shutdown.wait_for(|stop| *stop) => return,
            _ = ticks.tick() => {}
        }
        if let Err(error) = state.maintenance.run(&[KIND], &[KEY], pass(&state)).await {
            let sqlstate = error
                .downcast_ref::<sqlx::Error>()
                .and_then(sqlx::Error::as_database_error)
                .and_then(|error| error.code());
            tracing::warn!(?sqlstate, "sampling retention deferred");
        }
    }
}

async fn pass(state: &AppState) -> Result<()> {
    if *state.shutdown.borrow() {
        return Ok(());
    }
    maintenance::ensure(&state.pool, KEY, KIND, serde_json::Value::Null).await?;
    let Some(work) = maintenance::claim(&state.pool, KIND, &state.instance_id).await? else {
        return Ok(());
    };
    execution::supervise(state.pool.clone(), work.leases.clone(), async {
        let result = run_claim(state, &work).await;
        if result.is_err() {
            work.retry(&state.pool).await?;
        }
        result
    })
    .await
    .into_result()?
    .transpose()?;
    Ok(())
}

async fn run_claim(state: &AppState, work: &maintenance::Work) -> Result<()> {
    let start = Instant::now();
    let mut sweep = if work.payload.is_null() {
        None
    } else {
        let previous: Sweep = serde_json::from_value(work.payload.clone())?;
        matches!(previous.checkpoint, RetentionCheckpoint::Delete).then_some(previous)
    };
    let mut totals = Counts::default();
    let mut batches = 0;
    let mut more = true;
    let result = async {
        for index in 0..MAX_BATCHES {
            let work = work.clone();
            let previous = sweep.clone();
            let shutdown = state.shutdown.clone();
            let (next, counts, yielded) = db::write(&state.pool, move |tx| {
                let shutdown = shutdown.clone();
                let work = work.clone();
                let previous = previous.clone();
                Box::pin(async move {
                    work.guard_on(tx).await?;
                    let mut sweep = match previous {
                        Some(previous) => previous,
                        None => {
                            let cutoff =
                                sqlx::query_scalar("SELECT clock_timestamp() - interval '14 days'")
                                    .fetch_one(&mut **tx)
                                    .await?;
                            Sweep::new(cutoff)
                        }
                    };
                    let counts = if *shutdown.borrow() {
                        Counts::default()
                    } else {
                        scan_page(tx, &mut sweep).await?
                    };
                    let complete = sweep.complete();
                    let payload = if complete {
                        serde_json::Value::Null
                    } else {
                        serde_json::to_value(&sweep)?
                    };
                    sqlx::query("UPDATE background_work SET payload = $2 WHERE id = $1")
                        .bind(&work.id)
                        .bind(payload)
                        .execute(&mut **tx)
                        .await?;
                    let stop = complete
                        || index + 1 == MAX_BATCHES
                        || start.elapsed() >= SLICE
                        || *shutdown.borrow();
                    if stop {
                        work.finish_on(
                            tx,
                            !complete,
                            if complete { IDLE_SECONDS } else { 1 },
                            None,
                        )
                        .await?;
                    }
                    Ok::<_, anyhow::Error>((sweep, counts, stop))
                })
            })
            .await?;
            batches += 1;
            totals.merge(counts);
            more = !next.complete();
            sweep = Some(next);
            if yielded {
                break;
            }
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    // Report only committed pages, including those before a failed page.
    tracing::info!(
        batches, more, failed = result.is_err(),
        cutoff = ?sweep.as_ref().map(|sweep| sweep.cutoff),
        elapsed_ms = start.elapsed().as_millis() as u64,
        scanned = totals.scanned, eligible = totals.eligible,
        work_deleted = totals.work_deleted, leases_deleted = totals.leases_deleted,
        batches_deleted = totals.batches_deleted, cycles_deleted = totals.cycles_deleted,
        skipped = ?totals.skipped, "sampling retention claim finished"
    );
    result
}

async fn scan_page(connection: &mut PgConnection, sweep: &mut Sweep) -> Result<Counts> {
    if !sweep.cycles.done && (sweep.work.done || sweep.scan_cycles_next) {
        sweep.scan_cycles_next = false;
        return scan_cycles(connection, sweep.cutoff, &mut sweep.cycles).await;
    }
    sweep.scan_cycles_next = true;
    scan_work(connection, sweep.cutoff, &mut sweep.work).await
}

async fn scan_work(
    connection: &mut PgConnection,
    cutoff: DateTime<Utc>,
    cursor: &mut Cursor<String>,
) -> Result<Counts> {
    let (after_time, after_id) = cursor.after.as_ref().map(|(time, id)| (time, id)).unzip();
    // Limit lock acquisition to indexed point lookups, including in a generic
    // plan where PostgreSQL cannot see the bound page size.
    // Sampling claims lock work first but finalization may lock the related
    // lease first. Skip contested rows and page over the whole scanned range.
    let page: Vec<WorkPageEntry> = sqlx::query_as(WORK_PAGE_LOCKED)
        .bind(cutoff)
        .bind(after_time)
        .bind(after_id)
        .bind(PAGE as i64)
        .fetch_all(&mut *connection)
        .await?;
    cursor.advance_to(
        page.last().map(|row| (row.finished_at, row.id.clone())),
        page.len(),
    );
    let mut counts = Counts {
        scanned: page.len(),
        ..Counts::default()
    };
    if page.is_empty() {
        return Ok(counts);
    }
    let work_ids: Vec<_> = page
        .iter()
        .filter(|row| row.work_locked)
        .map(|row| &row.id)
        .collect();
    let lease_ids: Vec<_> = page
        .iter()
        .filter(|row| row.lease_locked)
        .map(|row| &row.id)
        .collect();
    let batch_ids: Vec<_> = page
        .iter()
        .filter(|row| row.batch_locked)
        .map(|row| &row.id)
        .collect();
    counts.skip("work_locked", counts.scanned - work_ids.len());
    if work_ids.is_empty() {
        return Ok(counts);
    }

    // The second statement sees evidence committed before the locks were
    // acquired. The locked lease and batch also fence new foreign references.
    let sql = format!(
        "{CLASSIFIED_WORK}{WORK_DELETE}{WORK_COUNTS}
        (SELECT count(*) FROM work) AS work_deleted,
        (SELECT count(*) FROM leases) AS leases_deleted,
        (SELECT count(*) FROM batches) AS batches_deleted"
    );
    let result: WorkPageCounts = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(&work_ids)
        .bind(cutoff)
        .bind(&lease_ids)
        .bind(&batch_ids)
        .fetch_one(&mut *connection)
        .await?;
    counts.eligible = result.eligible as usize;
    for (reason, count) in result.skipped.0 {
        counts.skip(&reason, count);
    }
    ensure!(
        result.work_deleted == result.eligible
            && result.leases_deleted == result.eligible_leases
            && result.batches_deleted == result.eligible_batches,
        "sampling work delete count changed"
    );
    counts.work_deleted = result.work_deleted as u64;
    counts.leases_deleted = result.leases_deleted as u64;
    counts.batches_deleted = result.batches_deleted as u64;
    Ok(counts)
}

async fn scan_cycles(
    connection: &mut PgConnection,
    cutoff: DateTime<Utc>,
    cursor: &mut Cursor<Uuid>,
) -> Result<Counts> {
    let (after_time, after_id) = cursor.after.as_ref().map(|(time, id)| (time, id)).unzip();
    let page: Vec<(DateTime<Utc>, Uuid)> = sqlx::query_as(include_str!("retention_cycle_page.sql"))
        .bind(cutoff)
        .bind(after_time)
        .bind(after_id)
        .bind(PAGE as i64)
        .fetch_all(&mut *connection)
        .await?;
    cursor.advance(&page);
    let mut counts = Counts {
        scanned: page.len(),
        ..Counts::default()
    };
    if page.is_empty() {
        return Ok(counts);
    }
    let candidates: Vec<_> = page.into_iter().map(|(_, id)| id).collect();
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM sampling_cycles WHERE id = ANY($1) ORDER BY id FOR UPDATE SKIP LOCKED",
    )
    .bind(&candidates)
    .fetch_all(&mut *connection)
    .await?;
    counts.skip("cycle_locked", counts.scanned - ids.len());
    let rows: Vec<(Uuid, Option<String>)> = sqlx::query_as(include_str!("retention_cycles.sql"))
        .bind(&ids)
        .bind(cutoff)
        .fetch_all(&mut *connection)
        .await?;
    let mut eligible = Vec::new();
    for (id, reason) in rows {
        if let Some(reason) = reason {
            counts.skip(&reason, 1);
        } else {
            eligible.push(id);
        }
    }
    counts.eligible = eligible.len();
    if !eligible.is_empty() {
        counts.cycles_deleted = sqlx::query("DELETE FROM sampling_cycles WHERE id = ANY($1)")
            .bind(&eligible)
            .execute(connection)
            .await?
            .rows_affected();
        ensure!(
            counts.cycles_deleted == eligible.len() as u64,
            "sampling cycle delete count changed"
        );
    }
    Ok(counts)
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
