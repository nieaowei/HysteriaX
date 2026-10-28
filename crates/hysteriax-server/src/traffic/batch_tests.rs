use super::batches::{self, Progress};
use serde_json::{Map, Value, json};
use sqlx::{PgPool, Row};

#[path = "bulk_tests.rs"]
mod bulk_tests;

async fn fixture(users: usize) -> PgPool {
    fixture_through(users, i64::MAX).await
}

pub(super) async fn fixture_through(users: usize, version: i64) -> PgPool {
    let pool = crate::db::test_pool_through_version(8, version).await;
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT id,id,'127.0.0.1',22,'root','password','secret','example.test',443,':443',id,id,'stats','{}',now(),now() FROM unnest(ARRAY['a','b']) id")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) SELECT 'user-'||lpad(n::text,4,'0'),'User '||n,now(),now() FROM generate_series(1,$1) n")
        .bind(users as i32).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) SELECT u.id,n.id,u.id||n.id,'secret',now() FROM users u CROSS JOIN nodes n")
        .execute(&pool).await.unwrap();
    pool
}
fn counters(users: usize, tx: i64, rx: i64) -> Map<String, Value> {
    (1..=users)
        .map(|index| (format!("user-{index:04}"), json!({"tx":tx,"rx":rx})))
        .collect()
}
async fn capture(pool: &PgPool, id: &str, node: &str, values: &Map<String, Value>) {
    assert_eq!(
        batches::reserve(pool, id, node, None).await.unwrap(),
        "capturing"
    );
    batches::seal(pool, id, "service-instance", values)
        .await
        .unwrap();
}
async fn usage(pool: &PgPool) -> Vec<i64> {
    sqlx::query_scalar("SELECT usage_bytes FROM users ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Exercise the billing transaction's own resource protection independently of
/// the legacy global-lock and shared read-model triggers scheduled for removal.
/// Native constraints (including item/ledger uniqueness and FKs) remain active.
async fn isolate_source_writes(pool: &PgPool) {
    sqlx::raw_sql("DO $$ DECLARE relation record; BEGIN FOR relation IN SELECT tablename FROM pg_tables WHERE schemaname=current_schema() LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER USER',relation.tablename); END LOOP; END $$;")
        .execute(pool).await.unwrap();
    // This is business state used by durable revocation fencing, rather than
    // a shared read model: keep it active even in the old contention fixture.
    sqlx::query("ALTER TABLE users ENABLE TRIGGER track_access_restriction_epoch")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE users ENABLE TRIGGER schedule_changed_user_access_check")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("ALTER TABLE users ENABLE TRIGGER schedule_new_user_access_check")
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn checkpoint_commits_at_most_500_items_and_another_worker_resumes_without_double_billing() {
    let pool = fixture(1001).await;
    capture(&pool, "sample", "a", &counters(1001, 7, 3)).await;
    assert_eq!(
        batches::process(&pool, "sample").await.unwrap(),
        Progress::Committed {
            items: 500,
            complete: false
        }
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT processed_count FROM traffic_sample_batches WHERE sample_id='sample'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        500
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM traffic_records WHERE sample_id='sample'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        500
    );
    // Drop the original execution's state and continue from persisted rows only.
    assert!(batches::drain(&pool.clone(), "sample").await.unwrap());
    assert_eq!(
        batches::process(&pool, "sample").await.unwrap(),
        Progress::Finished
    );
    batches::seal(&pool, "sample", "service-instance", &counters(1001, 7, 3))
        .await
        .unwrap();
    assert!(usage(&pool).await.iter().all(|value| *value == 10));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM traffic_records WHERE sample_id='sample'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1001
    );
}

#[tokio::test]
async fn duplicate_captures_and_parallel_processors_preserve_every_user_and_quota_obligation() {
    let pool = fixture(3).await;
    sqlx::query("UPDATE users SET quota_bytes=15")
        .execute(&pool)
        .await
        .unwrap();
    capture(&pool, "a1", "a", &counters(3, 10, 0)).await;
    capture(&pool, "b1", "b", &counters(3, 10, 0)).await;
    isolate_source_writes(&pool).await;
    let (a, b, c) = tokio::join!(
        batches::drain(&pool, "a1"),
        batches::drain(&pool, "b1"),
        batches::drain(&pool, "a1")
    );
    a.unwrap();
    b.unwrap();
    c.unwrap();
    batches::drain(&pool, "a1").await.unwrap();
    batches::drain(&pool, "b1").await.unwrap();
    assert_eq!(usage(&pool).await, vec![20; 3]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM background_work WHERE kind='access-revocation' AND state='queued'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        3
    );
    super::access::drain_for_test(&pool).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        6
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM kick_requests WHERE state='active'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        6
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind='kick'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        6
    );
}

#[tokio::test]
async fn later_ready_sample_waits_for_an_earlier_capture_and_expiry_does_not_discard_ready_data() {
    let pool = fixture(1).await;
    batches::reserve(&pool, "early", "a", None).await.unwrap();
    capture(&pool, "later", "a", &counters(1, 100, 0)).await;
    assert_eq!(
        batches::process(&pool, "later").await.unwrap(),
        Progress::Waiting
    );
    batches::seal(&pool, "early", "service-instance", &counters(1, 100, 0))
        .await
        .unwrap();
    sqlx::query("UPDATE traffic_sample_batches SET capture_deadline=now()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    batches::expire_captures(&pool).await.unwrap();
    batches::drain(&pool, "early").await.unwrap();
    batches::drain(&pool, "later").await.unwrap();
    assert_eq!(usage(&pool).await, vec![200]);
    let deltas: Vec<i64> =
        sqlx::query_scalar("SELECT delta_tx FROM traffic_records ORDER BY baseline_tx")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(deltas, vec![100, 100]);
    assert!(
        batches::seal(&pool, "early", "service-instance", &counters(1, 99, 0))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn expired_capture_cannot_overwrite_newer_baselines_and_saturation_is_atomic() {
    let pool = fixture(1).await;
    batches::reserve(&pool, "stale", "a", None).await.unwrap();
    sqlx::query("UPDATE traffic_sample_batches SET capture_deadline=now()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    batches::expire_captures(&pool).await.unwrap();
    capture(&pool, "fresh", "a", &counters(1, 200, 0)).await;
    sqlx::query("UPDATE users SET usage_bytes=9223372036854775800")
        .execute(&pool)
        .await
        .unwrap();
    batches::drain(&pool, "fresh").await.unwrap();
    assert!(
        batches::seal(&pool, "stale", "service-instance", &counters(1, 100, 0))
            .await
            .is_err()
    );
    assert_eq!(usage(&pool).await, vec![i64::MAX]);
    let baseline: i64 =
        sqlx::query_scalar("SELECT tx_total FROM traffic_baselines WHERE node_id='a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(baseline, 200);
}

#[tokio::test]
async fn failed_chunk_rolls_back_usage_baselines_records_and_item_checkpoint() {
    let pool = fixture(3).await;
    capture(&pool, "sample", "a", &counters(3, 5, 5)).await;
    sqlx::raw_sql("CREATE FUNCTION reject_billing() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.user_id='user-0002' THEN RAISE EXCEPTION 'injected billing failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_billing BEFORE INSERT ON traffic_records FOR EACH ROW EXECUTE FUNCTION reject_billing();")
        .execute(&pool).await.unwrap();
    assert!(batches::process(&pool, "sample").await.is_err());
    assert_eq!(usage(&pool).await, vec![0; 3]);
    let counts:(i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM traffic_records),(SELECT count(*) FROM traffic_baselines),(SELECT count(*) FROM traffic_sample_items WHERE processed_at IS NOT NULL),(SELECT processed_count FROM traffic_sample_batches WHERE sample_id='sample')")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 0));
    sqlx::query("DROP TRIGGER reject_billing ON traffic_records")
        .execute(&pool)
        .await
        .unwrap();
    batches::drain(&pool, "sample").await.unwrap();
    assert_eq!(usage(&pool).await, vec![10; 3]);
}

#[tokio::test]
async fn malformed_or_conflicting_payloads_never_replace_a_captured_batch() {
    let pool = fixture(2).await;
    batches::reserve(&pool, "sample", "a", None).await.unwrap();
    let bad = Map::from_iter([
        ("user-0001".into(), json!({"tx":10,"rx":0})),
        ("user-0002".into(), json!({"tx":"bad","rx":0})),
    ]);
    assert!(
        batches::seal(&pool, "sample", "instance", &bad)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_sample_items")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let good = counters(2, 10, 0);
    batches::seal(&pool, "sample", "instance", &good)
        .await
        .unwrap();
    assert!(
        batches::seal(&pool, "sample", "different-instance", &good)
            .await
            .is_err()
    );
    batches::drain(&pool, "sample").await.unwrap();
    assert_eq!(usage(&pool).await, vec![10; 2]);
    let row=sqlx::query("SELECT item_count,processed_count,state FROM traffic_sample_batches WHERE sample_id='sample'").fetch_one(&pool).await.unwrap();
    assert_eq!(row.get::<i64, _>("item_count"), 2);
    assert_eq!(row.get::<i64, _>("processed_count"), 2);
    assert_eq!(row.get::<String, _>("state"), "complete");
}

#[tokio::test]
async fn capture_filters_unbillable_identities_after_validating_and_hashing_every_counter() {
    let pool = fixture(3).await;
    sqlx::query("UPDATE users SET enabled=false WHERE id='user-0001'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM node_assignments WHERE node_id='a' AND user_id='user-0002'")
        .execute(&pool)
        .await
        .unwrap();
    let probe = format!("probe-a-{}", crate::security::token_digest("probe"));
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES($1,'Probe',now(),now())")
        .bind(&probe)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES($1,'a','probe','secret',now())")
        .bind(&probe).execute(&pool).await.unwrap();
    let mut values = Map::from_iter([
        ("user-0001".into(), json!({"tx":10,"rx":0})),
        ("user-0002".into(), json!({"tx":20,"rx":0})),
        ("deleted".into(), json!({"tx":30,"rx":0})),
        (probe.clone(), json!({"tx":40,"rx":0})),
    ]);
    batches::reserve(&pool, "filtered", "a", None)
        .await
        .unwrap();
    values.insert("deleted".into(), json!({"tx":"bad","rx":0}));
    assert!(
        batches::seal(&pool, "filtered", "instance", &values)
            .await
            .is_err()
    );
    values.insert("deleted".into(), json!({"tx":30,"rx":0}));
    batches::seal(&pool, "filtered", "instance", &values)
        .await
        .unwrap();
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT user_id FROM traffic_sample_items WHERE sample_id='filtered'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(ids, vec!["user-0001"]);
    values.insert("deleted".into(), json!({"tx":31,"rx":0}));
    assert!(
        batches::seal(&pool, "filtered", "instance", &values)
            .await
            .is_err()
    );
    batches::drain(&pool, "filtered").await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id='user-0001'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        10
    );

    let empty = Map::from_iter([("deleted".into(), json!({"tx":1,"rx":0}))]);
    batches::reserve(&pool, "empty", "a", None).await.unwrap();
    batches::seal(&pool, "empty", "instance", &empty)
        .await
        .unwrap();
    batches::drain(&pool, "empty").await.unwrap();
    let state: (i64, i64, String) = sqlx::query_as("SELECT item_count,processed_count,state FROM traffic_sample_batches WHERE sample_id='empty'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(state, (0, 0, "complete".into()));
}

#[tokio::test]
async fn stale_counters_checkpoint_across_pages_and_membership_is_rechecked_per_page() {
    let pool = fixture(503).await;
    let mut values = counters(503, 7, 3);
    values.insert("user-9999".into(), json!({"tx":7,"rx":3}));
    capture(&pool, "stale-counters", "a", &values).await;
    // Other-node assignments must not authorize billing on this node. Include
    // a counter for a nonexistent user, as real nodes can retain old counters.
    sqlx::query("DELETE FROM node_assignments WHERE node_id='a' AND user_id NOT IN ('user-0001','user-0501','user-0503')")
        .execute(&pool).await.unwrap();
    assert_eq!(
        batches::process(&pool, "stale-counters").await.unwrap(),
        Progress::Committed {
            items: 500,
            complete: false
        }
    );
    let first = usage(&pool).await;
    assert_eq!(first[0], 10);
    assert!(first[1..].iter().all(|value| *value == 0));
    sqlx::query("DELETE FROM node_assignments WHERE node_id='a' AND user_id='user-0501'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        batches::process(&pool, "stale-counters").await.unwrap(),
        Progress::Committed {
            items: 3,
            complete: true
        }
    );
    let final_usage = usage(&pool).await;
    assert_eq!(final_usage[0], 10);
    assert_eq!(final_usage[502], 10);
    assert!(final_usage[1..502].iter().all(|value| *value == 0));
    let counts: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM traffic_records),(SELECT count(*) FROM traffic_baselines),(SELECT count(*) FROM traffic_sample_items WHERE processed_at IS NOT NULL),(SELECT processed_count FROM traffic_sample_batches WHERE sample_id='stale-counters')")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (2, 2, 503, 503));
    assert_eq!(
        batches::process(&pool, "stale-counters").await.unwrap(),
        Progress::Finished
    );
    assert_eq!(usage(&pool).await, final_usage);
}

async fn reset_user(pool: &PgPool) -> Result<(), crate::error::ApiError> {
    let state = crate::state::AppState::new(
        pool.clone(),
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    let request = serde_json::from_value(json!({"expected_revision":1})).unwrap();
    let _ = crate::api::users::reset_quota(
        axum::extract::State(state),
        axum::extract::Path("user-0001".into()),
        axum::Json(request),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn reset_between_chunks_preserves_checkpoints_and_charges_next_interval_to_new_period() {
    let pool = fixture(501).await;
    capture(&pool, "initial", "a", &counters(501, 100, 0)).await;
    assert_eq!(
        batches::process(&pool, "initial").await.unwrap(),
        Progress::Committed {
            items: 500,
            complete: false
        }
    );
    reset_user(&pool).await.unwrap();
    batches::drain(&pool, "initial").await.unwrap();
    capture(&pool, "next", "a", &counters(501, 50, 0)).await;
    batches::drain(&pool, "next").await.unwrap();
    let values = usage(&pool).await;
    assert_eq!(values[0], 50);
    assert!(values[1..].iter().all(|value| *value == 150));
    let baseline: i64 = sqlx::query_scalar(
        "SELECT tx_total FROM traffic_baselines WHERE node_id='a' AND user_id='user-0001'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(baseline, 50);
}

async fn await_advisory_waiter(pool: &PgPool, key: i64) {
    tokio::time::timeout(std::time::Duration::from_secs(3),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND classid=0 AND objid=$1::oid AND NOT granted)")
                .bind(key).fetch_one(pool).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("the real transaction must reach the injected gate");
}

#[tokio::test]
async fn contended_resets_follow_node_then_user_admission_order_in_both_directions() {
    for reset_first in [true, false] {
        let pool = fixture(1).await;
        capture(&pool, "initial", "a", &counters(1, 100, 0)).await;
        batches::drain(&pool, "initial").await.unwrap();
        capture(&pool, "next", "a", &counters(1, 100, 0)).await;
        isolate_source_writes(&pool).await;
        let key = rand::random_range(1_i64..i32::MAX as i64);
        let mut gate = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(key)
            .execute(&mut *gate)
            .await
            .unwrap();
        let trigger = if reset_first {
            format!(
                "CREATE FUNCTION billing_gate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.revision>OLD.revision THEN PERFORM pg_advisory_xact_lock({key}); END IF; RETURN NEW; END $$; CREATE TRIGGER billing_gate BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION billing_gate();"
            )
        } else {
            format!(
                "CREATE FUNCTION billing_gate() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({key}); RETURN NEW; END $$; CREATE TRIGGER billing_gate BEFORE INSERT ON traffic_records FOR EACH ROW EXECUTE FUNCTION billing_gate();"
            )
        };
        sqlx::raw_sql(sqlx::AssertSqlSafe(trigger.as_str()))
            .execute(&pool)
            .await
            .unwrap();
        if reset_first {
            let reset_pool = pool.clone();
            let reset = tokio::spawn(async move { reset_user(&reset_pool).await });
            await_advisory_waiter(&pool, key).await;
            // Reset owns node -> user. Billing must skip the busy node without
            // consuming a captured item, then resume it on the next scheduler pass.
            assert_eq!(
                batches::process(&pool, "next").await.unwrap(),
                Progress::Waiting
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT processed_count FROM traffic_sample_batches WHERE sample_id='next'"
                )
                .fetch_one(&pool)
                .await
                .unwrap(),
                0
            );
            gate.commit().await.unwrap();
            reset.await.unwrap().unwrap();
            assert!(batches::drain(&pool, "next").await.unwrap());
            assert_eq!(usage(&pool).await, vec![100]);
        } else {
            let billing_pool = pool.clone();
            let billing = tokio::spawn(async move { batches::drain(&billing_pool, "next").await });
            await_advisory_waiter(&pool, key).await;
            let reset_pool = pool.clone();
            let reset = tokio::spawn(async move { reset_user(&reset_pool).await });
            tokio::task::yield_now().await;
            gate.commit().await.unwrap();
            billing.await.unwrap().unwrap();
            reset.await.unwrap().unwrap();
            assert_eq!(usage(&pool).await, vec![0]);
        }
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT sum(delta_tx)::bigint FROM traffic_records")
                .fetch_one(&pool)
                .await
                .unwrap(),
            200
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT processed_count FROM traffic_sample_batches WHERE sample_id='next'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            1
        );
    }
}

#[tokio::test]
async fn timeout_after_capture_keeps_payload_and_quality_for_background_billing() {
    let pool = fixture(1).await;
    let cycle = uuid::Uuid::new_v4();
    let id = format!("{cycle}:a");
    sqlx::query("UPDATE nodes SET state='deployed' WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    crate::overview::record_online(
        &pool,
        "a",
        cycle,
        chrono::Utc::now(),
        1,
        Some(br#"{"user-0001":1}"#),
    )
    .await
    .unwrap();
    let before: String = sqlx::query_scalar(
        "SELECT traffic_status FROM online_samples WHERE node_id='a' AND cycle_id=$1",
    )
    .bind(cycle)
    .fetch_one(&pool)
    .await
    .unwrap();
    batches::reserve(&pool, &id, "a", Some(cycle))
        .await
        .unwrap();
    batches::seal(&pool, &id, "service-instance", &counters(1, 80, 20))
        .await
        .unwrap();
    super::record_failed_sample(
        &pool,
        "a",
        cycle,
        chrono::Utc::now(),
        1,
        &anyhow::anyhow!("caller timed out during persistence"),
    )
    .await
    .unwrap();
    let after: String = sqlx::query_scalar(
        "SELECT traffic_status FROM online_samples WHERE node_id='a' AND cycle_id=$1",
    )
    .bind(cycle)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(after, before);
    assert!(batches::captured(&pool, &id).await.unwrap());
    super::work::initialize(&pool).await.unwrap();
    let state = crate::state::AppState::new(
        pool.clone(),
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    let work = crate::background::claim(&pool, "traffic", &state.instance_id)
        .await
        .unwrap()
        .unwrap();
    crate::execution::supervise(pool.clone(), work.leases.clone(), async {
        super::sample_work(&state, &work).await.unwrap();
        crate::background::finish(&pool, &work, true).await.unwrap();
    })
    .await;
    assert_eq!(usage(&pool).await, vec![100]);
    let after: String = sqlx::query_scalar(
        "SELECT traffic_status FROM online_samples WHERE node_id='a' AND cycle_id=$1",
    )
    .bind(cycle)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(after, "ok");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM data_gaps")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn node_cleanup_purges_pending_items_and_batches_in_its_durable_cursor() {
    let pool = fixture(501).await;
    capture(&pool, "a-sample", "a", &counters(501, 10, 0)).await;
    batches::process(&pool, "a-sample").await.unwrap();
    capture(&pool, "b-sample", "b", &counters(1, 5, 0)).await;
    batches::drain(&pool, "b-sample").await.unwrap();
    sqlx::query("UPDATE users SET quota_bytes=5 WHERE id='user-0501'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET state='deleting',desired_revision=2 WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,attempts,available_at,created_at,updated_at) VALUES('cleanup','record-remove','a',2,'running','starting',1,now(),now(),now())").execute(&pool).await.unwrap();
    let job = crate::deployment::JobInput {
        id: "cleanup".into(),
        kind: "record-remove".into(),
        node_id: Some("a".into()),
        target_revision: Some(2),
        payload: json!({}),
        attempts: 1,
    };
    crate::node_cleanup::purge_history(&pool, &job)
        .await
        .unwrap();
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
    // Late callbacks cannot repopulate a node after its cleanup cursor passed.
    let late_cycle = uuid::Uuid::new_v4();
    crate::overview::record_online(
        &pool,
        "a",
        late_cycle,
        chrono::Utc::now(),
        1,
        Some(br#"{"user-0001":1}"#),
    )
    .await
    .unwrap();
    super::record_failed_sample(
        &pool,
        "a",
        late_cycle,
        chrono::Utc::now(),
        1,
        &anyhow::anyhow!("late callback"),
    )
    .await
    .unwrap();
    super::record_gap(&pool, "a", "late callback")
        .await
        .unwrap();
    assert!(
        batches::reserve(&pool, "too-late", "a", Some(late_cycle))
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM online_samples WHERE node_id='a'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM data_gaps WHERE node_id='a'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM traffic_sample_items WHERE node_id='a'),(SELECT count(*) FROM traffic_sample_batches WHERE node_id='a'),(SELECT count(*) FROM traffic_records WHERE node_id='a')").fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_records WHERE node_id='b'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let values = usage(&pool).await;
    assert_eq!(values[0], 15);
    assert_eq!(values[500], 10);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM kick_requests WHERE node_id='a' AND user_id='user-0501' AND state='active'").fetch_one(&pool).await.unwrap(),1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind='kick' AND node_id='a'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind='kick' AND node_id='b'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn an_unrelated_node_commits_while_another_billing_transaction_is_paused() {
    let pool = fixture(2).await;
    capture(&pool, "a1", "a", &counters(1, 10, 0)).await;
    let other = Map::from_iter([("user-0002".into(), json!({"tx":10,"rx":0}))]);
    capture(&pool, "b1", "b", &other).await;
    isolate_source_writes(&pool).await;
    let key = rand::random_range(1_i64..i32::MAX as i64);
    let mut gate = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(&mut *gate)
        .await
        .unwrap();
    let trigger = format!(
        "CREATE FUNCTION pause_one_node() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.node_id='a' THEN PERFORM pg_advisory_xact_lock({key}); END IF; RETURN NEW; END $$; CREATE TRIGGER pause_one_node BEFORE INSERT ON traffic_records FOR EACH ROW EXECUTE FUNCTION pause_one_node();"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(trigger.as_str()))
        .execute(&pool)
        .await
        .unwrap();
    let blocked_pool = pool.clone();
    let blocked = tokio::spawn(async move { batches::drain(&blocked_pool, "a1").await });
    await_advisory_waiter(&pool, key).await;
    let peer = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        batches::drain(&pool, "b1"),
    )
    .await;
    assert!(
        peer.expect("unrelated resource must commit before the paused transaction resumes")
            .unwrap()
    );
    assert!(!blocked.is_finished());
    gate.rollback().await.unwrap();
    blocked.await.unwrap().unwrap();
    assert_eq!(usage(&pool).await, vec![10, 10]);
}
