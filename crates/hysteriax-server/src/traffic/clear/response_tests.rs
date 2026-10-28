use super::*;
use crate::traffic::{batch_tests::fixture as batch_fixture, batches, capture_clear_response};
use serde_json::{Map, Value, json};

async fn fixture(users: usize) -> (PgPool, crate::background::Work) {
    let pool = batch_fixture(users).await;
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    let cycle = crate::background::ensure_cycle(&pool, "traffic")
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour' WHERE cycle_id=$1")
        .bind(cycle).execute(&pool).await.unwrap();
    let work = crate::background::claim(&pool, "traffic", "original")
        .await
        .unwrap()
        .unwrap();
    batches::reserve_work(&pool, &work).await.unwrap();
    assert_eq!(prepare(&pool, &work).await.unwrap(), Admission::Send);
    crate::overview::record_online(
        &pool,
        &work.node,
        cycle,
        work.period_start,
        work.expected,
        Some(br#"{"user-0001":1}"#),
    )
    .await
    .unwrap();
    (pool, work)
}

fn response(tx: u64) -> (Map<String, Value>, Vec<u8>) {
    let counters = Map::from_iter([("user-0001".into(), json!({"tx":tx,"rx":0}))]);
    let body = serde_json::to_vec(&counters).unwrap();
    (counters, body)
}

#[tokio::test]
async fn complete_response_is_durable_before_followup_ssh_and_interval_bytes_are_never_discarded() {
    for outcome in ["disconnected", "restarted", "verified"] {
        let (pool, work) = fixture(1).await;
        let (counters, body) = response(100);
        capture_clear_response(&pool, &work, "before", &counters, &body, async {
            // A connection can fail at this exact boundary. The full
            // response must already be recoverable before SSH is polled.
            assert!(batches::captured(&pool, &work.id).await.unwrap());
            match outcome {
                "disconnected" => Err(anyhow::anyhow!("SSH disconnected after complete response")),
                "restarted" => Ok("after".into()),
                _ => Ok("before".into()),
            }
        })
        .await
        .unwrap();
        assert_eq!(prepare(&pool, &work).await.unwrap(), Admission::Captured);
        assert!(!uncertain(&pool, &work).await.unwrap());
        for _ in 0..2 {
            assert!(batches::drain(&pool, &work.id).await.unwrap());
        }
        let verified = outcome == "verified";
        let row: (String, bool, i64, i64) = sqlx::query_as("SELECT state,clear_instance_verified,item_count,processed_count FROM traffic_sample_batches WHERE sample_id=$1")
                .bind(&work.id).fetch_one(&pool).await.unwrap();
        assert_eq!(row, ("complete".into(), verified, 1, 1));
        let usage: i64 = sqlx::query_scalar("SELECT usage_bytes FROM users WHERE id='user-0001'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(usage, 100);
        let status: String = sqlx::query_scalar(
            "SELECT traffic_status FROM online_samples WHERE node_id='a' AND cycle_id=$1",
        )
        .bind(work.cycle)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, if verified { "ok" } else { "failed" });
        let gaps: i64 =
            sqlx::query_scalar("SELECT count(*) FROM data_gaps WHERE resolved_at IS NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(gaps, i64::from(!verified));
        let capture_mode: String =
            sqlx::query_scalar("SELECT traffic_capture_mode FROM nodes WHERE id='a'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(capture_mode, "interval");
    }
}

#[tokio::test]
async fn process_loss_during_followup_ssh_recovers_received_interval_once_under_new_leases() {
    let (pool, old) = fixture(1).await;
    let (counters, body) = response(17);
    let (saved, ready) = tokio::sync::oneshot::channel();
    let (other_pool, other_work) = (pool.clone(), old.clone());
    let task = tokio::spawn(async move {
        capture_clear_response(
            &other_pool,
            &other_work,
            "before",
            &counters,
            &body,
            async {
                saved.send(()).unwrap();
                std::future::pending().await
            },
        )
        .await
    });
    ready.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let new = crate::background::claim(&pool, "traffic", "replacement")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new.id, old.id);
    let (counters, body) = response(17);
    assert!(
        batches::seal_clear(&pool, &old, "before", &counters, &body, true)
            .await
            .is_err()
    );
    let state = crate::state::AppState::new(
        pool.clone(),
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    // Captured recovery cannot contact SSH or issue another destructive read.
    for _ in 0..2 {
        crate::traffic::sample_work(&state, &new).await.unwrap();
    }
    let result: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT usage_bytes FROM users WHERE id='user-0001'),(SELECT count(*) FROM traffic_records WHERE sample_id=$1),(SELECT count(*) FROM data_gaps WHERE reason='traffic_clear_instance_unverified')")
        .bind(&old.id).fetch_one(&pool).await.unwrap();
    assert_eq!(result, (17, 1, 1));
}

#[tokio::test]
async fn unverified_empty_and_multipage_intervals_preserve_quality_and_one_gap() {
    for users in [0, 501] {
        let (pool, work) = fixture(users).await;
        let counters: Map<String, Value> = (1..=users)
            .map(|n| (format!("user-{n:04}"), json!({"tx":10,"rx":0})))
            .collect();
        let body = serde_json::to_vec(&counters).unwrap();
        capture_clear_response(&pool, &work, "before", &counters, &body, async {
            Err(anyhow::anyhow!("SSH disconnected after complete response"))
        })
        .await
        .unwrap();
        if users > 0 {
            assert!(matches!(
                batches::process(&pool, &work.id).await.unwrap(),
                batches::Progress::Committed {
                    items: 500,
                    complete: false
                }
            ));
            // Even the original owner cannot revise the quality decision once
            // the first page has committed under an unverified instance.
            batches::seal_clear(&pool, &work, "before", &counters, &body, true)
                .await
                .unwrap();
        }
        batches::drain(&pool, &work.id).await.unwrap();
        let result: (bool, i64, i64, i64, String) = sqlx::query_as("SELECT clear_instance_verified,(SELECT coalesce(sum(usage_bytes),0)::bigint FROM users),(SELECT count(*) FROM traffic_records),(SELECT count(*) FROM data_gaps WHERE reason='traffic_clear_instance_unverified' AND resolved_at IS NULL),(SELECT traffic_status FROM online_samples WHERE cycle_id=$2 AND node_id='a') FROM traffic_sample_batches WHERE sample_id=$1")
            .bind(&work.id).bind(work.cycle).fetch_one(&pool).await.unwrap();
        assert_eq!(
            result,
            (false, users as i64 * 10, users as i64, 1, "failed".into())
        );
    }
}
