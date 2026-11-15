use super::super::poll_tests::{Statements, recorded};
use super::*;
use crate::{background::maintenance, db, execution};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn credential_supervisor_future_fits_the_default_runtime_stack_budget() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://fixture:fixture@127.0.0.1:1/unused")
        .unwrap();
    let state = crate::state::AppState::new(
        pool,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    let worker = crate::credentials::maintenance::run(state);
    let bytes = std::mem::size_of_val(&worker);
    println!("credential supervisor future: {bytes} bytes");
    // Leave headroom for nested poll frames and ordinary stack locals on the
    // default 2 MiB Tokio worker stack, including debug and optimized builds.
    assert!(
        bytes < 512 * 1024,
        "credential supervisor future is {bytes} bytes"
    );
}

async fn future_work(pool: &PgPool, key: &str, kind: &str) {
    maintenance::ensure(pool, key, kind, json!({}))
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET state='complete',available_at=clock_timestamp()+interval '1 hour' WHERE id=$1")
        .bind(format!("maintenance:{key}")).execute(pool).await.unwrap();
}

#[tokio::test]
async fn many_idle_workers_share_one_database_probe() {
    let pool = db::test_pool_with_max_connections(4).await;
    let names: Vec<_> = (0..32).map(|i| format!("idle-{i}")).collect();
    for name in &names {
        future_work(&pool, name, name).await;
    }
    let scheduler = Arc::new(Scheduler::new(pool.clone()));
    let statements = Statements::default();
    recorded(&statements, async {
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let names = names.clone();
            let scheduler = scheduler.clone();
            tasks.spawn(async move {
                let names: Vec<_> = names.iter().map(String::as_str).collect();
                assert!(
                    scheduler
                        .run::<()>(&names, &names, async {
                            anyhow::bail!("an idle worker must not be run")
                        })
                        .await
                        .unwrap()
                        .is_none()
                );
            });
        }
        while let Some(task) = tasks.join_next().await {
            task.unwrap();
        }
    })
    .await;
    let sql = statements.snapshot();
    assert_eq!(
        sql.iter()
            .filter(|s| s.trim_start().starts_with("SELECT") || s.trim_start().starts_with("WITH"))
            .count(),
        1,
        "{sql:?}"
    );
    assert!(
        !sql.iter().any(|s| s.trim_start().starts_with("BEGIN")),
        "idle dispatch must not open transactions"
    );
}

#[tokio::test]
async fn missing_recurring_key_runs_and_its_new_work_is_claimable_in_the_same_pass() {
    let pool = db::test_pool_with_max_connections(3).await;
    let scheduler = Scheduler::new(pool.clone());
    let work = scheduler
        .run(&["new-kind"], &["new-key"], async {
            maintenance::ensure(&pool, "new-key", "new-kind", json!({"value":7})).await?;
            maintenance::claim(&pool, "new-kind", "owner").await
        })
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(work.id, "maintenance:new-key");
    assert_eq!(work.payload["value"], 7);
}

#[tokio::test]
async fn a_long_running_pass_does_not_keep_using_expired_negative_hints() {
    let pool = db::test_pool_with_max_connections(3).await;
    maintenance::ensure(&pool, "trigger", "trigger", json!({}))
        .await
        .unwrap();
    future_work(&pool, "later", "later").await;
    let scheduler = Scheduler::new(pool.clone());
    let result = scheduler.run(&["trigger", "later"], &["later"], async {
        tokio::time::sleep(HINT_LIFETIME + Duration::from_millis(20)).await;
        sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE id='maintenance:later'")
            .execute(&pool).await?;
        maintenance::ensure(&pool, "later", "later", json!({})).await?;
        maintenance::claim(&pool, "later", "owner").await
    }).await.unwrap();
    assert!(result.flatten().is_some());
}

