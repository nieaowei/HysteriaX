use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use tokio::sync::Mutex;

use crate::{db, execution, state::AppState};

use super::{maintenance, retention::RetentionCheckpoint};

mod operational;
mod operations;
mod rows;

const PAGE: i64 = 512;
const MAX_PAGES: usize = 16;
const DATA_SHARDS: u8 = 4;
const SLICE: Duration = Duration::from_secs(4);
const MAX_SAMPLING_HEADROOM_MS: i64 = 12_000;
const BEFORE_SLOT_MS: i64 = 1_000;

#[derive(Clone, Copy)]
struct Group {
    key: &'static str,
    tasks: &'static [Task],
    sharded: bool,
    traffic_gate: bool,
    schedule: &'static [usize],
}

#[derive(Clone, Copy)]
enum Task {
    Rows(rows::Rule),
    OnlineSamples,
    TrafficBaselines,
    Publications,
    JobChains,
    KickRequests,
    ExecutionOperations,
    ExecutionRecheckRequests,
    NonSamplingWork,
    ManagementReceipts,
    MtlsOperations,
    CacheTombstones,
    OrphanResourceLeases,
    OrphanJobNodeKickTotals,
    CredentialBatches,
    MtlsTrustCleanup,
    MtlsDeploymentSnapshots,
    MtlsActivations,
    ConfigVersions,
}

const GROUPS: [Group; 5] = [
    Group {
        key: "traffic-detail-retention",
        tasks: &[Task::Rows(rows::ITEMS)],
        sharded: true,
        traffic_gate: true,
        schedule: &[0],
    },
    Group {
        key: "traffic-history-retention",
        tasks: &[
            Task::Rows(rows::RECORDS),
            Task::Rows(rows::NETWORK),
            Task::Rows(rows::BATCHES),
            Task::Rows(rows::BATCHES_DISCARDED),
            Task::Rows(rows::GAPS),
            Task::TrafficBaselines,
        ],
        sharded: true,
        traffic_gate: true,
        // Ledger pages are larger. Give network history two turns per sweep.
        schedule: &[0, 0, 1, 0, 0, 1, 0, 0, 2, 3, 4, 5],
    },
    Group {
        key: "monitoring-history-retention",
        tasks: &[Task::OnlineSamples, Task::Rows(rows::PROXY_PROBES)],
        sharded: true,
        traffic_gate: false,
        schedule: &[0, 1],
    },
    Group {
        key: "operations-history-retention",
        tasks: &[
            Task::Rows(rows::INSTANCES),
            Task::Rows(rows::DEPLOYMENT_TOKENS),
            Task::Rows(rows::MONITORING_TOKENS),
            Task::Publications,
            Task::Rows(rows::JOB_EVENTS),
            Task::KickRequests,
            Task::ExecutionOperations,
            Task::ExecutionRecheckRequests,
            Task::JobChains,
            Task::NonSamplingWork,
            Task::Rows(rows::AUDIT),
        ],
        sharded: false,
        traffic_gate: false,
        schedule: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
    },
    Group {
        key: "metadata-history-retention",
        tasks: &[
            Task::ManagementReceipts,
            Task::MtlsOperations,
            Task::CacheTombstones,
            Task::OrphanResourceLeases,
            Task::OrphanJobNodeKickTotals,
            Task::Rows(rows::USER_CREDENTIALS_REVOKED),
            Task::Rows(rows::CREDENTIAL_APP_RECEIPTS),
            Task::CredentialBatches,
            Task::Rows(rows::DNS_OPERATIONS),
            Task::Rows(rows::DNS_RECORDS_DELETED),
            Task::Rows(rows::MTLS_NODE_OPERATIONS),
            Task::Rows(rows::MTLS_ROTATIONS),
            Task::MtlsTrustCleanup,
            Task::MtlsDeploymentSnapshots,
            Task::MtlsActivations,
            Task::ConfigVersions,
        ],
        sharded: false,
        traffic_gate: false,
        schedule: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    },
];

