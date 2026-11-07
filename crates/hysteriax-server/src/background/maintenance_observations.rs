//! Poll outcomes are separate from transaction successes: None is not a claim.
use std::{
    collections::HashMap,
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
struct Counters {
    calls: AtomicU64,
    preflight_empty: AtomicU64,
    transactions: AtomicU64,
    claimed: AtomicU64,
    empty: AtomicU64,
    failed: AtomicU64,
    cancelled: AtomicU64,
    duration_us: AtomicU64,
}
type Registry = HashMap<(String, &'static str), Arc<Counters>>;
static REGISTRY: OnceLock<RwLock<Registry>> = OnceLock::new();

pub(super) struct Poll {
    counters: Arc<Counters>,
    started: Instant,
    finished: bool,
    claim: bool,
}
impl Poll {
    pub fn new(kind: &str, operation: &'static str) -> Self {
        let registry = REGISTRY.get_or_init(Default::default);
        let key = (kind.to_owned(), operation);
        let found = registry.read().unwrap().get(&key).cloned();
        let counters =
            found.unwrap_or_else(|| registry.write().unwrap().entry(key).or_default().clone());
        counters.calls.fetch_add(1, Ordering::Relaxed);
        Self {
            counters,
            started: Instant::now(),
            finished: false,
            claim: operation == "claim",
        }
    }
    pub fn preflight_empty(&self) {
        self.counters
            .preflight_empty
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn transaction(&self) {
        self.counters.transactions.fetch_add(1, Ordering::Relaxed);
    }
    pub fn finish(&mut self, success: bool, claimed: bool) {
        self.finished = true;
        if !success {
            self.counters.failed.fetch_add(1, Ordering::Relaxed);
        }
        if claimed {
            self.counters.claimed.fetch_add(1, Ordering::Relaxed);
        } else if success && self.claim {
            self.counters.empty.fetch_add(1, Ordering::Relaxed);
        }
        self.duration();
    }
    fn duration(&self) {
        self.counters
            .duration_us
            .fetch_add(self.started.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
}
impl Drop for Poll {
    fn drop(&mut self) {
        if !self.finished {
            self.counters.cancelled.fetch_add(1, Ordering::Relaxed);
            self.duration();
        }
    }
}

fn summaries() {
    let Some(registry) = REGISTRY.get() else {
        return;
    };
    let snapshot: Vec<_> = registry
        .read()
        .unwrap()
        .iter()
        .map(|(key, counters)| (key.clone(), counters.clone()))
        .collect();
    for ((kind, operation), c) in snapshot {
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        tracing::info!(
            kind,
            operation,
            cumulative = true,
            calls = load(&c.calls),
            preflight_empty = load(&c.preflight_empty),
            transactions = load(&c.transactions),
            claimed = load(&c.claimed),
            empty = load(&c.empty),
            failed = load(&c.failed),
            cancelled = load(&c.cancelled),
            duration_us = load(&c.duration_us),
            "maintenance poll summary"
        );
    }
}

pub(crate) async fn run_summaries(mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let mut timer = tokio::time::interval(Duration::from_secs(60));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    timer.tick().await;
    loop {
        tokio::select! {
            _=timer.tick()=>summaries(),
            _=shutdown.wait_for(|stop|*stop)=>{summaries();return;}
        }
    }
}