#[tokio::test]
async fn a_new_worker_registration_invalidates_an_existing_empty_snapshot() {
    let pool = db::test_pool_with_max_connections(3).await;
    future_work(&pool, "future", "future").await;
    let scheduler = Scheduler::new(pool.clone());
    assert!(
        scheduler
            .run(&["future"], &["future"], async { Ok(()) })
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        scheduler
            .run(&["new-kind"], &["new-key"], async { Ok(()) })
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn an_external_instance_advancing_a_deadline_is_seen_after_hint_expiry() {
    let pool = db::test_pool_with_max_connections(3).await;
    future_work(&pool, "future", "future").await;
    let scheduler = Scheduler::new(pool.clone());
    assert!(
        scheduler
            .run(&["future"], &["future"], async { Ok(()) })
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE id='maintenance:future'").execute(&pool).await.unwrap();
    tokio::time::sleep(HINT_LIFETIME + Duration::from_millis(20)).await;
    let result = scheduler
        .run(&["future"], &["future"], async {
            maintenance::ensure(&pool, "future", "future", json!({})).await?;
            maintenance::claim(&pool, "future", "other-instance").await
        })
        .await
        .unwrap();
    assert!(result.flatten().is_some());
}

#[tokio::test]
async fn a_stale_positive_hint_cannot_claim_future_work_or_another_live_owner() {
    let pool = db::test_pool_with_max_connections(4).await;
    maintenance::ensure(&pool, "race", "race", json!({}))
        .await
        .unwrap();
    let scheduler = Scheduler::new(pool.clone());
    scheduler.run(&["race"], &[], async {
        sqlx::query("UPDATE background_work SET available_at=clock_timestamp()+interval '1 hour' WHERE id='maintenance:race'").execute(&pool).await?;
        assert!(maintenance::claim(&pool, "race", "late").await?.is_none());
        sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE id='maintenance:race'").execute(&pool).await?;
        let winner = maintenance::claim(&pool, "race", "winner").await?.unwrap();
        assert_eq!(winner.leases[0].owner, "winner");
        assert!(maintenance::claim(&pool, "race", "loser").await?.is_none());
        Ok(())
    }).await.unwrap().unwrap();
}

#[tokio::test]
async fn scoped_resource_barriers_and_unknown_remote_outcomes_are_respected() {
    let pool = db::test_pool_with_max_connections(4).await;
    maintenance::ensure(
        &pool,
        "blocked",
        "blocked",
        json!({"lease_resources":["node:busy"]}),
    )
    .await
    .unwrap();
    db::write::<_,anyhow::Error,_>(&pool, |tx| Box::pin(async move {
        let lease = execution::acquire(tx, "node:busy", "external", "external-work").await?.unwrap();
        lease.dispatch_prepared(tx, "pending", &json!({}), "scheduler-pending").await?;
        sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key='node:busy'").execute(&mut **tx).await?;
        Ok(())
    })).await.unwrap();
    let scheduler = Scheduler::new(pool.clone());
    assert!(
        scheduler
            .run(&["blocked"], &[], async { Ok(()) })
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        maintenance::claim(&pool, "blocked", "new-owner")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_hint_cannot_suppress_queries_to_a_different_database_pool() {
    let pool = db::test_pool_with_max_connections(3).await;
    let other = db::test_pool_with_max_connections(3).await;
    maintenance::ensure(&pool, "trigger", "trigger", json!({}))
        .await
        .unwrap();
    future_work(&pool, "future", "future").await;
    maintenance::ensure(&other, "future", "future", json!({}))
        .await
        .unwrap();
    let scheduler = Scheduler::new(pool.clone());
    let result = scheduler
        .run(&["trigger", "future"], &["future"], async {
            maintenance::claim(&other, "future", "other-pool").await
        })
        .await
        .unwrap();
    assert!(result.flatten().is_some());
}

#[tokio::test]
async fn failed_and_cancelled_refreshes_do_not_publish_empty_hints() {
    let pool = db::test_pool_with_max_connections(1).await;
    let scheduler = Scheduler::new(pool.clone());
    let mut connection = pool.acquire().await.unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            scheduler.run(&["new"], &["new"], async { Ok(()) })
        )
        .await
        .is_err()
    );
    connection.return_to_pool().await;
    sqlx::raw_sql("ALTER TABLE background_work RENAME TO temporarily_unavailable_work")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        scheduler
            .run(&["new"], &["new"], async { Ok(()) })
            .await
            .is_err()
    );
    sqlx::raw_sql("ALTER TABLE temporarily_unavailable_work RENAME TO background_work")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        scheduler
            .run(&["new"], &["new"], async { Ok(()) })
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn an_active_execution_scope_is_never_delayed_by_a_negative_hint() {
    let pool = db::test_pool_with_max_connections(3).await;
    maintenance::ensure(&pool, "parent", "parent", json!({}))
        .await
        .unwrap();
    let work = maintenance::claim(&pool, "parent", "owner")
        .await
        .unwrap()
        .unwrap();
    let scheduler = Scheduler::new(pool.clone());
    let result = execution::supervise(pool.clone(), work.leases, async {
        scheduler.run(&["empty"], &[], async { Ok(42) }).await
    })
    .await
    .into_result()
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(result, Some(42));
}

#[tokio::test]
async fn due_work_after_ten_thousand_blocked_candidates_is_still_discovered() {
    let pool = db::test_pool_with_max_connections(4).await;
    sqlx::raw_sql("INSERT INTO background_work(id,kind,payload) SELECT 'blocked:'||i,'scale',jsonb_build_object('lease_resources',jsonb_build_array('node:blocked:'||i)) FROM generate_series(1,10000)i; INSERT INTO resource_leases(resource_key,blocked) SELECT 'node:blocked:'||i,true FROM generate_series(1,10000)i;").execute(&pool).await.unwrap();
    maintenance::ensure(&pool, "ready-tail", "scale", json!({}))
        .await
        .unwrap();
    let scheduler = Scheduler::new(pool.clone());
    let work = scheduler
        .run(&["scale"], &[], async {
            maintenance::claim(&pool, "scale", "winner").await
        })
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(work.id, "maintenance:ready-tail");
}

#[tokio::test]
async fn idle_readers_and_an_existing_sampling_cycle_do_not_start_transactions() {
    let pool = db::test_pool_with_max_connections(3).await;
    // Remove initial migration dirtiness before measuring an idle pass.
    crate::analytics::refresh(&pool).await.unwrap();
    let statements = Statements::default();
    recorded(&statements, async {
        crate::analytics::refresh(&pool).await.unwrap();
        assert_eq!(crate::background::expire(&pool).await.unwrap(), 0);
        assert!(crate::jobs::claim_next(&pool).await.unwrap().is_none());
        assert_eq!(crate::events::publish_pending(&pool).await.unwrap(), 0);
    })
    .await;
    let sql = statements.snapshot();
    assert!(!sql.is_empty());
    assert!(
        !sql.iter().any(|s| s.trim_start().starts_with("BEGIN")),
        "{sql:?}"
    );
    let analytics = sql
        .iter()
        .filter(|s| s.contains("analytics_dirty_cycles"))
        .count();
    assert_eq!(
        analytics, 1,
        "empty analytics uses one four-queue probe: {sql:?}"
    );

    // Avoid a flaky wall-clock boundary: retry if the DB crosses a sampling slot.
    for _ in 0..3 {
        let id = crate::background::ensure_cycle(&pool, "proxy")
            .await
            .unwrap();
        let statements = Statements::default();
        let repeated = recorded(&statements, crate::background::ensure_cycle(&pool, "proxy"))
            .await
            .unwrap();
        if repeated == id {
            assert!(
                !statements
                    .snapshot()
                    .iter()
                    .any(|s| s.trim_start().starts_with("BEGIN"))
            );
            return;
        }
    }
    panic!("database clock repeatedly crossed the sampling boundary");
}

const TEST_DEMAND: Demand = Demand {
    kind: "domain-scan",
    key: "domain-scan",
    predicate: "EXISTS(SELECT 1 FROM jobs WHERE status='queued')",
};

#[tokio::test]
async fn domain_hints_discover_external_writes_and_do_not_gate_child_work() {
    let pool = db::test_pool_with_max_connections(3).await;
    let scheduler = Scheduler::new(pool.clone());
    assert!(
        scheduler
            .run_when::<()>(
                &["domain-scan", "domain-child"],
                &["domain-scan"],
                TEST_DEMAND,
                async { anyhow::bail!("no business work") }
            )
            .await
            .unwrap()
            .is_none()
    );
    // An external instance writes only durable business data, with no local notification.
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('domain-job','sync','queued','queued',now(),now(),now())")
        .execute(&pool).await.unwrap();
    tokio::time::sleep(HINT_LIFETIME + Duration::from_millis(20)).await;
    let scan = scheduler
        .run_when(
            &["domain-scan", "domain-child"],
            &["domain-scan"],
            TEST_DEMAND,
            async {
                maintenance::ensure(&pool, "domain-scan", "domain-scan", json!({})).await?;
                maintenance::claim(&pool, "domain-scan", "owner").await
            },
        )
        .await
        .unwrap()
        .flatten()
        .unwrap();
    db::write::<_, anyhow::Error, _>(&pool, move |tx| {
        let scan = scan.clone();
        Box::pin(async move { scan.finish_on(tx, true, 1, None).await })
    })
    .await
    .unwrap();
    sqlx::query("UPDATE jobs SET status='succeeded' WHERE id='domain-job'")
        .execute(&pool)
        .await
        .unwrap();
    maintenance::ensure(&pool, "domain-child", "domain-child", json!({}))
        .await
        .unwrap();
    tokio::time::sleep(HINT_LIFETIME + Duration::from_millis(20)).await;
    let child = scheduler
        .run_when(
            &["domain-scan", "domain-child"],
            &["domain-scan"],
            TEST_DEMAND,
            async {
                assert!(
                    maintenance::claim(&pool, "domain-scan", "owner")
                        .await?
                        .is_none()
                );
                maintenance::claim(&pool, "domain-child", "owner").await
            },
        )
        .await
        .unwrap()
        .flatten();
    assert!(
        child.is_some(),
        "queued child work must bypass a negative domain hint"
    );
}

#[tokio::test]
async fn stage_refresh_preserves_ensured_work_and_suppresses_only_negative_children() {
    let pool = db::test_pool_with_max_connections(3).await;
    let scheduler = Scheduler::new(pool.clone());
    maintenance::ensure(&pool, "stage-trigger", "stage-trigger", json!({}))
        .await
        .unwrap();
    scheduler
        .run(
            &["stage-trigger", "stage-child", "stage-empty"],
            &["stage-child"],
            async {
                maintenance::ensure(&pool, "stage-child", "stage-child", json!({})).await?;
                tokio::time::sleep(HINT_LIFETIME + Duration::from_millis(20)).await;
                scheduler.refresh_hint().await?;
                let statements = Statements::default();
                recorded(&statements, async {
                    assert!(
                        maintenance::claim(&pool, "stage-empty", "owner")
                            .await?
                            .is_none()
                    );
                    Ok::<_, anyhow::Error>(())
                })
                .await?;
                assert!(
                    statements.snapshot().is_empty(),
                    "refreshed negative child must not query"
                );
                assert!(
                    maintenance::claim(&pool, "stage-child", "owner")
                        .await?
                        .is_some()
                );
                Ok(())
            },
        )
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_negative_domain_hint_still_recovers_an_expired_running_scanner() {
    let pool = db::test_pool_with_max_connections(3).await;
    maintenance::ensure(&pool, "domain-scan", "domain-scan", json!({}))
        .await
        .unwrap();
    let old = maintenance::claim(&pool, "domain-scan", "old-instance")
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1")
        .bind(&old.id).execute(&pool).await.unwrap();
    let scheduler = Scheduler::new(pool.clone());
    let recovered = scheduler
        .run_when(&["domain-scan"], &["domain-scan"], TEST_DEMAND, async {
            maintenance::claim(&pool, "domain-scan", "replacement").await
        })
        .await
        .unwrap()
        .flatten()
        .expect("expired running work must be recoverable without domain demand");
    assert!(recovered.leases[0].generation > old.leases[0].generation);
    assert_eq!(recovered.leases[0].owner, "replacement");
}

#[tokio::test]
async fn existing_cycle_probe_uses_the_unique_period_index_with_historical_cycles() {
    let pool = db::test_pool_with_max_connections(3).await;
    sqlx::query("INSERT INTO sampling_cycles(id,kind,period_start,deadline_at,expected_nodes) SELECT gen_random_uuid(),'network','2020-01-01'::timestamptz+i*interval '1 minute','2020-01-01'::timestamptz+(i+1)*interval '1 minute',0 FROM generate_series(1,10000)i")
        .execute(&pool).await.unwrap();
    crate::background::ensure_cycle(&pool, "network")
        .await
        .unwrap();
    sqlx::query("ANALYZE sampling_cycles")
        .execute(&pool)
        .await
        .unwrap();
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("SET plan_cache_mode=force_generic_plan")
        .execute(&mut *connection)
        .await
        .unwrap();
    let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE, FORMAT JSON) {}",
        crate::background::CURRENT_CYCLE
    )))
    .bind("network")
    .bind(60_f64)
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    let text = plan.to_string();
    assert!(
        text.contains("Index Cond") && text.contains("period_start"),
        "period equality must bound the index: {plan}"
    );
    assert!(
        !text.contains("Seq Scan"),
        "historical periods must not be walked: {plan}"
    );
}

