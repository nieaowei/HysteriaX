//! In-process transaction diagnostics. No SQL or extra database round trips.
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context};

#[derive(Clone, Debug, Default)]
struct Statement {
    sql: String,
    fingerprint: String,
    elapsed_us: u64,
}
#[derive(Clone, Debug, Default)]
struct Failure {
    sqlstate: String,
    routine: String,
    server_line: usize,
    server_context: String,
    phase: &'static str,
    statement: Statement,
}
#[derive(Default)]
struct AttemptContext {
    phase: &'static str,
    last: Statement,
    failure: Option<Failure>,
}
tokio::task_local! { static CURRENT: RefCell<AttemptContext>; }

pub(crate) struct QueryLayer;
impl<S: Subscriber> Layer<S> for QueryLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() != "sqlx::query" {
            return;
        }
        let _ = CURRENT.try_with(|current| {
            let mut visitor = QueryVisitor::default();
            event.record(&mut visitor);
            let sql = if visitor.sql.trim().is_empty() {
                &visitor.summary
            } else {
                &visitor.sql
            };
            if !sql.trim().is_empty() {
                current.borrow_mut().last = Statement {
                    sql: redact(sql),
                    fingerprint: hex::encode(Sha256::digest(sql.trim().as_bytes())),
                    elapsed_us: (visitor.elapsed_secs * 1_000_000.0) as u64,
                };
            }
        });
    }
}
#[derive(Default)]
struct QueryVisitor {
    sql: String,
    summary: String,
    elapsed_secs: f64,
}
impl Visit for QueryVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "db.statement" => self.sql = value.to_owned(),
            "summary" => self.summary = value.to_owned(),
            _ => {}
        }
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_secs" {
            self.elapsed_secs = value;
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if matches!(field.name(), "db.statement" | "summary") {
            let text = format!("{value:?}");
            self.record_str(
                field,
                &serde_json::from_str::<String>(&text).unwrap_or(text),
            );
        }
    }
}

