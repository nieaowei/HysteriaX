use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn fixture() -> (PgPool, Execution) {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let mut tx = pool.begin().await.unwrap();
    let lease = acquire(&mut tx, "node:a", "instance-a", "work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    (pool, lease)
}

#[tokio::test]
async fn a_guarded_database_batch_does_not_lose_ownership_to_its_own_renewal_lock_wait() {
    let (pool, lease) = fixture().await;
    sqlx::query("CREATE TABLE guarded_progress(value bigint)")
        .execute(&pool)
        .await
        .unwrap();
    let result = supervise(
        pool.clone(),
        vec![lease.clone()],
        crate::db::write::<_, anyhow::Error, _>(&pool, |tx| {
            Box::pin(async move {
                sqlx::query("SELECT pg_sleep(13)")
                    .execute(&mut **tx)
                    .await?;
                sqlx::query("INSERT INTO guarded_progress VALUES(42)")
                    .execute(&mut **tx)
                    .await?;
                Ok(42)
            })
        }),
    )
    .await;
    assert_eq!(result.unwrap().unwrap(), 42);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM guarded_progress WHERE value=42")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert!(lease.renew(&pool).await.unwrap());
}

#[tokio::test]
async fn renewal_contention_still_observes_committed_job_cancellation() {
    let (pool, node) = fixture().await;
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('work','sync','running','running',now(),now(),now())").execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let job = acquire(&mut tx, "job:work", "instance-a", "work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let (guarded, ready) = tokio::sync::oneshot::channel();
    let other = pool.clone();
    let cancel = tokio::spawn(async move {
        ready.await.unwrap();
        sqlx::query("UPDATE jobs SET status='cancelled',stage='cancelled' WHERE id='work'")
            .execute(&other)
            .await
            .unwrap();
    });
    let writes = Arc::new(AtomicUsize::new(0));
    let counter = writes.clone();
    let started = tokio::time::Instant::now();
    let result = supervise(pool.clone(), vec![job, node.clone()], async {
        let mut held = pool.begin().await.unwrap();
        // Hold only the node lease to make renewal contend while the job's
        // persisted cancellation remains independently writable.
        node.guard(&mut held).await.unwrap();
        guarded.send(()).unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
        counter.fetch_add(1, Ordering::SeqCst);
        held.commit().await.unwrap();
    })
    .await;
    cancel.await.unwrap();
    assert!(result.is_none());
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
    assert_eq!(writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn persisted_cancellation_prevents_new_dispatch_even_with_a_live_lease() {
    let (pool, node) = fixture().await;
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('work','sync','cancelled','cancelled',now(),now(),now())")
            .execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let job = acquire(&mut tx, "job:work", "instance-a", "work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let sends = Arc::new(AtomicUsize::new(0));
    let counter = sends.clone();
    let result = supervise(
        pool.clone(),
        vec![job, node],
        remote(
            "ssh-command",
            "digest".into(),
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |_| serde_json::json!({"acknowledged":true}),
        ),
    )
    .await;
    assert!(result.is_none(), "cancelled work must not be polled at all");
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM execution_operations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn lost_response_persists_intent_and_prevents_another_send() {
    let (pool, lease) = fixture().await;
    let sends = Arc::new(AtomicUsize::new(0));
    let counter = sends.clone();
    let outcome = supervise(
        pool.clone(),
        vec![lease.clone()],
        remote(
            "ssh-command",
            "request-digest".into(),
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(anyhow::anyhow!("response lost after remote execution"))
            },
            |_| serde_json::json!({"complete":true}),
        ),
    )
    .await;
    assert!(outcome.unwrap().is_err());
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    let state: String =
        sqlx::query_scalar("SELECT state FROM execution_operations WHERE work_id='work'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "dispatching");
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert!(
        acquire(&mut tx, "node:a", "instance-b", "work")
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    let counter = sends.clone();
    let outcome = supervise(
        pool.clone(),
        vec![lease],
        remote(
            "ssh-command",
            "request-digest".into(),
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |_| serde_json::json!({"complete":true}),
        ),
    )
    .await;
    assert!(outcome.is_none());
    assert_eq!(sends.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn acknowledged_request_is_not_sent_again_when_work_is_reentered() {
    let (pool, lease) = fixture().await;
    let sends = Arc::new(AtomicUsize::new(0));
    for expected in [true, false] {
        let counter = sends.clone();
        let outcome = supervise(
            pool.clone(),
            vec![lease.clone()],
            remote(
                "dns-request",
                "request-digest".into(),
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                |_| serde_json::json!({"provider_receipt":"stable"}),
            ),
        )
        .await;
        assert_eq!(outcome.unwrap().is_ok(), expected);
    }
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    let (state, evidence): (String, Value) =
        sqlx::query_as("SELECT state,evidence FROM execution_operations WHERE work_id='work'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "verified");
    assert_eq!(evidence["provider_receipt"], "stable");
}

#[tokio::test]
async fn catching_an_uncertain_response_does_not_permit_a_later_cleanup_or_rollback_send() {
    let (pool, lease) = fixture().await;
    let sends = Arc::new(AtomicUsize::new(0));
    let result = supervise(pool.clone(), vec![lease.clone()], async {
        let first = remote(
            "ssh-command",
            "install".into(),
            async {
                sends.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(anyhow::anyhow!("response lost"))
            },
            |_| serde_json::json!({}),
        )
        .await;
        assert!(first.is_err());
        for kind in ["ssh-cleanup", "ssh-rollback", "dns-update"] {
            let next = remote(
                kind,
                "later".into(),
                async {
                    sends.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                |_| serde_json::json!({}),
            )
            .await;
            assert!(next.is_err());
        }
    })
    .await;
    assert!(result.is_some());
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM execution_operations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let mut tx = pool.begin().await.unwrap();
    assert!(!lease.release(&mut tx).await.unwrap());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn uncertainty_on_any_resource_of_the_same_work_blocks_a_new_step() {
    let (pool, lease) = fixture().await;
    let mut tx = pool.begin().await.unwrap();
    let other = acquire(&mut tx, "dns-zone:z", "instance-a", "work")
        .await
        .unwrap()
        .unwrap();
    other
        .dispatch(&mut tx, "prior", &serde_json::json!({"request":"first"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    for state in ["dispatching", "reconciling", "needs_attention"] {
        sqlx::query("UPDATE execution_operations SET state=$1")
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert!(
            lease
                .dispatch(&mut tx, "next", &serde_json::json!({}))
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM execution_operations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn verified_steps_continue_sequentially_and_uncertainty_does_not_block_other_resources() {
    let (pool, lease) = fixture().await;
    let mut tx = pool.begin().await.unwrap();
    let unrelated = acquire(&mut tx, "node:b", "instance-b", "other-work")
        .await
        .unwrap()
        .unwrap();
    unrelated
        .dispatch(&mut tx, "unresolved", &serde_json::json!({}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let sends = Arc::new(AtomicUsize::new(0));
    let result = supervise(pool.clone(), vec![lease], async {
        for step in ["upload", "install", "cleanup"] {
            remote(
                "ssh-command",
                step.into(),
                async {
                    sends.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                |_| serde_json::json!({"finished":true}),
            )
            .await
            .unwrap();
        }
    })
    .await;
    assert!(result.is_some());
    assert_eq!(sends.load(Ordering::SeqCst), 3);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM execution_operations WHERE work_id='work' AND state='verified'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        3
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM execution_operations WHERE work_id='other-work' AND state='dispatching'").fetch_one(&pool).await.unwrap(),1);
}
