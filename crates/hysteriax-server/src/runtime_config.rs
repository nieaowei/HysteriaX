use std::{env, sync::OnceLock};

const DEFAULT_DB_POOL_MAX_CONNECTIONS: usize = 10;
const MAX_DB_POOL_CONNECTIONS: usize = 100;
const DEFAULT_DB_POOL_ACQUIRE_TIMEOUT_SECONDS: usize = 5;
const MAX_DB_POOL_ACQUIRE_TIMEOUT_SECONDS: usize = 120;
const DEFAULT_DB_LOCK_TIMEOUT_SECONDS: usize = 1;
const DEFAULT_DB_STATEMENT_TIMEOUT_SECONDS: usize = 20;
const DEFAULT_DB_IDLE_TRANSACTION_TIMEOUT_SECONDS: usize = 20;
const MAX_TIMEOUT_SECONDS: usize = 3600;
const DEFAULT_BACKGROUND_WORKER_PARALLELISM: usize = 4;
const DEFAULT_SAMPLING_INTERVAL_SECONDS: usize = 10;
const MAX_SAMPLING_INTERVAL_SECONDS: usize = 3600;

static DB_POOL_MAX_CONNECTIONS: OnceLock<usize> = OnceLock::new();
static DB_POOL_ACQUIRE_TIMEOUT_SECONDS: OnceLock<usize> = OnceLock::new();
static DATABASE_TIMEOUTS: OnceLock<DatabaseTimeouts> = OnceLock::new();
static OPERATION_TIMEOUT_SECONDS: OnceLock<Option<u64>> = OnceLock::new();
static SSH_UPLOAD_TIMEOUT_SECONDS: OnceLock<u64> = OnceLock::new();
static BACKGROUND_WORKER_PARALLELISM: OnceLock<usize> = OnceLock::new();
static SAMPLING_INTERVAL_SECONDS: OnceLock<usize> = OnceLock::new();
static TRAFFIC_CAPTURE_ENABLED: OnceLock<bool> = OnceLock::new();

pub(crate) fn traffic_capture_enabled() -> bool {
    *TRAFFIC_CAPTURE_ENABLED.get_or_init(|| match env::var("HYSTERIAX_TRAFFIC_CAPTURE_ENABLED") {
        Ok(value) if value.eq_ignore_ascii_case("false") || value == "0" => false,
        Ok(value) if value.eq_ignore_ascii_case("true") || value == "1" => true,
        Err(env::VarError::NotPresent) => true,
        _ => {
            tracing::warn!("invalid HYSTERIAX_TRAFFIC_CAPTURE_ENABLED; traffic capture disabled");
            false
        }
    })
}

/// Shared cadence for traffic and host-network samples. Existing deployments
/// retain ten seconds unless they explicitly configure a longer collection slot.
pub(crate) fn sampling_interval_seconds() -> u64 {
    *SAMPLING_INTERVAL_SECONDS.get_or_init(|| {
        bounded_usize(
            "HYSTERIAX_SAMPLING_INTERVAL_SECONDS",
            DEFAULT_SAMPLING_INTERVAL_SECONDS,
            MAX_SAMPLING_INTERVAL_SECONDS,
        )
    }) as u64
}

pub(crate) fn sampling_freshness_seconds() -> i64 {
    (sampling_interval_seconds() * 3) as i64
}

pub(crate) fn db_pool_max_connections() -> usize {
    *DB_POOL_MAX_CONNECTIONS.get_or_init(|| {
        bounded_usize(
            "HYSTERIAX_DB_POOL_MAX_CONNECTIONS",
            DEFAULT_DB_POOL_MAX_CONNECTIONS,
            MAX_DB_POOL_CONNECTIONS,
        )
    })
}

pub(crate) fn db_pool_acquire_timeout_seconds() -> u64 {
    *DB_POOL_ACQUIRE_TIMEOUT_SECONDS.get_or_init(|| {
        bounded_usize(
            "HYSTERIAX_DB_POOL_ACQUIRE_TIMEOUT_SECONDS",
            DEFAULT_DB_POOL_ACQUIRE_TIMEOUT_SECONDS,
            MAX_DB_POOL_ACQUIRE_TIMEOUT_SECONDS,
        )
    }) as u64
}

struct DatabaseTimeouts {
    lock: u64,
    statement: u64,
    idle_transaction: u64,
    lease: u64,
}

fn database_timeouts() -> &'static DatabaseTimeouts {
    DATABASE_TIMEOUTS.get_or_init(|| {
        let seconds = |name, default| bounded_usize(name, default, MAX_TIMEOUT_SECONDS) as u64;
        let timeouts = DatabaseTimeouts {
            lock: seconds("HYSTERIAX_DB_LOCK_TIMEOUT_SECONDS", DEFAULT_DB_LOCK_TIMEOUT_SECONDS),
            statement: seconds("HYSTERIAX_DB_STATEMENT_TIMEOUT_SECONDS", DEFAULT_DB_STATEMENT_TIMEOUT_SECONDS),
            idle_transaction: seconds("HYSTERIAX_DB_IDLE_TRANSACTION_TIMEOUT_SECONDS", DEFAULT_DB_IDLE_TRANSACTION_TIMEOUT_SECONDS),
            lease: seconds("HYSTERIAX_EXECUTION_LEASE_SECONDS", crate::execution::LEASE_SECONDS as usize),
        };
        assert!(timeouts.statement + timeouts.idle_transaction < timeouts.lease,
            "database statement timeout plus idle transaction timeout must be shorter than the execution lease");
        assert!(timeouts.lock <= timeouts.statement,
            "database lock timeout must not exceed the statement timeout");
        timeouts
    })
}