pub(crate) fn active() -> bool {
    CURRENT.try_with(|_| ()).is_ok()
}
pub(crate) fn phase(phase: &'static str) {
    let _ = CURRENT.try_with(|c| c.borrow_mut().phase = phase);
}
/// Capture before a conversion erases sqlx's structured error, and before rollback.
pub(crate) fn record(error: &sqlx::Error) {
    let _ = CURRENT.try_with(|c| {
        let mut c = c.borrow_mut();
        let pg = error
            .as_database_error()
            .and_then(|e| e.try_downcast_ref::<sqlx::postgres::PgDatabaseError>());
        c.failure = Some(Failure {
            sqlstate: error
                .as_database_error()
                .and_then(|e| e.code())
                .map(|s| s.into_owned())
                .unwrap_or_default(),
            routine: pg.and_then(|e| e.routine()).unwrap_or("").to_owned(),
            server_line: pg.and_then(|e| e.line()).unwrap_or(0),
            // Detail and error messages may contain bound values. Retain only a
            // redacted server context (e.g. the cache-flush trigger function).
            server_context: pg
                .and_then(|e| e.r#where())
                .map(server_functions)
                .unwrap_or_default(),
            phase: c.phase,
            statement: c.last.clone(),
        });
    });
}
pub(crate) fn record_anyhow(error: &anyhow::Error) {
    if let Some(error) = error.chain().find_map(|e| e.downcast_ref::<sqlx::Error>()) {
        record(error);
    }
}

/// A successful cleanup must not replace the statement that caused the abort.
/// On cleanup failure retain its own statement and phase instead.
pub(crate) async fn cleanup<T, E>(
    work: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let previous = CURRENT
        .try_with(|c| {
            let mut c = c.borrow_mut();
            let previous = (c.phase, c.last.clone());
            c.phase = "rollback";
            previous
        })
        .ok();
    let result = work.await;
    if result.is_ok()
        && let Some((phase, last)) = previous
    {
        let _ = CURRENT.try_with(|c| {
            let mut c = c.borrow_mut();
            c.phase = phase;
            c.last = last;
        });
    }
    result
}

pub(crate) struct AttemptReport {
    failure: Failure,
}
pub(crate) async fn attempt<T>(work: impl std::future::Future<Output = T>) -> (T, AttemptReport) {
    let context = RefCell::new(AttemptContext {
        phase: "begin",
        ..Default::default()
    });
    CURRENT
        .scope(context, async {
            let value = work.await;
            let failure = CURRENT.with(|c| {
                let c = c.borrow();
                c.failure.clone().unwrap_or_else(|| Failure {
                    phase: c.phase,
                    statement: c.last.clone(),
                    ..Default::default()
                })
            });
            (value, AttemptReport { failure })
        })
        .await
}

#[derive(Default)]
struct Counters {
    calls: AtomicU64,
    attempts: AtomicU64,
    conflicts: AtomicU64,
    succeeded: AtomicU64,
    retried_success: AtomicU64,
    failed: AtomicU64,
    exhausted: AtomicU64,
    cancelled: AtomicU64,
    duration_us: AtomicU64,
    max_duration_us: AtomicU64,
    latency: [AtomicU64; 8],
}
type Operations = HashMap<(&'static str, &'static str), Arc<Counters>>;
static OPERATIONS: OnceLock<RwLock<Operations>> = OnceLock::new();
const BOUNDS_US: [u64; 7] = [
    10_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 5_000_000,
];

pub(crate) struct Observation {
    id: uuid::Uuid,
    operation: &'static str,
    isolation: &'static str,
    start: Instant,
    counters: Arc<Counters>,
    finished: bool,
    pub conflicts: u64,
}
impl Observation {
    pub fn new(operation: &'static str, isolation: &'static str) -> Self {
        let registry = OPERATIONS.get_or_init(Default::default);
        let found = registry
            .read()
            .unwrap()
            .get(&(operation, isolation))
            .cloned();
        let counters = found.unwrap_or_else(|| {
            registry
                .write()
                .unwrap()
                .entry((operation, isolation))
                .or_default()
                .clone()
        });
        counters.calls.fetch_add(1, Ordering::Relaxed);
        Self {
            id: uuid::Uuid::new_v4(),
            operation,
            isolation,
            start: Instant::now(),
            counters,
            finished: false,
            conflicts: 0,
        }
    }
    pub fn attempted(&self) {
        self.counters.attempts.fetch_add(1, Ordering::Relaxed);
    }
    pub fn conflict(&mut self) {
        self.conflicts += 1;
        self.counters.conflicts.fetch_add(1, Ordering::Relaxed);
    }
    pub fn event(
        &self,
        report: &AttemptReport,
        attempt: usize,
        outcome: &'static str,
        delay_ms: u64,
    ) {
        let f = &report.failure;
        let identity = crate::execution::diagnostic_execution();
        let work_id = identity.as_ref().map(|e| e.work.as_str()).unwrap_or("");
        let resource = identity.as_ref().map(|e| e.resource.as_str()).unwrap_or("");
        let owner = identity.as_ref().map(|e| e.owner.as_str()).unwrap_or("");
        let generation = identity.as_ref().map(|e| e.generation).unwrap_or(0);
        let epoch = identity.as_ref().map(|e| e.epoch.as_str()).unwrap_or("");
        macro_rules! emit { ($level:ident) => {{ tracing::$level!(
            transaction_id=%self.id, operation=self.operation, isolation=self.isolation,
            attempt, max_attempts=6, outcome, delay_ms, elapsed_us=self.start.elapsed().as_micros() as u64,
            phase=f.phase, sqlstate=%f.sqlstate, statement=?f.statement.sql,
            statement_fingerprint=%f.statement.fingerprint, statement_elapsed_us=f.statement.elapsed_us,
            server_routine=%f.routine, server_line=f.server_line, server_context=%f.server_context,
            work_id,resource,owner,generation,epoch,"database transaction outcome"); }}; }
        match outcome {
            "retry" => emit!(warn),
            "retry_exhausted" => emit!(error),
            "failed" if !f.sqlstate.is_empty() => emit!(error),
            "failed" if self.conflicts > 0 => emit!(warn),
            "succeeded_after_retry" => emit!(info),
            _ => emit!(debug),
        }
    }
    pub fn finish(&mut self, success: bool, exhausted: bool) {
        self.finished = true;
        let c = &self.counters;
        if success {
            c.succeeded.fetch_add(1, Ordering::Relaxed);
            if self.conflicts > 0 {
                c.retried_success.fetch_add(1, Ordering::Relaxed);
            }
        } else {
            c.failed.fetch_add(1, Ordering::Relaxed);
        }
        if exhausted {
            c.exhausted.fetch_add(1, Ordering::Relaxed);
        }
        self.duration();
    }
    fn duration(&self) {
        let us = self.start.elapsed().as_micros() as u64;
        self.counters.duration_us.fetch_add(us, Ordering::Relaxed);
        self.counters
            .max_duration_us
            .fetch_max(us, Ordering::Relaxed);
        let bucket = BOUNDS_US.iter().position(|b| us <= *b).unwrap_or(7);
        self.counters.latency[bucket].fetch_add(1, Ordering::Relaxed);
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        if !self.finished {
            self.counters.cancelled.fetch_add(1, Ordering::Relaxed);
            self.duration();
            if self.conflicts > 0 {
                tracing::warn!(transaction_id=%self.id,operation=self.operation,isolation=self.isolation,
                    conflicts=self.conflicts,outcome="cancelled","database transaction future cancelled");
            }
        }
    }
}
pub(crate) fn summaries() {
    let Some(registry) = OPERATIONS.get() else {
        return;
    };
    let snapshot: Vec<_> = registry
        .read()
        .unwrap()
        .iter()
        .map(|(key, value)| (*key, value.clone()))
        .collect();
    for ((operation, isolation), c) in snapshot {
        let load = |v: &AtomicU64| v.load(Ordering::Relaxed);
        let latency: Vec<_> = c.latency.iter().map(load).collect();
        tracing::info!(operation,isolation,cumulative=true,calls=load(&c.calls),attempts=load(&c.attempts),
            conflicts=load(&c.conflicts),succeeded=load(&c.succeeded),retried_success=load(&c.retried_success),
            failed=load(&c.failed),retry_exhausted=load(&c.exhausted),cancelled=load(&c.cancelled),
            duration_us=load(&c.duration_us),max_duration_us=load(&c.max_duration_us),
            latency_buckets=?latency,latency_bounds_us=?BOUNDS_US,"database transaction summary");
    }
}
pub(crate) async fn run_summaries(mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let mut timer = tokio::time::interval(Duration::from_secs(60));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    timer.tick().await;
    loop {
        tokio::select! {
            _=timer.tick()=>summaries(),
            _=shutdown.wait_for(|v|*v)=>{summaries();return;}
        }
    }
}

// Context can embed SQL and data values. Only function identifiers are useful.
fn server_functions(context: &str) -> String {
    context
        .lines()
        .filter_map(|line| {
            let text = line
                .trim()
                .strip_prefix("PL/pgSQL function ")
                .or_else(|| line.trim().strip_prefix("SQL function "))?;
            let name = text.split(['(', ' ']).next()?.trim_matches('"');
            (name.len() <= 128
                && !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.')))
            .then_some(name)
        })
        .take(4)
        .collect::<Vec<_>>()
        .join(" > ")
}

/// Remove SQL literals, quoted identifiers and comments before writing any SQL.
/// Bound parameters are kept; their values never enter SQLx query events.
fn redact(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut out = String::new();
    while i < bytes.len() && out.len() < 2048 {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
            i += 1;
        } else if bytes[i..].starts_with(b"--") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if b == b'\'' || b == b'"' {
            let quote = b;
            let escapes = quote == b'\''
                && i > 0
                && matches!(bytes[i - 1], b'e' | b'E')
                && (i == 1 || !(bytes[i - 2].is_ascii_alphanumeric() || bytes[i - 2] == b'_'));
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && quote == b'\'' {
                    // A plain string's backslash depends on the server setting.
                    // Suppress the rest rather than guess where a secret ends.
                    if !escapes {
                        i = bytes.len();
                        break;
                    }
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == quote {
                    i += 1;
                    if i < bytes.len() && bytes[i] == quote {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            out.push('?');
        } else if b == b'$' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
            out.push('$');
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                out.push(bytes[i] as char);
                i += 1;
            }
        } else if b == b'$' {
            let mut end = i + 1;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if bytes.get(end) == Some(&b'$') {
                let delimiter = &input[i..=end];
                i = end + 1;
                i = input[i..]
                    .find(delimiter)
                    .map(|offset| i + offset + delimiter.len())
                    .unwrap_or(bytes.len());
                out.push('?');
            } else {
                out.push('$');
                i += 1;
            }
        } else if b.is_ascii_digit()
            && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
        {
            out.push('?');
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'_'))
            {
                i += 1;
            }
        } else {
            let ch = input[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out.trim().to_owned()
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
