use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};

use super::{
    JobInput, deployment_probe_server_config, parse_http_probe_target, primary_listener_port,
    report_progress, systemd_unit, verify_hysteria_asset,
};

#[test]
fn startup_budget_and_service_failures() {
    assert_eq!(
        super::startup_health_timeout(&json!({"acme": {"domains": ["sg1.conn.lol"]}})).as_secs(),
        180
    );
    assert_eq!(
        super::startup_health_timeout(&json!({"tls": {"cert": "cert.pem", "key": "key.pem"}}))
            .as_secs(),
        30
    );
    for status in [
        "ActiveState=failed\nSubState=failed\nExecMainStatus=1\nNRestarts=0",
        "ActiveState=activating\nSubState=auto-restart\nExecMainStatus=1\nNRestarts=0",
        "ActiveState=active\nSubState=running\nExecMainStatus=0\nNRestarts=3",
        "ActiveState=inactive\nSubState=dead\nExecMainStatus=0\nNRestarts=0",
    ] {
        assert!(super::startup_service_failure(status).is_some());
    }
    assert!(
        super::startup_service_failure(
            "ActiveState=active\nSubState=running\nExecMainStatus=0\nNRestarts=0"
        )
        .is_none()
    );
}

#[tokio::test(start_paused = true)]
async fn startup_wait_accepts_delayed_acme_readiness() {
    let started = tokio::time::Instant::now();
    super::wait_for_startup(
        super::startup_health_timeout(&json!({"acme": {}})),
        || async {
            Ok(if started.elapsed() >= std::time::Duration::from_secs(30) {
                super::StartupCheck::Ready
            } else {
                super::StartupCheck::Pending("ConnectFailed".to_owned())
            })
        },
    )
    .await
    .unwrap();
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(30));
}

#[tokio::test(start_paused = true)]
async fn startup_wait_fails_early_when_service_exits() {
    let started = tokio::time::Instant::now();
    let error = super::wait_for_startup(std::time::Duration::from_secs(180), || async {
        Ok(if started.elapsed() >= std::time::Duration::from_secs(2) {
            super::StartupCheck::Failed("ExecMainStatus=1".to_owned())
        } else {
            super::StartupCheck::Pending("ConnectFailed".to_owned())
        })
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("ExecMainStatus=1"));
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
async fn startup_wait_expires_with_last_error() {
    let started = tokio::time::Instant::now();
    let error = super::wait_for_startup(std::time::Duration::from_secs(180), || async {
        Ok(super::StartupCheck::Pending("ConnectFailed".to_owned()))
    })
    .await
    .unwrap_err();
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(180));
    assert!(error.to_string().contains("budget 180s"));
    assert!(error.to_string().contains("ConnectFailed"));
}

#[tokio::test(start_paused = true)]
async fn startup_wait_bounds_hung_ssh_checks() {
    let started = tokio::time::Instant::now();
    let error = super::wait_for_startup(std::time::Duration::from_secs(30), || async {
        std::future::pending::<anyhow::Result<super::StartupCheck>>().await
    })
    .await
    .unwrap_err();
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(30));
    assert!(error.to_string().contains("startup check timed out"));
}

#[test]
fn realm_deployment_probe_uses_realm_uri_and_client_tuning() {
    let options = json!({
        "realm": {
            "connection": {
                "serverURL": "http://127.0.0.1:10820",
                "token": "realm-token",
                "realmID": "node-realm-1"
            },
            "stunServers": ["127.0.0.1:3478"],
            "stunTimeout": "5s",
            "ipMode": "v4"
        }
    });
    let (server, realm) = deployment_probe_server_config(&options, ":443").unwrap();
    assert_eq!(
        server,
        "realm+http://realm-token@127.0.0.1:10820/node-realm-1"
    );
    let realm = realm.unwrap();
    assert!(realm.get("connection").is_none());
    assert_eq!(realm["stunServers"][0], "127.0.0.1:3478");
    assert_eq!(realm["stunTimeout"], "5s");
    assert_eq!(realm["ipMode"], "v4");
}

