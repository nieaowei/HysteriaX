//! Explicit finite-cohort concurrency experiment. Never linked into production.
//! Uses the real claim, collection, supervision, completion and expiry functions;
//! cycle admission is frozen before timing so every limit gets the same 60s slot.
use super::*;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::task::JoinSet;

#[path = "sampling_benchmark_fixture.rs"]
mod fixture;

#[path = "retention_benchmark.rs"]
mod retention;

fn distribution(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return json!({"count":0});
    }
    let at = |q: f64| values[((values.len() as f64 * q).ceil() as usize).saturating_sub(1)];
    json!({"count":values.len(),"p50_ms":at(0.5),"p95_ms":at(0.95),"p99_ms":at(0.99),"max_ms":values.last()})
}
