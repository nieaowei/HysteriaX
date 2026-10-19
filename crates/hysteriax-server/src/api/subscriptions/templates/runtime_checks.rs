//! Offline diagnostic: exercise the production validators without a database or secrets.
use super::{catalog, document, render, verify_rendering};
use crate::error::ApiError;
use serde_json::{Value, json};

fn memory() -> Value {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kib = |name: &str| {
        status.lines().find_map(|line| {
            line.strip_prefix(name)?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
    };
    json!({"rss_kib":kib("VmRSS:"),"peak_rss_kib":kib("VmHWM:"),"swap_kib":kib("VmSwap:")})
}

pub fn check_runtime() -> Result<Value, ApiError> {
    let before = memory();
    let documents = ["mihomo", "singbox"].map(|format| (format, render::seed(format)));
    for (format, document) in &documents {
        document::ensure_valid(&document::validate(format, document)?)?;
        verify_rendering(format, document)?;
        let invalid = if *format == "mihomo" {
            json!({"mixed-port":"invalid"})
        } else {
            json!({"log":{"level":123}})
        };
        if !catalog::validate(format, &invalid, "linux")
            .iter()
            .any(|diagnostic| diagnostic.severity == "error")
        {
            return Err(ApiError::internal());
        }
    }
    let compiled = memory();
    let concurrent = || {
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        for _ in 0..2 {
                            for (format, document) in &documents {
                                document::ensure_valid(&document::validate(format, document)?)?;
                            }
                        }
                        Ok::<_, ApiError>(())
                    })
                })
                .collect();
            for handle in handles {
                handle.join().map_err(|_| ApiError::internal())??;
            }
            Ok::<_, ApiError>(())
        })
    };
    // Warm both serial and concurrent paths before testing repeated-work growth.
    concurrent()?;
    let warmed = memory();
    for _ in 0..16 {
        for (format, document) in &documents {
            document::ensure_valid(&document::validate(format, document)?)?;
        }
    }
    concurrent()?;
    Ok(
        json!({"status":"ok","formats":["mihomo","singbox"],"repeat_passes":16,
        "concurrent_workers":4,"before":before,"compiled":compiled,"warmed":warmed,"after":memory(),"invalid_configurations_rejected":true}),
    )
}