#[test]
fn non_realm_deployment_probe_keeps_loopback_listener_address() {
    let (server, realm) = deployment_probe_server_config(&json!({}), ":443").unwrap();
    assert_eq!(server, "127.0.0.1:443");
    assert!(realm.is_none());
}

async fn test_pool() -> PgPool {
    crate::db::test_pool().await
}

#[test]
fn release_asset_digests_are_sha256_hex() {
    assert_eq!(
        hex::encode(Sha256::digest(b"pinned-release-check")),
        "5939868cdcb89df1b4fed608f8db89b421c39c53c909ef9458c523f3fe1fc3d5"
    );
}

#[test]
fn rejects_downloaded_asset_with_wrong_pinned_digest() {
    let bytes = vec![0x5a; 1_000_000];
    let error = verify_hysteria_asset(&bytes, &"0".repeat(64)).unwrap_err();
    assert!(error.to_string().contains("SHA-256 did not match"));
}

#[test]
fn parses_http_probe_targets_for_tcp_forwarding() {
    let target = parse_http_probe_target("http://status.example.test/health").unwrap();
    assert_eq!(target.remote_address, "status.example.test:80");
    assert_eq!(target.host_header, "status.example.test");
    assert_eq!(target.path, "/health");

    let ipv6 = parse_http_probe_target("http://[::1]:8080/ready").unwrap();
    assert_eq!(ipv6.remote_address, "[::1]:8080");
    assert_eq!(ipv6.host_header, "[::1]:8080");
    assert!(parse_http_probe_target("https://status.example.test/health").is_err());
    assert!(parse_http_probe_target("http://status.example.test/health?token=secret").is_err());
}

#[tokio::test]
async fn progress_updates_job_stage_logs_and_persisted_event_atomically() {
    let pool = test_pool().await;
    sqlx::query("INSERT INTO jobs (id, kind, target_revision, attempts, status, stage, available_at, created_at, updated_at) VALUES ('progress-job', 'sync', 1, 1, 'running', 'starting', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
    let job = JobInput {
        id: "progress-job".to_owned(),
        kind: "sync".to_owned(),
        node_id: None,
        target_revision: Some(1),
        payload: serde_json::json!({}),
        attempts: 1,
    };

    report_progress(
        &pool,
        &job,
        "downloading_release",
        "Verifying pinned release.",
    )
    .await
    .unwrap();

    let row = sqlx::query("SELECT stage, logs_json FROM jobs WHERE id = 'progress-job'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("stage"), "downloading_release");
    let logs: serde_json::Value = row
        .get::<sqlx::types::Json<serde_json::Value>, _>("logs_json")
        .0;
    assert_eq!(logs[0]["stage"], "downloading_release");
    assert_eq!(logs[0]["message"], "Verifying pinned release.");

    let event = sqlx::query(
        "SELECT event_type, payload_json FROM job_events WHERE job_id = 'progress-job'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(event.get::<String, _>("event_type"), "job.progress");
    let payload: serde_json::Value = event.get("payload_json");
    assert_eq!(payload["stage"], "downloading_release");
    assert_eq!(payload["message"], "Verifying pinned release.");
}

#[test]
fn net_admin_capability_is_limited_to_port_hopping_nodes() {
    assert!(!systemd_unit(false).contains("CAP_NET_ADMIN"));
    let unit = systemd_unit(true);
    assert!(unit.contains("AmbientCapabilities=CAP_NET_BIND_SERVICE CAP_NET_ADMIN"));
    assert!(unit.contains("CapabilityBoundingSet=CAP_NET_BIND_SERVICE CAP_NET_ADMIN"));
}

#[test]
fn extracts_the_first_listener_port_for_preflight_checks() {
    assert_eq!(primary_listener_port(":443"), Some(443));
    assert_eq!(primary_listener_port("0.0.0.0:8443,9000-9010"), Some(8443));
    assert_eq!(primary_listener_port("[::]:12000-12010"), Some(12000));
    assert_eq!(primary_listener_port("not-a-listener"), None);
}
