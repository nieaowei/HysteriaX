//! One shared, short-lived due-work hint per application pool. Hints never grant
//! ownership: the existing admission/claim transactions still recheck all state.
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::Mutex,
    time::Duration,
};

use sqlx::PgPool;
use tokio::{sync::Mutex as AsyncMutex, time::Instant};

use super::{candidate_filter, observations::Poll};

const HINT_LIFETIME: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Demand {
    pub kind: &'static str,
    pub key: &'static str,
    // Trusted module-owned SQL, never external input. A hint, not admission.
    // Time predicates use (SELECT now FROM cutoff), one DB cutoff per snapshot.
    pub predicate: &'static str,
}

#[derive(Default)]
struct Registry {
    kinds: HashSet<String>,
    keys: HashSet<String>,
    generation: u64,
    demands: HashMap<&'static str, Demand>,
    urgencies: HashMap<&'static str, Demand>,
}

#[derive(Clone, Default)]
struct Ready {
    kinds: HashSet<String>,
    keys: HashSet<String>,
}

#[derive(Default)]
struct Snapshot {
    generation: u64,
    expires: Option<Instant>,
    ready: Ready,
}

pub struct Scheduler {
    pool: PgPool,
    registry: Mutex<Registry>,
    snapshot: AsyncMutex<Snapshot>,
}

struct Hint {
    pool: PgPool,
    expires: Mutex<Instant>,
    kinds: HashSet<String>,
    keys: HashSet<String>,
    ready_kinds: Mutex<HashSet<String>>,
    ready_keys: Mutex<HashSet<String>>,
    ensured_kinds: Mutex<HashSet<String>>,
}

tokio::task_local! {
    static HINT: Hint;
}