impl Task {
    fn table(self) -> &'static str {
        match self {
            Self::Rows(rule) => rule.table,
            Self::OnlineSamples => "online_samples",
            Self::TrafficBaselines => "traffic_baselines",
            Self::Publications => "event_publications",
            Self::JobChains => "jobs",
            Self::KickRequests => "kick_requests",
            Self::ExecutionOperations => "execution_operations",
            Self::ExecutionRecheckRequests => "execution_recheck_requests",
            Self::NonSamplingWork => "background_work",
            Self::ManagementReceipts => "management_receipts",
            Self::MtlsOperations => "mtls_operations",
            Self::CacheTombstones => "cache_entity_versions",
            Self::OrphanResourceLeases => "resource_leases",
            Self::OrphanJobNodeKickTotals => "job_node_kick_totals",
            Self::CredentialBatches => "credential_batches",
            Self::MtlsTrustCleanup => "mtls_trust_cleanup",
            Self::MtlsDeploymentSnapshots => "mtls_deployment_snapshots",
            Self::MtlsActivations => "mtls_activations",
            Self::ConfigVersions => "config_versions",
        }
    }

    fn days(self) -> i64 {
        match self {
            Self::Rows(rule) => rule.days,
            Self::OnlineSamples => 31,
            Self::TrafficBaselines => 60,
            Self::Publications
            | Self::KickRequests
            | Self::NonSamplingWork
            | Self::ManagementReceipts
            | Self::MtlsOperations
            | Self::CacheTombstones => 30,
            Self::OrphanResourceLeases | Self::OrphanJobNodeKickTotals => 0,
            Self::JobChains
            | Self::ExecutionOperations
            | Self::ExecutionRecheckRequests
            | Self::CredentialBatches
            | Self::MtlsTrustCleanup
            | Self::MtlsDeploymentSnapshots
            | Self::MtlsActivations
            | Self::ConfigVersions => 90,
        }
    }

    async fn page(
        self,
        connection: &mut PgConnection,
        cursor: &mut Cursor,
        now: DateTime<Utc>,
        shard: Option<u8>,
        batch_cutoff: Option<DateTime<Utc>>,
    ) -> Result<Counts> {
        let cutoff = now - TimeDelta::days(self.days());
        match self {
            Self::Rows(rule) => {
                rows::page(connection, rule, cursor, cutoff, shard, batch_cutoff).await
            }
            Self::OnlineSamples => {
                operations::online_samples(connection, cursor, cutoff, shard).await
            }
            Self::TrafficBaselines => {
                operational::traffic_baselines(connection, cursor, cutoff, shard).await
            }
            Self::Publications => operations::publications(connection, cursor, cutoff).await,
            Self::JobChains => operations::job_chain(connection, cursor, cutoff).await,
            Self::KickRequests => operational::kick_requests(connection, cursor, cutoff).await,
            Self::ExecutionOperations => {
                operational::execution_operations(connection, cursor, cutoff).await
            }
            Self::ExecutionRecheckRequests => {
                operational::execution_recheck_requests(connection, cursor, cutoff).await
            }
            Self::NonSamplingWork => {
                operational::non_sampling_work(connection, cursor, cutoff).await
            }
            Self::ManagementReceipts => {
                operational::management_receipts(connection, cursor, cutoff).await
            }
            Self::MtlsOperations => operational::mtls_operations(connection, cursor, cutoff).await,
            Self::CacheTombstones => {
                operational::cache_tombstones(connection, cursor, cutoff).await
            }
            Self::OrphanResourceLeases => {
                operational::orphan_resource_leases(connection, cursor, now).await
            }
            Self::OrphanJobNodeKickTotals => {
                operational::orphan_job_node_kick_totals(connection, cursor, now).await
            }
            Self::CredentialBatches => {
                operational::credential_batches(connection, cursor, cutoff).await
            }
            Self::MtlsTrustCleanup => {
                operational::mtls_trust_cleanup(connection, cursor, cutoff).await
            }
            Self::MtlsDeploymentSnapshots => {
                operational::mtls_deployment_snapshots(connection, cursor, cutoff).await
            }
            Self::MtlsActivations => {
                operational::mtls_activations(connection, cursor, cutoff).await
            }
            Self::ConfigVersions => operational::config_versions(connection, cursor, cutoff).await,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    after: Option<(DateTime<Utc>, String)>,
    #[serde(default)]
    sample: Option<String>,
    #[serde(default)]
    extra: Vec<String>,
    done: bool,
}

type Candidate = (DateTime<Utc>, String);

#[derive(Default)]
struct Counts {
    scanned: usize,
    eligible: usize,
    deleted: u64,
    oldest: Option<DateTime<Utc>>,
    skipped: BTreeMap<String, usize>,
    locked_batches: Vec<String>,
}

impl Counts {
    fn skip(&mut self, reason: &str, count: usize) {
        if count > 0 {
            *self.skipped.entry(reason.to_owned()).or_default() += count;
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Sweep {
    #[serde(rename = "mode")]
    checkpoint: RetentionCheckpoint,
    now: DateTime<Utc>,
    next: usize,
    cursors: Vec<Cursor>,
    #[serde(default)]
    busy: bool,
}

pub(crate) async fn run(state: AppState) {
    let mut workers = tokio::task::JoinSet::new();
    let page_gates: Arc<[Mutex<()>; DATA_SHARDS as usize]> =
        Arc::new(std::array::from_fn(|_| Mutex::new(())));
    for group in GROUPS {
        let lanes = if group.sharded { DATA_SHARDS } else { 1 };
        for lane in 0..lanes {
            let state = state.clone();
            let shard = group.sharded.then_some(lane);
            let page_gates = page_gates.clone();
            workers.spawn(async move {
                let page_gate = shard
                    .filter(|_| group.traffic_gate)
                    .map(|shard| &page_gates[usize::from(shard)]);
                run_group(&state, group, shard, page_gate).await
            });
        }
    }
    while let Some(result) = workers.join_next().await {
        if let Err(error) = result {
            tracing::error!(%error, "history retention worker stopped unexpectedly");
        }
    }
}

async fn run_group(
    state: &AppState,
    group: Group,
    shard: Option<u8>,
    page_gate: Option<&Mutex<()>>,
) {
    let key = match shard {
        Some(shard) => format!("{}:{shard}", group.key),
        None => group.key.to_owned(),
    };
    let mut shutdown = state.shutdown.subscribe();
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tracing::info!(kind = group.key, ?shard, "history retention started");
    let mut consecutive_failures: u64 = 0;
    loop {
        tokio::select! {
            biased;
            _ = shutdown.wait_for(|stop| *stop) => return,
            _ = ticks.tick() => {}
        }
        if group.sharded && !sampling_headroom().is_zero() {
            continue;
        }
        match state
            .maintenance
            .run(
                &[&key],
                &[&key],
                pass_lane(state, group, &key, shard, page_gate),
            )
            .await
        {
            Ok(_) => consecutive_failures = 0,
            Err(error) => {
                consecutive_failures += 1;
                let sqlstate = error
                    .downcast_ref::<sqlx::Error>()
                    .and_then(sqlx::Error::as_database_error)
                    .and_then(|error| error.code());
                tracing::warn!(
                    kind = group.key,
                    ?sqlstate,
                    consecutive_failures,
                    "history retention deferred"
                );
            }
        }
    }
}

fn sampling_headroom() -> Duration {
    sampling_headroom_at(
        Utc::now().timestamp_millis(),
        crate::runtime_config::sampling_interval_seconds(),
    )
}

fn sampling_headroom_at(now_ms: i64, sampling_seconds: u64) -> Duration {
    // Proxy sampling always runs on a one-minute slot. Traffic and network
    // share the configured slot. Reserve the start of either slot for claims.
    let mut wait = 0;
    for seconds in [60, sampling_seconds] {
        let period = seconds as i64 * 1_000;
        let headroom = (period / 5).min(MAX_SAMPLING_HEADROOM_MS);
        let phase = now_ms.rem_euclid(period);
        if phase < headroom {
            wait = wait.max(headroom - phase);
        } else if period - phase <= BEFORE_SLOT_MS.min(period / 10) {
            wait = wait.max(period - phase + headroom);
        }
    }
    Duration::from_millis(wait as u64)
}

async fn wait_for_sampling(state: &AppState) {
    let mut shutdown = state.shutdown.subscribe();
    loop {
        let delay = sampling_headroom();
        if delay.is_zero() || *shutdown.borrow() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown.wait_for(|stop| *stop) => return,
        }
    }
}

#[cfg(test)]
async fn pass(state: &AppState, group: Group) -> Result<()> {
    pass_lane(state, group, group.key, None, None).await
}

async fn pass_lane(
    state: &AppState,
    group: Group,
    key: &str,
    shard: Option<u8>,
    page_gate: Option<&Mutex<()>>,
) -> Result<()> {
    if *state.shutdown.borrow() {
        return Ok(());
    }
    maintenance::ensure(&state.pool, key, key, serde_json::Value::Null).await?;
    let Some(work) = maintenance::claim(&state.pool, key, &state.instance_id).await? else {
        return Ok(());
    };
    execution::supervise(state.pool.clone(), work.leases.clone(), async {
        let result = run_claim(state, group, &work, shard, page_gate).await;
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

async fn run_claim(
    state: &AppState,
    group: Group,
    work: &maintenance::Work,
    shard: Option<u8>,
    page_gate: Option<&Mutex<()>>,
) -> Result<()> {
    let start = Instant::now();
    let mut payload = work.payload.clone();
    for index in 0..MAX_PAGES {
        // Detail and ledger sweeps can reach the same node at the same time.
        // Let only one of them open a page transaction per shard.
        let page_guard = match page_gate {
            Some(gate) => Some(gate.lock().await),
            None => None,
        };
        if shard.is_some() {
            wait_for_sampling(state).await;
        }
        let (next, task, counts, retired_batches, stop) = db::write(&state.pool, |tx| {
            let payload = payload.clone();
            let work = work.clone();
            let shutdown = state.shutdown.clone();
            Box::pin(async move {
                work.guard_on(tx).await?;
                sqlx::query("SET LOCAL statement_timeout = '4s'")
                    .execute(&mut **tx)
                    .await?;
                let previous: Option<Sweep> = serde_json::from_value(payload)?;
                let mut sweep = match previous {
                    Some(sweep)
                        if matches!(sweep.checkpoint, RetentionCheckpoint::Delete)
                            && sweep.cursors.len() == group.tasks.len()
                            && sweep.next < group.schedule.len() =>
                    {
                        sweep
                    }
                    _ => Sweep {
                        checkpoint: RetentionCheckpoint::Delete,
                        now: sqlx::query_scalar("SELECT clock_timestamp()")
                            .fetch_one(&mut **tx)
                            .await?,
                        next: 0,
                        cursors: group.tasks.iter().map(|_| Cursor::default()).collect(),
                        busy: false,
                    },
                };
                let slot = (0..group.schedule.len())
                    .map(|offset| (sweep.next + offset) % group.schedule.len())
                    .find(|slot| !sweep.cursors[group.schedule[*slot]].done)
                    .context("history retention sweep has no active task")?;
                let active = group.schedule[slot];
                let task = group.tasks[active];
                let batch_cutoff = sweep.now - TimeDelta::days(rows::BATCHES.days);
                let touches_batch =
                    matches!(task.table(), "traffic_records" | "traffic_sample_items");
                let retire_cutoff = touches_batch.then_some(batch_cutoff);
                let counts = if *shutdown.borrow() {
                    Counts::default()
                } else {
                    task.page(
                        tx,
                        &mut sweep.cursors[active],
                        sweep.now,
                        shard,
                        retire_cutoff,
                    )
                    .await?
                };
                sweep.next = (slot + 1) % group.schedule.len();
                sweep.busy |= !sweep.cursors[active].done && counts.scanned > 0;
                let complete = sweep.cursors.iter().all(|cursor| cursor.done);
                let retry_after = match (complete, group.sharded && sweep.busy) {
                    (false, _) => 1,
                    (true, true) => 60,
                    (true, false) => 300,
                };
                let next = if complete {
                    serde_json::Value::Null
                } else {
                    serde_json::to_value(sweep)?
                };
                let batches = if counts.deleted > 0 {
                    counts.locked_batches.as_slice()
                } else {
                    &[]
                };
                let retired_batches =
                    rows::checkpoint(tx, &work.id, &next, batch_cutoff, batches).await?;
                let stop = complete
                    || index + 1 == MAX_PAGES
                    || start.elapsed() >= SLICE
                    || *shutdown.borrow();
                if stop {
                    work.finish_on(tx, !complete, retry_after, None).await?;
                }
                Ok::<_, anyhow::Error>((next, task, counts, retired_batches, stop))
            })
        })
        .await?;
        drop(page_guard);
        payload = next;
        tracing::info!(kind = group.key, table = task.table(), ?shard, scanned = counts.scanned,
            eligible = counts.eligible, deleted = counts.deleted, oldest = ?counts.oldest,
            skipped = ?counts.skipped, elapsed_ms = start.elapsed().as_millis(), "history retention page committed");
        if retired_batches > 0 {
            tracing::info!(
                kind = group.key,
                table = rows::BATCHES.table,
                ?shard,
                deleted = retired_batches,
                "history retention page committed"
            );
        }
        if stop {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod operational_tests;
#[cfg(test)]
mod tests;
