use serde_json::{Map, Value, json};
use sqlx::PgPool;

use super::{apply_sample, record_gap};

#[test]
fn raw_response_rejects_duplicate_or_invalid_filtered_counters() {
    assert!(super::parse_response(br#"{"u":{"tx":2,"rx":3}}"#).is_ok());
    for body in [
        &br#"{"probe":{"tx":"bad","rx":0},"probe":{"tx":1,"rx":0}}"#[..],
        br#"{"probe":{"tx":1,"tx":2,"rx":0}}"#,
        br#"{"u":{"tx":9223372036854775808,"rx":0}}"#,
        br#"{"u":{"tx":1,"rx":0}} trailing"#,
    ] {
        assert!(super::parse_response(body).is_err());
    }
}

async fn sample_pool(user_id: &str) -> PgPool {
    let pool = crate::db::test_pool().await;
    let timestamp = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    sqlx::query(
        "INSERT INTO users (id, name, created_at, updated_at) VALUES ($1, 'Sample user', $2, $3)",
    )
    .bind(user_id)
    .bind(timestamp)
    .bind(timestamp)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("WITH fixture_ssh AS (INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('fixture-ssh','Fixture SSH','ssh_password',now(),now()) ON CONFLICT DO NOTHING), fixture_ssh_version AS (INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('fixture-ssh',1,'unused',now()) ON CONFLICT DO NOTHING) INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at,ssh_credential_id,ssh_credential_version) VALUES ('node-one','Node one','127.0.0.1',22,'root','node.example.test',443,':443','node-hash','node-token','stats-secret','{}',$1,$2,'fixture-ssh',1)")
            .bind(timestamp)
            .bind(timestamp)
            .execute(&pool)
            .await
            .unwrap();
    sqlx::query("WITH assignment_fixture(user_id,node_id,credential_hash,credential_enc,created_at) AS MATERIALIZED (VALUES ($1, 'node-one', 'credential-hash', 'credential', $2)), password_fixture AS (INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at) SELECT 'connection:'||user_id,user_id,'connection',credential_hash,credential_enc,now() FROM (SELECT DISTINCT ON(user_id) user_id,credential_hash,credential_enc FROM assignment_fixture ORDER BY user_id) c ON CONFLICT(user_id,kind) WHERE revoked_at IS NULL DO UPDATE SET secret_hash=EXCLUDED.secret_hash,secret_enc=EXCLUDED.secret_enc), fixture_groups AS (INSERT INTO authorization_groups(id,name,created_at,updated_at) SELECT 'fixture-'||md5(user_id||chr(31)||node_id),'Fixture authorization',now(),now() FROM assignment_fixture ON CONFLICT(id) DO UPDATE SET name=EXCLUDED.name RETURNING id), fixture_members AS (INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT g.id,a.user_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING), fixture_nodes AS (INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT g.id,a.node_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING) INSERT INTO node_assignments(user_id,node_id,created_at) SELECT user_id,node_id,created_at FROM assignment_fixture")
            .bind(user_id)
            .bind(timestamp)
            .execute(&pool)
            .await
            .unwrap();
    pool
}

fn counters(user_id: &str, tx: u64, rx: u64) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([(user_id.to_owned(), json!({"tx": tx, "rx": rx}))])
}

#[tokio::test]
async fn slow_and_failed_nodes_do_not_block_peers_and_concurrency_is_bounded() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let slow = Arc::new(tokio::sync::Notify::new());
    let peers = Arc::new(tokio::sync::Notify::new());
    let worker = {
        let (active, peak, completed, slow, peers) = (
            active.clone(),
            peak.clone(),
            completed.clone(),
            slow.clone(),
            peers.clone(),
        );
        tokio::spawn(async move {
            super::collect_nodes((0..12).map(|i| i.to_string()).collect(), |id| {
                let (active, peak, completed, slow, peers) = (
                    active.clone(),
                    peak.clone(),
                    completed.clone(),
                    slow.clone(),
                    peers.clone(),
                );
                async move {
                    let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(count, Ordering::SeqCst);
                    if id == "0" {
                        slow.notified().await;
                    } else {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                    if completed.fetch_add(1, Ordering::SeqCst) == 10 {
                        peers.notify_one();
                    }
                    if id == "1" {
                        anyhow::bail!("injected sample failure");
                    }
                    Ok(())
                }
            })
            .await;
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), peers.notified())
        .await
        .unwrap();
    assert_eq!(completed.load(Ordering::SeqCst), 11);
    assert_eq!(peak.load(Ordering::SeqCst), 4);
    slow.notify_one();
    worker.await.unwrap();
    assert_eq!(completed.load(Ordering::SeqCst), 12);
}

#[tokio::test]
async fn malformed_sample_is_rejected_before_any_user_is_charged() {
    let pool = sample_pool("user-a").await;
    let counters = Map::<String, Value>::from_iter([
        ("user-a".to_owned(), json!({"tx": 100, "rx": 50})),
        ("user-z".to_owned(), json!({"tx": "invalid", "rx": 25})),
    ]);

    assert!(
        apply_sample(&pool, "node-one", "instance-one", &counters)
            .await
            .is_err()
    );
    record_gap(&pool, "node-one", "traffic_sample_failed")
        .await
        .unwrap();

    let usage: i64 = sqlx::query_scalar("SELECT usage_bytes FROM users WHERE id = 'user-a'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let baselines: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM traffic_baselines WHERE node_id = 'node-one'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let records: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM traffic_records WHERE node_id = 'node-one'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let gaps: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM data_gaps WHERE node_id = 'node-one' AND resolved_at IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage, 0);
    assert_eq!(baselines, 0);
    assert_eq!(records, 0);
    assert_eq!(gaps, 1);
}

#[tokio::test]
async fn a_sample_applied_after_quota_reset_is_charged_to_the_new_period() {
    let pool = sample_pool("late-user").await;
    sqlx::query("UPDATE users SET usage_bytes = 450 WHERE id = 'late-user'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO traffic_baselines (node_id, user_id, instance_id, tx_total, rx_total, sampled_at) VALUES ('node-one', 'late-user', 'instance-one', 100, 100, '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();

    // The remote counter snapshot was taken before reset but persisted after it.
    sqlx::query("UPDATE users SET usage_bytes = 0, quota_reset_at = '2026-01-01T00:00:10Z' WHERE id = 'late-user'")
            .execute(&pool)
            .await
            .unwrap();
    apply_sample(
        &pool,
        "node-one",
        "instance-one",
        &counters("late-user", 40, 30),
    )
    .await
    .unwrap();

    let usage: i64 = sqlx::query_scalar("SELECT usage_bytes FROM users WHERE id = 'late-user'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let deltas: (i64, i64) = sqlx::query_as(
        "SELECT delta_tx, delta_rx FROM traffic_records WHERE user_id = 'late-user'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(usage, 70);
    assert_eq!(deltas, (40, 30));
}

#[tokio::test]
async fn same_user_usage_aggregates_across_nodes_and_enqueues_one_kick_per_node() {
    let pool = crate::db::test_pool().await;

    let user_id = "quota-user";
    let timestamp = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    sqlx::query("INSERT INTO users (id, name, enabled, quota_bytes, usage_bytes, revision, created_at, updated_at) VALUES ($1, $2, TRUE, 500, 0, 1, $3, $4)")
            .bind(user_id).bind("Shared user").bind(timestamp).bind(timestamp)
            .execute(&pool).await.unwrap();

    for (index, node_id) in ["node-one", "node-two"].into_iter().enumerate() {
        sqlx::query("WITH fixture_ssh AS (INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('fixture-ssh','Fixture SSH','ssh_password',now(),now()) ON CONFLICT DO NOTHING), fixture_ssh_version AS (INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('fixture-ssh',1,'unused',now()) ON CONFLICT DO NOTHING) INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at,ssh_credential_id,ssh_credential_version) VALUES ($1,$2,$3,22,'root',$4,443,':443',$5,'node-token','stats-secret','{}',$6,$7,'fixture-ssh',1)")
                .bind(node_id).bind(node_id).bind("127.0.0.1").bind("node.example.test")
                .bind(format!("node-hash-{index}" )).bind(timestamp).bind(timestamp)
                .execute(&pool).await.unwrap();
        sqlx::query("WITH assignment_fixture(user_id,node_id,credential_hash,credential_enc,created_at) AS MATERIALIZED (VALUES ($1, $2, $3, 'credential', $4)), password_fixture AS (INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at) SELECT 'connection:'||user_id,user_id,'connection',credential_hash,credential_enc,now() FROM (SELECT DISTINCT ON(user_id) user_id,credential_hash,credential_enc FROM assignment_fixture ORDER BY user_id) c ON CONFLICT(user_id,kind) WHERE revoked_at IS NULL DO UPDATE SET secret_hash=EXCLUDED.secret_hash,secret_enc=EXCLUDED.secret_enc), fixture_groups AS (INSERT INTO authorization_groups(id,name,created_at,updated_at) SELECT 'fixture-'||md5(user_id||chr(31)||node_id),'Fixture authorization',now(),now() FROM assignment_fixture ON CONFLICT(id) DO UPDATE SET name=EXCLUDED.name RETURNING id), fixture_members AS (INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT g.id,a.user_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING), fixture_nodes AS (INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT g.id,a.node_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING) INSERT INTO node_assignments(user_id,node_id,created_at) SELECT user_id,node_id,created_at FROM assignment_fixture")
                .bind(user_id).bind(node_id).bind(format!("credential-hash-{index}" )).bind(timestamp)
                .execute(&pool).await.unwrap();
    }

    apply_sample(
        &pool,
        "node-one",
        "instance-one",
        &counters(user_id, 100, 50),
    )
    .await
    .unwrap();
    apply_sample(
        &pool,
        "node-two",
        "instance-two",
        &counters(user_id, 200, 75),
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        425
    );

    apply_sample(
        &pool,
        "node-one",
        "instance-one",
        &counters(user_id, 20, 10),
    )
    .await
    .unwrap();
    apply_sample(
        &pool,
        "node-two",
        "instance-two",
        &counters(user_id, 50, 15),
    )
    .await
    .unwrap();

    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        520
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM background_work WHERE kind='access-revocation' AND state='queued'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    super::access::drain_for_test(&pool).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM jobs WHERE kind = 'kick'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );

    apply_sample(&pool, "node-one", "instance-one", &counters(user_id, 0, 0))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        520
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM jobs WHERE kind = 'kick'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}