impl Scheduler {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            registry: Mutex::new(Registry::default()),
            snapshot: AsyncMutex::new(Snapshot::default()),
        }
    }

    /// Register a fixed worker's recurring keys and claim kinds, and run it only
    /// when a shared snapshot contains a due hint. Keys are scheduling categories,
    /// not one registration per user/node. Cross-instance changes are discovered
    /// after at most one hint lifetime plus the worker's next normal tick.
    pub fn run<T>(
        &self,
        kinds: &[&str],
        keys: &[&str],
        operation: impl Future<Output = anyhow::Result<T>>,
    ) -> impl Future<Output = anyhow::Result<Option<T>>> {
        // Box before constructing the scheduler future. Keeping a large worker
        // future inline multiplies its stack footprint across run/task-local
        // scopes and the joined credential loops, even in optimized builds.
        self.run_boxed(kinds, keys, None, None, Box::pin(operation))
    }

    pub fn run_when<T>(
        &self,
        kinds: &[&str],
        keys: &[&str],
        demand: Demand,
        operation: impl Future<Output = anyhow::Result<T>>,
    ) -> impl Future<Output = anyhow::Result<Option<T>>> {
        self.run_boxed(kinds, keys, Some(demand), None, Box::pin(operation))
    }

    /// A durable business predicate can advance a completed recurring scanner.
    /// Ordinary due work still runs; queued retry delays and live owners are kept.
    pub fn run_urgent<T>(
        &self,
        kinds: &[&str],
        keys: &[&str],
        demand: Demand,
        operation: impl Future<Output = anyhow::Result<T>>,
    ) -> impl Future<Output = anyhow::Result<Option<T>>> {
        self.run_boxed(kinds, keys, None, Some(demand), Box::pin(operation))
    }

    /// Refresh only an expired stage hint, sharing another worker's refresh.
    /// Executed ensures stay conservative positives until this pass completes.
    pub async fn refresh_hint(&self) -> Result<(), sqlx::Error> {
        if crate::execution::has_scope() {
            return Ok(());
        }
        let expired = HINT
            .try_with(|hint| {
                same_pool(hint, &self.pool) && Instant::now() >= *hint.expires.lock().unwrap()
            })
            .unwrap_or(false);
        if !expired {
            return Ok(());
        }
        let generation = self.registry.lock().unwrap().generation;
        let (ready, expires) = self.ready(generation).await?;
        let _ = HINT.try_with(|hint| {
            *hint.expires.lock().unwrap() = expires;
            *hint.ready_kinds.lock().unwrap() = ready.kinds;
            *hint.ready_keys.lock().unwrap() = ready.keys;
        });
        Ok(())
    }

    async fn run_boxed<T>(
        &self,
        kinds: &[&str],
        keys: &[&str],
        demand: Option<Demand>,
        urgency: Option<Demand>,
        operation: Pin<Box<impl Future<Output = anyhow::Result<T>>>>,
    ) -> anyhow::Result<Option<T>> {
        if crate::execution::has_scope() {
            return operation.await.map(Some);
        }
        let kinds: HashSet<String> = kinds.iter().map(|s| (*s).to_owned()).collect();
        let keys: HashSet<String> = keys.iter().map(|s| format!("maintenance:{s}")).collect();
        let generation = {
            let mut registry = self.registry.lock().unwrap();
            let mut changed = false;
            for kind in &kinds {
                changed |= registry.kinds.insert(kind.clone());
            }
            for key in &keys {
                changed |= registry.keys.insert(key.clone());
            }
            if let Some(demand) = demand {
                if let Some(existing) = registry.demands.get(demand.kind) {
                    assert_eq!(
                        *existing, demand,
                        "inconsistent maintenance domain predicate"
                    );
                } else {
                    registry.demands.insert(demand.kind, demand);
                    changed = true;
                }
            }
            if let Some(demand) = urgency {
                if let Some(existing) = registry.urgencies.get(demand.kind) {
                    assert_eq!(*existing, demand, "inconsistent maintenance wake predicate");
                } else {
                    registry.urgencies.insert(demand.kind, demand);
                    changed = true;
                }
            }
            if changed {
                registry.generation += 1;
            }
            registry.generation
        };
        let (ready, expires) = self.ready(generation).await?;
        if kinds.is_disjoint(&ready.kinds) && keys.is_disjoint(&ready.keys) {
            let mut poll = Poll::new("maintenance-scheduler", "pass");
            poll.coalesced_empty();
            poll.finish(true, false);
            return Ok(None);
        }
        HINT.scope(
            Hint {
                pool: self.pool.clone(),
                expires: Mutex::new(expires),
                kinds,
                keys,
                ready_kinds: Mutex::new(ready.kinds),
                ready_keys: Mutex::new(ready.keys),
                ensured_kinds: Mutex::new(HashSet::new()),
            },
            async { operation.await.map(Some) },
        )
        .await
    }

    async fn ready(&self, generation: u64) -> Result<(Ready, Instant), sqlx::Error> {
        // The mutex also makes concurrent refreshes single-flight. A cancelled
        // or failed refresh publishes neither a negative result nor a new expiry.
        let mut snapshot = self.snapshot.lock().await;
        if snapshot.generation >= generation
            && snapshot.expires.is_some_and(|end| Instant::now() < end)
        {
            return Ok((snapshot.ready.clone(), snapshot.expires.unwrap()));
        }
        let (kinds, keys, generation, demands, urgencies) = {
            let registry = self.registry.lock().unwrap();
            (
                registry.kinds.iter().cloned().collect::<Vec<_>>(),
                registry.keys.iter().cloned().collect::<Vec<_>>(),
                registry.generation,
                registry.demands.values().copied().collect::<Vec<_>>(),
                registry.urgencies.values().copied().collect::<Vec<_>>(),
            )
        };
        let condition = |column: &str, key: bool| {
            if demands.is_empty() {
                return "true".to_owned();
            }
            let mut sql = format!("CASE {column}");
            for d in &demands {
                let name = if key {
                    format!("maintenance:{}", d.key)
                } else {
                    d.kind.to_owned()
                };
                sql.push_str(&format!(
                    " WHEN '{}' THEN ({})",
                    name.replace('\'', "''"),
                    d.predicate
                ));
            }
            sql.push_str(" ELSE true END");
            sql
        };
        let kind_demand = condition("k.kind", false);
        let key_demand = condition("k.id", true);
        let mut urgent = "CASE k.id".to_owned();
        for demand in &urgencies {
            urgent.push_str(&format!(
                " WHEN 'maintenance:{}' THEN ({})",
                demand.key.replace('\'', "''"),
                demand.predicate
            ));
        }
        urgent.push_str(" ELSE false END");
        if urgencies.is_empty() {
            urgent = "false".to_owned();
        }
        let mut poll = Poll::new("maintenance-scheduler", "probe");
        let probed_at = Instant::now();
        let result = sqlx::query_as::<_, (String, String)>(sqlx::AssertSqlSafe(format!(
            r#"WITH cutoff AS MATERIALIZED (SELECT clock_timestamp() AS now)
            SELECT 'kind'::text,k.kind
            FROM unnest($1::text[]) k(kind)
            WHERE EXISTS (SELECT 1 FROM background_work w WHERE {} AND (w.state='running' OR {kind_demand}))
            UNION ALL
            SELECT 'key'::text,k.id
            FROM unnest($2::text[]) k(id)
            LEFT JOIN background_work w ON w.id=k.id
            WHERE ((w.id IS NULL OR (w.state IN ('complete','failed')
                AND w.available_at<=(SELECT now FROM cutoff))) AND {key_demand})
                OR (w.state='complete' AND ({urgent}))"#,
            candidate_filter("k.kind", "true")
        )))
        .bind(kinds)
        .bind(keys)
        .fetch_all(&self.pool)
        .await;
        if result.as_ref().is_ok_and(Vec::is_empty) {
            poll.preflight_empty();
        }
        poll.finish(result.is_ok(), false);
        let mut ready = Ready::default();
        for (category, value) in result? {
            if category == "kind" {
                ready.kinds.insert(value);
            } else {
                ready.keys.insert(value);
            }
        }
        snapshot.generation = generation;
        let expires = probed_at + HINT_LIFETIME;
        snapshot.expires = Some(expires);
        snapshot.ready = ready.clone();
        Ok((ready, expires))
    }
}

