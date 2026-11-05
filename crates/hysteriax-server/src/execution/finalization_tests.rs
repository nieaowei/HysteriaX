use super::finalization::Phase;
use super::finalization_test_relay::CommitRelay;
use super::*;
use crate::{background::maintenance, db};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

async fn fixture() -> (PgPool, maintenance::Work) {
    let pool = db::test_pool_with_max_connections(6).await;
    maintenance::ensure(&pool, "finish", "finish", serde_json::json!({}))
        .await
        .unwrap();
    let work = maintenance::claim(&pool, "finish", "owner")
        .await
        .unwrap()
        .unwrap();
    (pool, work)
}
async fn finish(pool: &PgPool, work: &maintenance::Work, more: bool) -> anyhow::Result<()> {
    let work = work.clone();
    db::write(pool, move |tx| {
        let work = work.clone();
        Box::pin(async move {
            work.finish_on(tx, more, if more { 0 } else { 86400 }, None)
                .await
        })
    })
    .await
}
async fn state(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT state FROM background_work WHERE id='maintenance:finish'")
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn fast<T>(future: impl std::future::Future<Output = T>) -> T {
    finalization::ACK_TIMEOUT
        .scope(
            Duration::from_secs(20),
            supervision::RENEW_INTERVAL.scope(Duration::from_millis(10), future),
        )
        .await
}

#[tokio::test]
async fn committed_completion_survives_delayed_ack_and_immediate_takeover() {
    for more in [false, true] {
        let (direct, work) = fixture().await;
        let relay = CommitRelay::new(&direct).await;
        let (pool, gate, original) = (relay.pool.clone(), relay.gate.clone(), work.clone());
        let run = tokio::spawn(fast(async move {
            supervise(pool.clone(), work.leases.clone(), async {
                gate.arm(false);
                finish(&pool, &work, more).await.unwrap();
                assert!(
                    db::write::<_, sqlx::Error, _>(&pool, |tx| Box::pin(async move {
                        sqlx::query("SELECT 1").execute(&mut **tx).await?;
                        Ok(())
                    }))
                    .await
                    .is_err()
                );
                assert!(check_current().await.is_err());
                42
            })
            .await
        }));
        tokio::time::timeout(Duration::from_secs(20), relay.gate.seen.notified())
            .await
            .unwrap();
        assert_eq!(
            state(&direct).await,
            if more { "queued" } else { "complete" }
        );
        assert!(!original.leases[0].renew(&direct).await.unwrap());
        let replacement = if more {
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    if let Some(next) = maintenance::claim(&direct, "finish", "replacement")
                        .await
                        .unwrap()
                    {
                        break next.leases[0].clone();
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("completed work must become claimable")
        } else {
            replacement_lease(&direct, &original.leases[0].resource, "replacement-work").await
        };
        assert!(replacement.generation > original.leases[0].generation);
        relay.gate.release.notify_one();
        assert_eq!(run.await.unwrap(), Supervised::Returned(42));
        assert!(replacement.renew(&direct).await.unwrap());
    }
}

#[tokio::test]
async fn renewal_blocked_by_its_own_finish_transaction_preserves_the_committed_result() {
    let (direct, work) = fixture().await;
    let relay = CommitRelay::new(&direct).await;
    let pool = relay.pool.clone();
    let gate = relay.gate.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let backend = Arc::new(AtomicUsize::new(0));
    let (ready, proceed, pid) = (entered.clone(), resume.clone(), backend.clone());
    let task = tokio::spawn(fast(async move {
        supervise(pool.clone(), work.leases.clone(), async {
            gate.arm(false);
            db::write::<_, anyhow::Error, _>(&pool, |tx| {
                let (work, ready, proceed, pid) =
                    (work.clone(), ready.clone(), proceed.clone(), pid.clone());
                Box::pin(async move {
                    let value: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                        .fetch_one(&mut **tx)
                        .await?;
                    work.finish_on(tx, false, 86400, None).await?;
                    pid.store(value as usize, Ordering::SeqCst);
                    ready.notify_one();
                    proceed.notified().await;
                    Ok(42)
                })
            })
            .await
        })
        .await
    }));
    tokio::time::timeout(Duration::from_secs(20), entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity a WHERE $1=ANY(pg_blocking_pids(a.pid)) AND a.query LIKE 'UPDATE resource_leases SET expires_at=%')",
            )
            .bind(backend.load(Ordering::SeqCst) as i32)
            .fetch_one(&direct)
            .await
            .unwrap();
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("renewal must actually wait on the finish transaction's lock");
    resume.notify_one();
    tokio::time::timeout(Duration::from_secs(20), relay.gate.seen.notified())
        .await
        .unwrap();
    assert_eq!(state(&direct).await, "complete");
    assert!(
        !task.is_finished(),
        "the commit acknowledgement is still held"
    );
    relay.gate.release.notify_one();
    assert_eq!(task.await.unwrap().unwrap().unwrap(), 42);
}

#[tokio::test]
async fn lost_commit_ack_is_recovery_required_even_when_database_committed() {
    let (direct, work) = fixture().await;
    let relay = CommitRelay::new(&direct).await;
    let gate = relay.gate.clone();
    let pool = relay.pool.clone();
    let continuation = Arc::new(AtomicUsize::new(0));
    let counter = continuation.clone();
    let sends = Arc::new(AtomicUsize::new(0));
    let sent = sends.clone();
    let result = fast(supervise(pool.clone(), work.leases.clone(), async {
        remote(
            "ssh-command",
            "recorded-command".into(),
            async {
                sent.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |_| serde_json::json!({"verified":true}),
        )
        .await?;
        gate.arm(true);
        finish(&pool, &work, false).await?;
        counter.fetch_add(1, Ordering::SeqCst);
        Ok::<_, anyhow::Error>(())
    }))
    .await;
    assert!(
        matches!(result, Supervised::RecoveryRequired(_)),
        "{result:?}"
    );
    assert_eq!(state(&direct).await, "complete");
    assert_eq!(continuation.load(Ordering::SeqCst), 0);
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM execution_operations")
            .fetch_one(&direct)
            .await
            .unwrap(),
        "verified"
    );
}

#[tokio::test]
async fn dropping_a_commit_future_records_uncertainty_without_releasing_replacement_resources() {
    let (direct, work) = fixture().await;
    let relay = CommitRelay::new(&direct).await;
    let (pool, gate) = (relay.pool.clone(), relay.gate.clone());
    let (send, receive) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(fast(async move {
        supervise(pool.clone(), work.leases.clone(), async {
            send.send(CURRENT.with(|s| s.lifecycle.clone().unwrap()))
                .ok();
            gate.arm(false);
            finish(&pool, &work, false).await
        })
        .await
    }));
    let lifecycle = receive.await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), relay.gate.seen.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(matches!(lifecycle.phase(), Phase::RecoveryRequired(_)));
    assert_eq!(state(&direct).await, "complete");
    relay.gate.release.notify_one();
    let replacement = replacement_lease(&direct, "work:maintenance:finish", "replacement").await;
    assert!(replacement.renew(&direct).await.unwrap());
}

