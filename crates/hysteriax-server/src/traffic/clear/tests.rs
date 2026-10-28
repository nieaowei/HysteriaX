use super::*;
use crate::traffic::{batch_tests::fixture_through, batches};
use serde_json::{Map, json};

async fn seal_clear_test(
    pool: &PgPool,
    work: &crate::background::Work,
    instance: &str,
    counters: &Map<String, serde_json::Value>,
) -> anyhow::Result<()> {
    let raw = serde_json::to_vec(counters)?;
    batches::seal_clear(pool, work, instance, counters, &raw, true).await
}

async fn bill(pool: &PgPool, id: &str, instance: &str, tx: i64) {
    let counters = Map::from_iter([("user-0001".into(), json!({"tx":tx,"rx":0}))]);
    batches::reserve(pool, id, "a", None).await.unwrap();
    batches::seal(pool, id, instance, &counters).await.unwrap();
    batches::drain(pool, id).await.unwrap();
    batches::drain(pool, id).await.unwrap();
}

#[tokio::test]
async fn payload_digest_covers_full_original_response_including_filtered_identities() {
    let pool = fixture_through(1, i64::MAX).await;
    batches::reserve(&pool, "raw", "a", None).await.unwrap();
    let first = br#"{"user-0001":{"tx":10,"rx":0,"extra":"first"},"missing":{"tx":2,"rx":0}}"#;
    let counters = crate::traffic::parse_response(first).unwrap();
    batches::seal_response(&pool, "raw", "instance", &counters, first)
        .await
        .unwrap();
    let changed = br#"{"user-0001":{"tx":10,"rx":0,"extra":"second"},"missing":{"tx":2,"rx":0}}"#;
    assert_eq!(crate::traffic::parse_response(changed).unwrap(), counters);
    assert!(
        batches::seal_response(&pool, "raw", "instance", &counters, changed)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT item_count FROM traffic_sample_batches WHERE sample_id='raw'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn interval_after_service_restart_bills_received_bytes_and_exposes_gap() {
    let pool = fixture_through(1, i64::MAX).await;
    bill(&pool, "switch", "old-instance", 100).await;
    bill(&pool, "restart", "new-instance", 40).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id='user-0001'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        140
    );
    let result: (i64, Option<String>, bool) = sqlx::query_as("SELECT delta_tx,gap_reason,(SELECT interval_restart FROM traffic_sample_batches WHERE sample_id='restart') FROM traffic_records WHERE sample_id='restart'").fetch_one(&pool).await.unwrap();
    assert_eq!(
        result,
        (40, Some("node_restarted_between_samples".into()), true)
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM data_gaps WHERE reason='node_restarted_between_samples' AND resolved_at IS NULL").fetch_one(&pool).await.unwrap(),1);
    bill(&pool, "after-restart", "new-instance", 10).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id='user-0001'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        150
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM data_gaps WHERE reason='node_restarted_between_samples' AND resolved_at IS NULL").fetch_one(&pool).await.unwrap(),0);
}

#[tokio::test]
async fn changing_probe_identities_do_not_accumulate_in_captured_details() {
    let pool = fixture_through(1000, i64::MAX).await;
    for round in 0..2 {
        let mut values: Map<String, serde_json::Value> = (1..=1000)
            .map(|n| (format!("user-{n:04}"), json!({"tx":10,"rx":5})))
            .collect();
        for n in 0..2000 {
            values.insert(
                format!(
                    "probe-a-{}",
                    crate::security::token_digest(&format!("{round}-{n}"))
                ),
                json!({"tx":1,"rx":2}),
            );
        }
        let id = format!("round-{round}");
        batches::reserve(&pool, &id, "a", None).await.unwrap();
        batches::seal(&pool, &id, "instance", &values)
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT item_count FROM traffic_sample_batches WHERE sample_id=$1"
            )
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            1000
        );
        batches::drain(&pool, &id).await.unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_sample_items")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2000
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users WHERE usage_bytes=30")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1000
    );
}

