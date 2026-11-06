use super::*;

async fn blocked_by(pool: &sqlx::PgPool, pid: i32) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
    )
    .bind(pid)
    .fetch_one(pool)
    .await
    .unwrap()
    {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
}

async fn fixture() -> (sqlx::PgPool, Vec<Execution>) {
    let pool = crate::db::test_pool_with_max_connections(5).await;
    let mut tx = pool.begin().await.unwrap();
    let leases = acquire_all(&mut tx, &["work:b".into(), "node:a".into()], "owner", "b")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    (pool, leases)
}

#[tokio::test]
async fn lease_sets_reject_every_mixed_identity_and_epoch() {
    let (pool, leases) = fixture().await;
    for field in [
        "owner",
        "work",
        "generation",
        "epoch",
        "missing",
        "blocked",
        "expired",
    ] {
        let mut invalid = leases.clone();
        match field {
            "owner" => invalid[1].owner.push('x'),
            "work" => invalid[1].work.push('x'),
            "generation" => invalid[1].generation += 1,
            "epoch" => {
                // A matching stale epoch on ONE row must fail even if the other
                // epoch sorts before it and still matches the current database.
                invalid[1].epoch = "zz-stale".into();
                sqlx::query("UPDATE resource_leases SET epoch=$2 WHERE resource_key=$1")
                    .bind(&invalid[1].resource)
                    .bind(&invalid[1].epoch)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            "missing" => invalid[1].resource.push('x'),
            "blocked" => {
                sqlx::query("UPDATE resource_leases SET blocked=true WHERE resource_key=$1")
                    .bind(&invalid[1].resource)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            "expired" => {
                sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key=$1")
                .bind(&invalid[1].resource).execute(&pool).await.unwrap();
            }
            _ => unreachable!(),
        }
        let mut tx = pool.begin().await.unwrap();
        assert!(guard_all(&mut tx, &invalid).await.is_err(), "{field}");
        tx.rollback().await.unwrap();
        sqlx::query("UPDATE resource_leases SET epoch=$2,blocked=false,expires_at=clock_timestamp()+interval '1 hour' WHERE resource_key=$1")
            .bind(&leases[1].resource).bind(&leases[1].epoch).execute(&pool).await.unwrap();
    }
    let mut duplicate = leases.clone();
    duplicate.push(leases[0].clone());
    duplicate[2].generation += 1;
    let mut tx = pool.begin().await.unwrap();
    assert!(guard_all(&mut tx, &duplicate).await.is_err());
    tx.rollback().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    guard_all(&mut tx, &leases).await.unwrap();
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn a_busy_or_unverified_resource_cannot_partially_claim_a_set() {
    let (pool, leases) = fixture().await;
    let keys = vec![leases[0].resource.clone(), "work:new".into()];
    let mut tx = pool.begin().await.unwrap();
    assert!(
        acquire_all(&mut tx, &keys, "other", "new")
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
    assert!(
        !sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM resource_leases WHERE resource_key='work:new')"
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    );
    let mut tx = pool.begin().await.unwrap();
    leases[0]
        .dispatch(&mut tx, "uncertain", &serde_json::json!({}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        acquire_all(&mut tx, &keys, "other", "new")
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    let row: (bool, String) = sqlx::query_as("SELECT l.blocked,o.state FROM resource_leases l JOIN execution_operations o USING(resource_key)")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(row, (true, "reconciling".into()));
    assert!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT owner FROM resource_leases WHERE resource_key='work:new'"
        )
        .fetch_one(&pool)
        .await
        .unwrap()
        .is_none()
    );
}

#[tokio::test]
async fn waiting_for_the_last_lock_rechecks_the_first_leases_expiry() {
    let (pool, leases) = fixture().await;
    // The ordered guard locks node:a before waiting on work:b.
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()+interval '2 seconds' WHERE resource_key='node:a'")
        .execute(&pool).await.unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT resource_key FROM resource_leases WHERE resource_key='work:b' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let other = pool.clone();
    let pending = tokio::spawn(async move {
        let mut tx = other.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let result = guard_all(&mut tx, &leases).await;
        tx.rollback().await.unwrap();
        result
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
    )
    .bind(pid)
    .fetch_one(&pool)
    .await
    .unwrap()
    {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT expires_at>clock_timestamp() FROM resource_leases WHERE resource_key='node:a'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        "first lease must still be valid when the last lock starts waiting"
    );
    sqlx::query("SELECT pg_sleep(greatest(0,extract(epoch FROM (expires_at-clock_timestamp())))+0.05) FROM resource_leases WHERE resource_key='node:a'")
        .execute(&pool).await.unwrap();
    held.commit().await.unwrap();
    assert!(
        matches!(pending.await.unwrap(),Err(sqlx::Error::Protocol(message)) if message=="execution lease was lost")
    );
}

#[tokio::test]
async fn an_intent_committed_while_inserting_the_set_still_blocks_takeover() {
    let pool = crate::db::test_pool_with_max_connections(4).await;
    let mut held = pool.begin().await.unwrap();
    let lease = super::super::acquire(&mut held, "node:new", "old", "prior")
        .await
        .unwrap()
        .unwrap();
    lease
        .dispatch(&mut held, "pending", &serde_json::json!({}))
        .await
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key='node:new'")
        .execute(&mut *held).await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let other = pool.clone();
    let pending = tokio::spawn(async move {
        let mut tx = other.begin().await.unwrap();
        let result = acquire_all(
            &mut tx,
            &["node:new".into(), "work:new".into()],
            "new",
            "new",
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        result
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
    )
    .bind(pid)
    .fetch_one(&pool)
    .await
    .unwrap()
    {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    held.commit().await.unwrap();
    assert!(pending.await.unwrap().is_none());
    let state: (bool, String) = sqlx::query_as("SELECT l.blocked,o.state FROM resource_leases l JOIN execution_operations o USING(resource_key) WHERE l.resource_key='node:new'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(state, (true, "reconciling".into()));
}

#[tokio::test]
async fn a_locked_resource_is_skipped_without_partial_ownership() {
    let (pool, leases) = fixture().await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT resource_key FROM resource_leases WHERE resource_key='work:b' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let keys = leases
        .iter()
        .map(|l| l.resource.clone())
        .collect::<Vec<_>>();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        acquire_all(&mut tx, &keys, "new", "next"),
    )
    .await
    .expect("SKIP LOCKED acquisition waited for a busy resource")
    .unwrap();
    assert!(result.is_none());
    tx.rollback().await.unwrap();
    held.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource_leases WHERE owner='new'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn ownership_replaced_while_guard_waits_is_rejected() {
    let (pool, leases) = fixture().await;
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT resource_key FROM resource_leases WHERE resource_key='node:a' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let other = pool.clone();
    let pending = tokio::spawn(async move {
        let mut tx = other.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout='5s'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let result = guard_all(&mut tx, &leases).await;
        tx.rollback().await.unwrap();
        result
    });
    blocked_by(&pool, pid).await;
    sqlx::query("UPDATE resource_leases SET owner='replacement',generation=generation+1 WHERE resource_key='node:a'")
        .execute(&mut *held).await.unwrap();
    held.commit().await.unwrap();
    assert!(
        matches!(pending.await.unwrap(),Err(sqlx::Error::Protocol(message)) if message=="execution lease was lost")
    );
}

#[tokio::test]
async fn an_already_blocked_or_expired_identity_is_rejected_without_waiting_for_its_lock() {
    let (pool, leases) = fixture().await;
    for blocked in [true, false] {
        sqlx::query("UPDATE resource_leases SET blocked=$1,expires_at=CASE WHEN $1 THEN clock_timestamp()+interval '1 hour' ELSE clock_timestamp()-interval '1 second' END WHERE resource_key='node:a'")
            .bind(blocked).execute(&pool).await.unwrap();
        let mut held = pool.begin().await.unwrap();
        sqlx::query(
            "SELECT resource_key FROM resource_leases WHERE resource_key='node:a' FOR UPDATE",
        )
        .fetch_one(&mut *held)
        .await
        .unwrap();
        let mut tx = pool.begin().await.unwrap();
        let error = guard_all(&mut tx, &leases).await.unwrap_err();
        assert!(
            matches!(error,sqlx::Error::Protocol(message) if message=="execution lease was lost"),
            "must reject invalid authority, not time out waiting for its row"
        );
        tx.rollback().await.unwrap();
        held.rollback().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "explicit local sampling lease round-trip comparison; run alone through a latency proxy"]
async fn benchmark_sampling_lease_round_trips() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::{Layer, layer::SubscriberExt};
    struct Count(Arc<AtomicU64>);
    impl<S: tracing::Subscriber> Layer<S> for Count {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if event.metadata().target() == "sqlx::query" {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    let (pool, leases) = fixture().await;
    sqlx::query("INSERT INTO resource_leases(resource_key) SELECT 'history:'||n FROM generate_series(1,20000) n")
        .execute(&pool).await.unwrap();
    let keys: Vec<String> = vec!["node:claim".into(), "work:claim".into()];
    sqlx::query("INSERT INTO resource_leases(resource_key) SELECT unnest($1::text[])")
        .bind(&keys)
        .execute(&pool)
        .await
        .unwrap();
    let mut warmed = vec![];
    for _ in 0..3 {
        warmed.push(pool.acquire().await.unwrap());
    }
    for mut connection in warmed {
        connection.return_to_pool().await;
    }
    for scenario in ["claim", "guard"] {
        let mut durations = [vec![], vec![]];
        let mut queries = [0, 0];
        for repetition in 0..12 {
            for variant in if repetition % 2 == 0 { [0, 1] } else { [1, 0] } {
                let count = Arc::new(AtomicU64::new(0));
                let subscriber = tracing_subscriber::registry().with(Count(count.clone()));
                let started = std::time::Instant::now();
                async {
                    let mut tx = pool.begin().await.unwrap();
                    if scenario == "claim" {
                        let claimed = if variant == 0 {
                            let mut found = vec![];
                            for resource in &keys {
                                found.push(
                                    super::super::acquire(&mut tx, resource, "bench", "claim")
                                        .await
                                        .unwrap()
                                        .unwrap(),
                                );
                            }
                            found
                        } else {
                            acquire_all(&mut tx, &keys, "bench", "claim")
                                .await
                                .unwrap()
                                .unwrap()
                        };
                        assert_eq!(claimed.len(), 2);
                    } else if variant == 0 {
                        for lease in &leases {
                            lease.guard(&mut tx).await.unwrap();
                        }
                    } else {
                        guard_all(&mut tx, &leases).await.unwrap();
                    }
                    tx.rollback().await.unwrap();
                }
                .with_subscriber(subscriber)
                .await;
                // The first two pairs warm prepared statements on both paths.
                if repetition >= 2 {
                    durations[variant].push(started.elapsed().as_micros() as u64);
                    queries[variant] += count.load(Ordering::Relaxed);
                }
            }
        }
        assert!(queries[1] < queries[0]);
        for variant in 0..2 {
            durations[variant].sort_unstable();
            println!(
                "SAMPLING_LEASE_BENCHMARK {}",
                serde_json::json!({
                    "scenario":scenario,"variant":if variant==0{"individual"}else{"set"},
                    "iterations":durations[variant].len(),"queries":queries[variant],
                    "mean_us":durations[variant].iter().sum::<u64>() as f64/durations[variant].len() as f64,
                    "max_us":durations[variant].last(),"historical_resource_rows":20000,
                    "scope":"local two-resource claim/guard primitives; no SSH, production or capacity acceptance"
                })
            );
        }
    }
}