fn same_pool(hint: &Hint, pool: &PgPool) -> bool {
    std::ptr::eq(hint.pool.options(), pool.options())
}

pub(super) fn skip_ensure(pool: &PgPool, ids: &[String]) -> bool {
    if crate::execution::has_scope() {
        return false;
    }
    HINT.try_with(|hint| {
        same_pool(hint, pool)
            && Instant::now() < *hint.expires.lock().unwrap()
            && ids.iter().all(|id| hint.keys.contains(id))
            && ids
                .iter()
                .all(|id| !hint.ready_keys.lock().unwrap().contains(id))
    })
    .unwrap_or(false)
}

pub(super) fn ensured(pool: &PgPool, kind: &str) {
    // Ensuring a recurring key may have just queued its work after the snapshot.
    // Its following claim must go to the DB even if that snapshot was negative.
    let _ = HINT.try_with(|hint| {
        if same_pool(hint, pool) {
            hint.ensured_kinds.lock().unwrap().insert(kind.to_owned());
        }
    });
}

pub(super) fn skip_claim(pool: &PgPool, kind: &str) -> bool {
    if crate::execution::has_scope() {
        return false;
    }
    HINT.try_with(|hint| {
        same_pool(hint, pool)
            && Instant::now() < *hint.expires.lock().unwrap()
            && hint.kinds.contains(kind)
            && !hint.ready_kinds.lock().unwrap().contains(kind)
            && !hint.ensured_kinds.lock().unwrap().contains(kind)
    })
    .unwrap_or(false)
}

#[cfg(test)]
#[path = "maintenance_scheduler_tests.rs"]
mod tests;