pub(crate) fn db_lock_timeout_seconds() -> u64 {
    database_timeouts().lock
}

pub(crate) fn db_statement_timeout_seconds() -> u64 {
    database_timeouts().statement
}

pub(crate) fn db_idle_transaction_timeout_seconds() -> u64 {
    database_timeouts().idle_transaction
}

pub(crate) fn execution_lease_seconds() -> u64 {
    database_timeouts().lease
}

/// Production can give collection and its SSH/HTTP substeps one shared budget.
/// Without an override, each caller retains its existing timeout.
pub(crate) fn operation_timeout_seconds(default: u64) -> u64 {
    OPERATION_TIMEOUT_SECONDS.get_or_init(|| {
        let name = "HYSTERIAX_OPERATION_TIMEOUT_SECONDS";
        match env::var(name) {
            Ok(raw) => match raw.parse::<u64>() {
                Ok(0) => None,
                Ok(value) if (1..=MAX_TIMEOUT_SECONDS as u64).contains(&value) => Some(value),
                _ => {
                    tracing::warn!(variable = name, value = %raw, "invalid operation timeout; using per-operation defaults");
                    None
                }
            },
            Err(env::VarError::NotPresent) => None,
            Err(error) => {
                tracing::warn!(variable = name, %error, "operation timeout unavailable; using per-operation defaults");
                None
            }
        }
    }).unwrap_or(default)
}

/// File transfers have their own budget because small management requests and
/// release assets have very different bandwidth requirements.
pub(crate) fn ssh_upload_timeout_seconds() -> u64 {
    *SSH_UPLOAD_TIMEOUT_SECONDS.get_or_init(|| {
        bounded_usize(
            "HYSTERIAX_SSH_UPLOAD_TIMEOUT_SECONDS",
            180,
            MAX_TIMEOUT_SECONDS,
        ) as u64
    })
}

/// Limit parallel tasks within each durable background work queue. This never
/// raises the existing default of four; deployments can lower it for a remote DB.
pub(crate) fn background_worker_parallelism() -> usize {
    *BACKGROUND_WORKER_PARALLELISM.get_or_init(|| {
        bounded_usize(
            "HYSTERIAX_BACKGROUND_WORKER_PARALLELISM",
            DEFAULT_BACKGROUND_WORKER_PARALLELISM,
            DEFAULT_BACKGROUND_WORKER_PARALLELISM,
        )
    })
}

fn bounded_usize(name: &'static str, default: usize, max: usize) -> usize {
    match env::var(name) {
        Ok(raw) => match raw.parse::<usize>() {
            Ok(value) if (1..=max).contains(&value) => value,
            _ => {
                tracing::warn!(
                    variable = name,
                    value = %raw,
                    default,
                    "invalid runtime setting; using default"
                );
                default
            }
        },
        Err(env::VarError::NotPresent) => default,
        Err(error) => {
            tracing::warn!(
                variable = name,
                %error,
                default,
                "runtime setting unavailable; using default"
            );
            default
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn traffic_capture_setting_child() {
        if let Ok(expected) = std::env::var("HYSTERIAX_TEST_CAPTURE_EXPECTED") {
            assert_eq!(super::traffic_capture_enabled(), expected == "true");
        }
    }

    #[test]
    fn traffic_capture_setting_defaults_on_and_can_pause_capture() {
        for (configured, expected) in [
            (None, true),
            (Some("true"), true),
            (Some("0"), false),
            (Some("false"), false),
            (Some("invalid"), false),
        ] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "runtime_config::tests::traffic_capture_setting_child",
                ])
                .env("HYSTERIAX_TEST_CAPTURE_EXPECTED", expected.to_string())
                .env_remove("HYSTERIAX_TRAFFIC_CAPTURE_ENABLED");
            if let Some(value) = configured {
                child.env("HYSTERIAX_TRAFFIC_CAPTURE_ENABLED", value);
            }
            assert!(
                child.output().unwrap().status.success(),
                "configuration {configured:?}"
            );
        }
    }

    #[test]
    fn upload_timeout_child() {
        if let Ok(expected) = std::env::var("HYSTERIAX_TEST_EXPECT_UPLOAD_TIMEOUT") {
            assert_eq!(
                super::ssh_upload_timeout_seconds(),
                expected.parse::<u64>().unwrap()
            );
            assert_eq!(super::operation_timeout_seconds(15), 60);
        }
    }

    #[test]
    fn upload_timeout_defaults_to_three_minutes_and_is_independent_of_operation_budget() {
        for (configured, expected) in [
            (None, 180),
            (Some("1"), 1),
            (Some("180"), 180),
            (Some("3600"), 3600),
            (Some("0"), 180),
            (Some("3601"), 180),
            (Some("invalid"), 180),
        ] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", "runtime_config::tests::upload_timeout_child"])
                .env("HYSTERIAX_TEST_EXPECT_UPLOAD_TIMEOUT", expected.to_string())
                .env("HYSTERIAX_OPERATION_TIMEOUT_SECONDS", "60")
                .env_remove("HYSTERIAX_SSH_UPLOAD_TIMEOUT_SECONDS");
            if let Some(value) = configured {
                child.env("HYSTERIAX_SSH_UPLOAD_TIMEOUT_SECONDS", value);
            }
            assert!(
                child.output().unwrap().status.success(),
                "configuration {configured:?}"
            );
        }
    }
}
