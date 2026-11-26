use super::*;
use std::sync::{Arc, Mutex};
use tracing::Subscriber;
use tracing_subscriber::{Layer, layer::Context, prelude::*};

#[derive(Clone, Default)]
pub(crate) struct Statements(Arc<Mutex<Vec<String>>>);
impl<S: Subscriber> Layer<S> for Statements {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() != "sqlx::query" {
            return;
        }
        #[derive(Default)]
        struct Visitor {
            sql: String,
            summary: String,
        }
        impl tracing::field::Visit for Visitor {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                match field.name() {
                    "db.statement" => self.sql = value.to_owned(),
                    "summary" => self.summary = value.to_owned(),
                    _ => {}
                }
            }
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if matches!(field.name(), "db.statement" | "summary") {
                    let value = format!("{value:?}");
                    self.record_str(
                        field,
                        &serde_json::from_str::<String>(&value).unwrap_or(value),
                    );
                }
            }
        }
        let mut visitor = Visitor::default();
        event.record(&mut visitor);
        let sql = if visitor.sql.trim().is_empty() {
            visitor.summary
        } else {
            visitor.sql
        };
        if !sql.is_empty() {
            self.0.lock().unwrap().push(sql);
        }
    }
}
impl Statements {
    pub(crate) fn snapshot(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

pub(crate) async fn recorded<F: std::future::Future>(
    statements: &Statements,
    work: F,
) -> F::Output {
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(statements.clone()));
    // These fixtures use Tokio's current-thread test runtime. Keep the dispatch
    // installed through future destruction as well as polling SQLx's streams.
    let _dispatch = tracing::dispatcher::set_default(&dispatch);
    assert!(tracing::enabled!(target: "sqlx::query", tracing::Level::DEBUG));
    work.await
}

tokio::task_local! {
    pub(crate) static PREFLIGHT_PAUSE: (&'static str, Arc<tokio::sync::Barrier>);
}

pub(crate) async fn after_preflight(operation: &str) {
    let barrier = PREFLIGHT_PAUSE
        .try_with(|(expected, barrier)| (*expected == operation).then(|| barrier.clone()))
        .ok()
        .flatten();
    if let Some(barrier) = barrier {
        barrier.wait().await;
        barrier.wait().await;
    }
}

#[tokio::test]
async fn empty_polls_do_not_open_transactions_or_cache_a_miss() {
    let pool = db::test_pool_with_max_connections(3).await;
    ensure(&pool, "future", "poll-test", serde_json::json!({}))
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET state='complete',available_at=clock_timestamp()+interval '1 hour'")
        .execute(&pool).await.unwrap();
    let statements = Statements::default();
    recorded(&statements, async {
        ensure(&pool, "future", "poll-test", serde_json::json!({}))
            .await
            .unwrap();
        assert!(claim(&pool, "poll-test", "one").await.unwrap().is_none());
    })
    .await;
    let sql = statements.snapshot();
    assert!(!sql.is_empty(), "query tracing must be active");
    assert!(
        !sql.iter().any(|s| s.trim_start().starts_with("BEGIN")),
        "{sql:?}"
    );
    assert!(
        !sql.iter().any(|s| s.trim_start().starts_with("COMMIT")),
        "{sql:?}"
    );
    assert_eq!(
        sql.iter()
            .filter(|s| s.trim_start().starts_with("SELECT") || s.trim_start().starts_with("WITH"))
            .count(),
        2,
        "{sql:?}"
    );
    // A different instance can advance the persisted deadline after an empty
    // hint; the very next poll must see it without a stale local sleep/cache.
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    ensure(&pool, "future", "poll-test", serde_json::json!({}))
        .await
        .unwrap();
    assert!(claim(&pool, "poll-test", "two").await.unwrap().is_some());
}

#[tokio::test]
async fn stale_ensure_hint_cannot_reset_a_newly_claimed_owner() {
    let pool = db::test_pool_with_max_connections(4).await;
    ensure(
        &pool,
        "race",
        "poll-test",
        serde_json::json!({"original":true}),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE background_work SET state='complete',available_at=clock_timestamp()-interval '1 second'")
        .execute(&pool).await.unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let task = tokio::spawn({
        let pool = pool.clone();
        let barrier = barrier.clone();
        async move {
            PREFLIGHT_PAUSE
                .scope(
                    ("ensure", barrier),
                    ensure(
                        &pool,
                        "race",
                        "poll-test",
                        serde_json::json!({"replacement":true}),
                    ),
                )
                .await
        }
    });
    barrier.wait().await;
    ensure(&pool, "race", "poll-test", serde_json::json!({}))
        .await
        .unwrap();
    let winner = claim(&pool, "poll-test", "winner").await.unwrap().unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(w) FROM background_work w WHERE id=$1")
            .bind(&winner.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    barrier.wait().await;
    task.await.unwrap().unwrap();
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(w) FROM background_work w WHERE id=$1")
            .bind(&winner.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(before, after);
    assert_eq!(winner.payload, serde_json::json!({"original":true}));
}

#[tokio::test]
async fn stale_claim_hint_reselects_after_another_instance_wins() {
    let pool = db::test_pool_with_max_connections(4).await;
    for key in ["a", "b"] {
        ensure(&pool, key, "poll-test", serde_json::json!({}))
            .await
            .unwrap();
    }
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let task = tokio::spawn({
        let pool = pool.clone();
        let barrier = barrier.clone();
        async move {
            PREFLIGHT_PAUSE
                .scope(("claim", barrier), claim(&pool, "poll-test", "second"))
                .await
        }
    });
    barrier.wait().await;
    let first = claim(&pool, "poll-test", "first").await.unwrap().unwrap();
    barrier.wait().await;
    let second = task.await.unwrap().unwrap().unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM background_work WHERE state='running'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn stale_claim_hint_rechecks_future_deadline_and_resource_barrier() {
    let pool = db::test_pool_with_max_connections(4).await;
    ensure(
        &pool,
        "scoped",
        "poll-test",
        serde_json::json!({"lease_resources":["node:busy"]}),
    )
    .await
    .unwrap();
    for make_future in [true, false] {
        sqlx::query(
            "UPDATE background_work SET available_at=clock_timestamp()-interval '1 second'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let task = tokio::spawn({
            let pool = pool.clone();
            let barrier = barrier.clone();
            async move {
                PREFLIGHT_PAUSE
                    .scope(
                        ("claim", barrier),
                        claim_id(&pool, "poll-test", "loser", "maintenance:scoped"),
                    )
                    .await
            }
        });
        barrier.wait().await;
        if make_future {
            sqlx::query(
                "UPDATE background_work SET available_at=clock_timestamp()+interval '1 hour'",
            )
            .execute(&pool)
            .await
            .unwrap();
        } else {
            // An unresolved remote intent is a barrier even when the lease is
            // expired, and must not be bypassed by an earlier eligible hint.
            db::write::<_,anyhow::Error,_>(&pool,|tx|Box::pin(async move {
                let lease=execution::acquire(tx,"node:busy","external","external-work").await?.unwrap();
                lease.dispatch_prepared(tx,"pending",&serde_json::json!({}),"poll-pending").await?;
                sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key='node:busy'")
                    .execute(&mut **tx).await?;
                Ok(())
            })).await.unwrap();
        }
        barrier.wait().await;
        assert!(task.await.unwrap().unwrap().is_none());
    }
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM background_work WHERE state='queued' AND owner IS NULL AND generation=0").fetch_one(&pool).await.unwrap(),1);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM resource_leases WHERE resource_key='work:maintenance:scoped' AND owner IS NOT NULL").fetch_one(&pool).await.unwrap(),0);
}

#[tokio::test]
async fn many_resource_guards_use_two_statements_and_reject_stale_members() {
    let pool = db::test_pool_with_max_connections(3).await;
    for count in [2, 8] {
        let resources: Vec<_> = (0..count)
            .map(|n| format!("node:guard-{count}-{n}"))
            .collect();
        let kind = format!("guard-{count}");
        ensure(
            &pool,
            &kind,
            &kind,
            serde_json::json!({"lease_resources":resources}),
        )
        .await
        .unwrap();
        let work = claim(&pool, &kind, "owner").await.unwrap().unwrap();
        let mut tx = pool.begin().await.unwrap();
        let statements = Statements::default();
        recorded(&statements, work.guard_on(&mut tx)).await.unwrap();
        let sql = statements.snapshot();
        // A pool task on this test's runtime may finish a previously queued rollback.
        assert_eq!(
            sql.iter()
                .filter(|statement| {
                    statement.trim()
                        != "SET client_min_messages='error'; ROLLBACK; RESET client_min_messages"
                })
                .count(),
            2,
            "{sql:?}"
        );
        tx.rollback().await.unwrap();
        let mut stale = work.clone();
        stale.leases[0].generation += 1;
        let mut tx = pool.begin().await.unwrap();
        assert!(stale.guard_on(&mut tx).await.is_err());
        tx.rollback().await.unwrap();
        assert!(work.leases[0].renew(&pool).await.unwrap());
    }
}

#[tokio::test]
async fn probed_connection_retries_with_a_fresh_transaction_and_atomic_rollback() {
    let pool = db::test_pool_with_max_connections(1).await;
    sqlx::query("CREATE TABLE poll_retry(value integer)")
        .execute(&pool)
        .await
        .unwrap();
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("SELECT 1")
        .execute(&mut *connection)
        .await
        .unwrap();
    let mut attempts = 0;
    let xids = Arc::new(Mutex::new(Vec::new()));
    db::write_after_probe::<_, anyhow::Error, _>(&pool, connection, |tx| {
        attempts += 1;
        let first = attempts == 1;
        let xids = xids.clone();
        Box::pin(async move {
            let xid = sqlx::query_scalar::<_, i64>("SELECT txid_current()::bigint")
                .fetch_one(&mut **tx)
                .await?;
            xids.lock().unwrap().push(xid);
            sqlx::query("INSERT INTO poll_retry VALUES(1)")
                .execute(&mut **tx)
                .await?;
            if first {
                sqlx::query("DO $$ BEGIN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END $$")
                    .execute(&mut **tx)
                    .await?;
            }
            Ok(())
        })
    })
    .await
    .unwrap();
    assert_eq!(attempts, 2);
    {
        let xids = xids.lock().unwrap();
        assert_ne!(xids[0], xids[1]);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM poll_retry")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