const URGENT: Demand = Demand {
    kind: "urgent-test",
    key: "urgent-test",
    predicate: "EXISTS(SELECT 1 FROM jobs WHERE status='queued')",
};
#[tokio::test]
async fn external_business_work_wakes_a_completed_future_scanner_without_waiting_thirty_seconds() {
    let pool = db::test_pool_with_max_connections(4).await;
    future_work(&pool, URGENT.key, URGENT.kind).await;
    let scheduler = Scheduler::new(pool.clone());
    assert!(
        scheduler
            .run_urgent(&[URGENT.kind], &[URGENT.key], URGENT, async { Ok(()) })
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('urgent-arrival','sync','queued','queued',now(),now(),now())")
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::sleep(HINT_LIFETIME + Duration::from_millis(20)).await;
    let work = scheduler
        .run_urgent(&[URGENT.kind], &[URGENT.key], URGENT, async {
            maintenance::ensure_urgent(&pool, URGENT, json!({})).await?;
            maintenance::claim(&pool, URGENT.kind, "owner").await
        })
        .await
        .unwrap()
        .flatten()
        .unwrap();
    assert_eq!(work.id, "maintenance:urgent-test");
}
#[tokio::test]
async fn stale_business_readiness_cannot_advance_a_scanner_or_steal_retry_and_live_ownership() {
    let pool = db::test_pool_with_max_connections(4).await;
    future_work(&pool, URGENT.key, URGENT.kind).await;
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('urgent-arrival','sync','queued','queued',now(),now(),now())")
        .execute(&pool)
        .await
        .unwrap();
    let scheduler = Scheduler::new(pool.clone());
    scheduler
        .run_urgent(&[URGENT.kind], &[URGENT.key], URGENT, async {
            sqlx::query("DELETE FROM jobs").execute(&pool).await?;
            maintenance::ensure_urgent(&pool, URGENT, json!({})).await?;
            assert!(
                maintenance::claim(&pool, URGENT.kind, "stale")
                    .await?
                    .is_none()
            );
            Ok(())
        })
        .await
        .unwrap()
        .unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('urgent-arrival','sync','queued','queued',now(),now(),now())")
        .execute(&pool)
        .await
        .unwrap();
    for state in ["queued", "failed"] {
        sqlx::query("UPDATE background_work SET state=$1,available_at=clock_timestamp()+interval '1 hour' WHERE id='maintenance:urgent-test'").bind(state).execute(&pool).await.unwrap();
        maintenance::ensure_urgent(&pool, URGENT, json!({}))
            .await
            .unwrap();
        assert!(
            maintenance::claim(&pool, URGENT.kind, "retry-thief")
                .await
                .unwrap()
                .is_none()
        );
    }
    sqlx::query("UPDATE background_work SET state='complete'")
        .execute(&pool)
        .await
        .unwrap();
    maintenance::ensure_urgent(&pool, URGENT, json!({}))
        .await
        .unwrap();
    let owner = maintenance::claim(&pool, URGENT.kind, "winner")
        .await
        .unwrap()
        .unwrap();
    maintenance::ensure_urgent(&pool, URGENT, json!({}))
        .await
        .unwrap();
    assert!(
        maintenance::claim(&pool, URGENT.kind, "loser")
            .await
            .unwrap()
            .is_none()
    );
    let actual: (String, i64) =
        sqlx::query_as("SELECT owner,generation FROM background_work WHERE id=$1")
            .bind(&owner.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(actual, ("winner".into(), owner.leases[0].generation));
}