#[tokio::test]
async fn interval_billing_respects_quota_reset_between_pages() {
    let pool = fixture_through(501, i64::MAX).await;
    let first: Map<String, serde_json::Value> = (1..=501)
        .map(|n| (format!("user-{n:04}"), json!({"tx":100,"rx":0})))
        .collect();
    batches::reserve(&pool, "interval-before-reset", "a", None)
        .await
        .unwrap();
    batches::seal(&pool, "interval-before-reset", "instance", &first)
        .await
        .unwrap();
    assert!(matches!(
        batches::process(&pool, "interval-before-reset")
            .await
            .unwrap(),
        batches::Progress::Committed {
            items: 500,
            complete: false
        }
    ));
    sqlx::query("UPDATE users SET usage_bytes=0,quota_reset_at=clock_timestamp() WHERE id IN ('user-0001','user-0501')").execute(&pool).await.unwrap();
    batches::drain(&pool, "interval-before-reset")
        .await
        .unwrap();
    let second: Map<String, serde_json::Value> = (1..=501)
        .map(|n| (format!("user-{n:04}"), json!({"tx":40,"rx":0})))
        .collect();
    batches::reserve(&pool, "interval-after-reset", "a", None)
        .await
        .unwrap();
    batches::seal(&pool, "interval-after-reset", "instance", &second)
        .await
        .unwrap();
    batches::drain(&pool, "interval-after-reset").await.unwrap();
    let usages: Vec<i64> = sqlx::query_scalar(
        "SELECT usage_bytes FROM users WHERE id IN ('user-0001','user-0501') ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(usages, vec![40, 140]);
}

#[tokio::test]
async fn request_admission_is_single_shot_and_fenced_by_both_leases_and_slot() {
    let pool = fixture_through(0, i64::MAX).await;
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    let cycle = crate::background::ensure_cycle(&pool, "traffic")
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour' WHERE cycle_id=$1")
        .bind(cycle).execute(&pool).await.unwrap();
    let old = crate::background::claim(&pool, "traffic", "old")
        .await
        .unwrap()
        .unwrap();
    batches::reserve_work(&pool, &old).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT capture_mode FROM traffic_sample_batches WHERE sample_id=$1"
        )
        .bind(&old.id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "interval"
    );
    assert_eq!(prepare(&pool, &old).await.unwrap(), Admission::Send);
    assert_eq!(
        prepare(&pool, &old).await.unwrap(),
        Admission::AlreadyRequested
    );
    let marker: (String, String, i64, i64, String) = sqlx::query_as("SELECT capture_mode,clear_owner,clear_work_generation,clear_node_generation,clear_epoch FROM traffic_sample_batches WHERE sample_id=$1")
        .bind(&old.id).fetch_one(&pool).await.unwrap();
    assert_eq!(marker.0, "interval");
    assert_eq!(marker.1, "old");
    let work_lease = old
        .leases
        .iter()
        .find(|l| l.resource.starts_with("work:"))
        .unwrap();
    let node_lease = old
        .leases
        .iter()
        .find(|l| l.resource.starts_with("node:"))
        .unwrap();
    assert_eq!(
        (marker.2, marker.3, marker.4),
        (
            work_lease.generation,
            node_lease.generation,
            work_lease.epoch.clone()
        )
    );

    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let new = crate::background::claim(&pool, "traffic", "new")
        .await
        .unwrap()
        .unwrap();
    assert!(prepare(&pool, &old).await.is_err());
    assert_eq!(
        prepare(&pool, &new).await.unwrap(),
        Admission::AlreadyRequested
    );
    sqlx::query(
        "UPDATE background_work SET deadline_at=clock_timestamp()-interval '1 second' WHERE id=$1",
    )
    .bind(&old.id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(prepare(&pool, &new).await.is_err());
    assert!(uncertain(&pool, &old).await.is_err());
    assert!(uncertain(&pool, &new).await.unwrap());
    assert!(!uncertain(&pool, &new).await.unwrap());
    let state: (String, String, i64, String) = sqlx::query_as("SELECT n.traffic_capture_mode,b.state,(SELECT count(*) FROM data_gaps WHERE node_id=n.id AND reason='traffic_clear_uncertain'),o.traffic_status FROM nodes n JOIN traffic_sample_batches b ON b.node_id=n.id JOIN online_samples o ON o.cycle_id=b.cycle_id AND o.node_id=n.id WHERE b.sample_id=$1")
        .bind(&old.id).fetch_one(&pool).await.unwrap();
    assert_eq!(
        state,
        ("interval".into(), "discarded".into(), 1, "failed".into())
    );
}

#[tokio::test]
async fn uncertain_interval_preserves_capture_mode_and_node_lease() {
    let pool = fixture_through(0, i64::MAX).await;
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    let cycle = crate::background::ensure_cycle(&pool, "traffic")
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour' WHERE cycle_id=$1")
        .bind(cycle).execute(&pool).await.unwrap();
    let work = crate::background::claim(&pool, "traffic", "owner")
        .await
        .unwrap()
        .unwrap();
    batches::reserve(&pool, &work.id, "a", Some(cycle))
        .await
        .unwrap();
    assert_eq!(prepare(&pool, &work).await.unwrap(), Admission::Send);
    assert!(uncertain(&pool, &work).await.unwrap());
    let result: (String, bool, i64) = sqlx::query_as("SELECT n.traffic_capture_mode,l.blocked,(SELECT count(*) FROM execution_operations WHERE state IN ('dispatching','reconciling','needs_attention')) FROM nodes n JOIN resource_leases l ON l.resource_key='node:'||n.id WHERE n.id='a'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(result, ("interval".into(), false, 0));
}

#[tokio::test]
async fn clear_response_seals_after_slot_expiry_but_only_under_original_leases() {
    let pool = fixture_through(1, i64::MAX).await;
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    let cycle = crate::background::ensure_cycle(&pool, "traffic")
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour' WHERE cycle_id=$1").bind(cycle).execute(&pool).await.unwrap();
    let old = crate::background::claim(&pool, "traffic", "old")
        .await
        .unwrap()
        .unwrap();
    batches::reserve(&pool, &old.id, "a", Some(cycle))
        .await
        .unwrap();
    assert_eq!(prepare(&pool, &old).await.unwrap(), Admission::Send);
    sqlx::query("UPDATE traffic_sample_batches SET capture_deadline=clock_timestamp()-interval '1 second' WHERE sample_id=$1").bind(&old.id).execute(&pool).await.unwrap();
    sqlx::query(
        "UPDATE background_work SET deadline_at=clock_timestamp()-interval '1 second' WHERE id=$1",
    )
    .bind(&old.id)
    .execute(&pool)
    .await
    .unwrap();
    let counters = Map::from_iter([("user-0001".into(), json!({"tx":12,"rx":0}))]);
    assert!(
        batches::seal(&pool, &old.id, "instance", &counters)
            .await
            .is_err()
    );
    seal_clear_test(&pool, &old, "instance", &counters)
        .await
        .unwrap();
    seal_clear_test(&pool, &old, "instance", &counters)
        .await
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let new = crate::background::claim(&pool, "traffic", "new")
        .await
        .unwrap()
        .unwrap();
    assert!(
        seal_clear_test(&pool, &old, "instance", &counters)
            .await
            .is_err()
    );
    assert!(
        seal_clear_test(&pool, &new, "instance", &counters)
            .await
            .is_err()
    );
    assert!(batches::drain(&pool, &old.id).await.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id='user-0001'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        12
    );
}

#[tokio::test]
async fn abandoned_request_is_recorded_as_uncertain_by_capture_expiry() {
    let pool = fixture_through(0, i64::MAX).await;
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    let cycle = crate::background::ensure_cycle(&pool, "traffic")
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour' WHERE cycle_id=$1").bind(cycle).execute(&pool).await.unwrap();
    let work = crate::background::claim(&pool, "traffic", "lost")
        .await
        .unwrap()
        .unwrap();
    batches::reserve(&pool, &work.id, "a", Some(cycle))
        .await
        .unwrap();
    assert_eq!(prepare(&pool, &work).await.unwrap(), Admission::Send);
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE traffic_sample_batches SET capture_deadline=clock_timestamp()-interval '1 second' WHERE sample_id=$1").bind(&work.id).execute(&pool).await.unwrap();
    batches::expire_captures(&pool).await.unwrap();
    batches::expire_captures(&pool).await.unwrap();
    let state: (String,String,i64) = sqlx::query_as("SELECT n.traffic_capture_mode,b.state,(SELECT count(*) FROM data_gaps WHERE reason='traffic_clear_uncertain' AND node_id=n.id) FROM nodes n JOIN traffic_sample_batches b ON b.node_id=n.id WHERE b.sample_id=$1")
        .bind(&work.id).fetch_one(&pool).await.unwrap();
    assert_eq!(state, ("interval".into(), "discarded".into(), 1));
}

#[tokio::test]
async fn takeover_of_requested_capture_records_gap_without_another_remote_read() {
    let pool = fixture_through(0, i64::MAX).await;
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    let cycle = crate::background::ensure_cycle(&pool, "traffic")
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour' WHERE cycle_id=$1").bind(cycle).execute(&pool).await.unwrap();
    let old = crate::background::claim(&pool, "traffic", "old")
        .await
        .unwrap()
        .unwrap();
    batches::reserve(&pool, &old.id, "a", Some(cycle))
        .await
        .unwrap();
    assert_eq!(prepare(&pool, &old).await.unwrap(), Admission::Send);
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let new = crate::background::claim(&pool, "traffic", "new")
        .await
        .unwrap()
        .unwrap();
    let state = crate::state::AppState::new(
        pool.clone(),
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    // The fixture's SSH secret is invalid ciphertext. Getting the intended
    // uncertainty instead of a decryption error proves no remote read began.
    let error = crate::traffic::sample_work(&state, &new).await.unwrap_err();
    assert!(error.to_string().contains("not persisted"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM data_gaps WHERE reason='traffic_clear_uncertain'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM traffic_sample_batches WHERE sample_id=$1"
        )
        .bind(&old.id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        "discarded"
    );
}

#[tokio::test]
async fn database_lock_timeout_retries_the_same_received_response() {
    let pool = fixture_through(1, i64::MAX).await;
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    let cycle = crate::background::ensure_cycle(&pool, "traffic")
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour' WHERE cycle_id=$1").bind(cycle).execute(&pool).await.unwrap();
    let work = crate::background::claim(&pool, "traffic", "owner")
        .await
        .unwrap()
        .unwrap();
    batches::reserve(&pool, &work.id, "a", Some(cycle))
        .await
        .unwrap();
    assert_eq!(prepare(&pool, &work).await.unwrap(), Admission::Send);
    let mut locked = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='a' FOR UPDATE")
        .fetch_one(&mut *locked)
        .await
        .unwrap();
    let pool2 = pool.clone();
    let work2 = work.clone();
    let started = std::time::Instant::now();
    let task = tokio::spawn(async move {
        let counters = Map::from_iter([("user-0001".into(), json!({"tx":12,"rx":0}))]);
        seal_clear_test(&pool2, &work2, "instance", &counters).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(1350)).await;
    locked.rollback().await.unwrap();
    task.await.unwrap().unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_secs(1));
    assert_eq!(prepare(&pool, &work).await.unwrap(), Admission::Captured);
    batches::drain(&pool, &work.id).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id='user-0001'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        12
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_records WHERE sample_id=$1")
            .bind(&work.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