#[tokio::test]
async fn an_intent_cannot_target_another_execution_or_another_transaction() {
    let (pool, work) = fixture().await;
    let result = fast(supervise(pool.clone(), work.leases.clone(), async {
        db::write::<_, sqlx::Error, _>(&pool, |tx| {
            let pool = pool.clone();
            Box::pin(async move {
                assert!(finish_current_on(tx, "work:another-work", Disposition::Complete).is_err());
                let mut other = pool.begin().await?;
                assert!(
                    finish_current_on(&mut other, "work:maintenance:finish", Disposition::Complete)
                        .is_err()
                );
                other.rollback().await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        assert_eq!(state(&pool).await, "running");
        check_current().await.unwrap();
        finish(&pool, &work, false).await.unwrap();
        42
    }))
    .await;
    assert_eq!(result, Supervised::Returned(42));
}

#[tokio::test]
async fn stalled_commit_ack_is_bounded_and_does_not_claim_success() {
    let (direct, work) = fixture().await;
    let relay = CommitRelay::new(&direct).await;
    let gate = relay.gate.clone();
    let pool = relay.pool.clone();
    let result = finalization::ACK_TIMEOUT
        .scope(
            Duration::from_millis(250),
            supervise(pool.clone(), work.leases.clone(), async {
                gate.arm(false);
                finish(&pool, &work, false).await
            }),
        )
        .await;
    assert!(
        matches!(result, Supervised::RecoveryRequired(_)),
        "{result:?}"
    );
    assert_eq!(state(&direct).await, "complete");
    relay.gate.release.notify_one();
}

#[tokio::test]
async fn deferred_commit_conflict_rearms_ownership_and_retries_without_duplicate_results() {
    let (pool, work) = fixture().await;
    sqlx::raw_sql("CREATE SEQUENCE finish_attempts; CREATE TABLE progress(value integer); CREATE FUNCTION conflict_once() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.id='maintenance:finish' AND NEW.state='complete' AND nextval('finish_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='commit conflict'; END IF; RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER finish_conflict AFTER UPDATE ON background_work DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION conflict_once();").execute(&pool).await.unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let result = fast(supervise(pool.clone(), work.leases.clone(), async {
        db::serializable::<_, anyhow::Error, _>(&pool, |tx| {
            let work = work.clone();
            let counter = counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                sqlx::query("INSERT INTO progress VALUES(42)")
                    .execute(&mut **tx)
                    .await?;
                work.finish_on(tx, false, 86400, None).await?;
                Ok(42)
            })
        })
        .await
    }))
    .await;
    assert_eq!(result.unwrap().unwrap(), 42);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM progress")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn rejected_result_rolls_back_the_intent_and_can_finish_on_the_next_attempt() {
    let (pool, work) = fixture().await;
    let result = fast(supervise(pool.clone(), work.leases.clone(), async {
        let rejected: anyhow::Result<()> = db::write::<_, anyhow::Error, _>(&pool, |tx| {
            let work = work.clone();
            Box::pin(async move {
                work.finish_on(tx, false, 86400, None).await?;
                anyhow::bail!("reject result")
            })
        })
        .await;
        assert!(rejected.is_err());
        assert_eq!(state(&pool).await, "running");
        check_current().await.unwrap();
        finish(&pool, &work, false).await.unwrap();
        42
    }))
    .await;
    assert_eq!(result, Supervised::Returned(42));
    assert_eq!(state(&pool).await, "complete");
}

#[tokio::test]
async fn a_savepoint_rollback_cannot_publish_a_stale_finish_intent() {
    let (pool, work) = fixture().await;
    let result = fast(supervise(pool.clone(), work.leases.clone(), async {
        let rejected = db::write::<_, anyhow::Error, _>(&pool, |tx| {
            let work = work.clone();
            Box::pin(async move {
                sqlx::query("SAVEPOINT before_finish")
                    .execute(&mut **tx)
                    .await?;
                work.finish_on(tx, false, 86400, None).await?;
                sqlx::query("ROLLBACK TO SAVEPOINT before_finish")
                    .execute(&mut **tx)
                    .await?;
                Ok(())
            })
        })
        .await;
        assert!(rejected.is_err());
        assert_eq!(state(&pool).await, "running");
        check_current().await.unwrap();
        finish(&pool, &work, false).await.unwrap();
        42
    }))
    .await;
    assert_eq!(result, Supervised::Returned(42));
}

#[tokio::test]
async fn unverified_remote_operation_prevents_finishing_and_preserves_all_ownership() {
    let (pool, work) = fixture().await;
    let mut tx = pool.begin().await.unwrap();
    work.leases[0]
        .dispatch(&mut tx, "remote", &serde_json::json!({"digest":"test"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let result = fast(supervise(
        pool.clone(),
        work.leases.clone(),
        finish(&pool, &work, false),
    ))
    .await;
    assert!(matches!(result, Supervised::Returned(Err(_))));
    assert_eq!(state(&pool).await, "running");
    assert!(work.leases[0].renew(&pool).await.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM execution_operations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        "dispatching"
    );
}

#[tokio::test]
async fn cancellation_without_a_local_finish_intent_still_stops_the_future() {
    let (pool, work) = fixture().await;
    let result = fast(supervise(pool.clone(), work.leases.clone(), async {
        sqlx::query("UPDATE background_work SET state='failed',owner=NULL,finished_at=clock_timestamp() WHERE id='maintenance:finish'")
            .execute(&pool)
            .await
            .unwrap();
        std::future::pending::<()>().await
    }))
    .await;
    assert!(matches!(result, Supervised::Lost(_)), "{result:?}");
}

async fn job_fixture() -> (PgPool, Vec<Execution>) {
    let pool = db::test_pool_with_max_connections(6).await;
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('job','sync','running','running',now(),now(),now())").execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let mut leases = vec![];
    for key in ["job:job", "node:node"] {
        leases.push(
            acquire(&mut tx, key, "owner", "job")
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    (pool, leases)
}
#[tokio::test]
async fn releasing_child_identity_keeps_parent_active_and_parent_finish_propagates() {
    for finish_in_child in [false, true] {
        let (pool, leases) = job_fixture().await;
        let result = fast(supervise(pool.clone(), leases.clone(), async {
            with_identity_leases(&["identity".into()], async {
                if finish_in_child {
                    db::write::<_, sqlx::Error, _>(&pool, |tx| {
                        Box::pin(async move {
                            sqlx::query("UPDATE jobs SET status='succeeded' WHERE id='job'")
                                .execute(&mut **tx)
                                .await?;
                            finish_current_on(tx, "job:job", Disposition::Complete)?;
                            Ok(())
                        })
                    })
                    .await?;
                }
                Ok(17)
            })
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT owner FROM resource_leases WHERE resource_key='identity:identity'"
                )
                .fetch_one(&pool)
                .await
                .unwrap(),
                None
            );
            if finish_in_child {
                assert!(matches!(
                    CURRENT.with(|s| s.lifecycle.as_ref().unwrap().phase()),
                    Phase::Committed { .. }
                ));
            } else {
                check_current().await.unwrap();
            }
            42
        }))
        .await;
        assert_eq!(result, Supervised::Returned(42));
        for lease in &leases {
            assert!(lease.renew(&pool).await.unwrap());
        }
    }
}

async fn replacement_lease(pool: &PgPool, resource: &str, work: &str) -> Execution {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut tx = pool.begin().await.unwrap();
            if let Some(next) = acquire(&mut tx, resource, "replacement", work)
                .await
                .unwrap()
            {
                tx.commit().await.unwrap();
                break next;
            }
            tx.rollback().await.unwrap();
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("released lease must become claimable after in-flight renewal exits")
}
