use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Count real PostgreSQL CommandComplete messages, including pool housekeeping.
// This measures client/database commands without an extension or SQL mocks.
struct BillingRelay {
    pool: PgPool,
    commands: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl BillingRelay {
    async fn new(direct: &PgPool, delay: Duration) -> Self {
        let schema: String = sqlx::query_scalar("SELECT current_schema()")
            .fetch_one(direct)
            .await
            .unwrap();
        let options = (*direct.connect_options()).clone();
        let target = (options.get_host().to_owned(), options.get_port());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let commands = Arc::new(AtomicUsize::new(0));
        let counter = commands.clone();
        let task = tokio::spawn(async move {
            loop {
                let (downstream, _) = listener.accept().await.unwrap();
                let upstream = tokio::net::TcpStream::connect(target.clone())
                    .await
                    .unwrap();
                let counter = counter.clone();
                tokio::spawn(async move {
                    let (mut dr, mut dw) = downstream.into_split();
                    let (mut ur, mut uw) = upstream.into_split();
                    let send = tokio::io::copy(&mut dr, &mut uw);
                    let receive = async {
                        let mut bytes = [0_u8; 65536];
                        let mut pending = Vec::new();
                        loop {
                            let count = ur.read(&mut bytes).await?;
                            if count == 0 {
                                return Ok::<(), std::io::Error>(());
                            }
                            pending.extend_from_slice(&bytes[..count]);
                            let mut consumed = 0;
                            while pending.len() - consumed >= 5 {
                                let length = u32::from_be_bytes(
                                    pending[consumed + 1..consumed + 5].try_into().unwrap(),
                                ) as usize;
                                if pending.len() - consumed < length + 1 {
                                    break;
                                }
                                if pending[consumed] == b'C' {
                                    counter.fetch_add(1, Ordering::SeqCst);
                                }
                                consumed += length + 1;
                            }
                            pending.drain(..consumed);
                            if !delay.is_zero() {
                                tokio::time::sleep(delay).await;
                            }
                            dw.write_all(&bytes[..count]).await?;
                        }
                    };
                    tokio::select! { _ = send => {}, _ = receive => {} }
                });
            }
        });
        let pool = crate::db::pool_options()
            .max_connections(1)
            .after_connect(move |connection, _| {
                let schema = schema.clone();
                Box::pin(async move {
                    sqlx::query(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}")))
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(
                options
                    .host("127.0.0.1")
                    .port(port)
                    .ssl_mode(sqlx::postgres::PgSslMode::Disable),
            )
            .await
            .unwrap();
        // Finish connection setup and after_release before starting a sample.
        pool.acquire().await.unwrap().return_to_pool().await;
        Self {
            pool,
            commands,
            task,
        }
    }

    async fn measure(&self, id: &str) -> (Progress, Duration, usize) {
        self.commands.store(0, Ordering::SeqCst);
        let started = Instant::now();
        let result = batches::process(&self.pool, id).await.unwrap();
        // Wait for asynchronous after_release, so it cannot spill into the next sample.
        self.pool.acquire().await.unwrap().return_to_pool().await;
        (
            result,
            started.elapsed(),
            self.commands.load(Ordering::SeqCst),
        )
    }
}
impl Drop for BillingRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn billing_commands_are_bounded_for_small_large_and_restricted_pages() {
    let pool = fixture(500).await;
    let relay = BillingRelay::new(&pool, Duration::ZERO).await;
    let mut counts = Vec::new();
    for (id, users, tx) in [
        ("small", 100, 10),
        ("large", 500, 20),
        ("restricted", 500, 10),
    ] {
        if id == "restricted" {
            sqlx::query("UPDATE users SET quota_bytes=25")
                .execute(&pool)
                .await
                .unwrap();
        }
        let mut intervals = counters(users, tx, 0);
        if id == "large" {
            for n in 1..=100 {
                intervals.insert(format!("user-{n:04}"), json!({"tx":10,"rx":0}));
            }
        }
        capture(&pool, id, "a", &intervals).await;
        let (progress, _, commands) = relay.measure(id).await;
        assert_eq!(
            progress,
            Progress::Committed {
                items: users,
                complete: true
            }
        );
        // Includes BEGIN/COMMIT, connection health checks and pool reset. A
        // per-user query regression exceeds this budget even on the small page.
        assert!(commands <= 35, "{id}: {commands} database commands");
        counts.push(commands);
    }
    assert_eq!(
        counts[0], counts[1],
        "billing must not add commands per user"
    );
    assert_eq!(usage(&pool).await, vec![30; 500]);
    let work: Vec<Value> =
        sqlx::query_scalar("SELECT payload FROM background_work WHERE kind='access-revocation'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(work.len(), 500);
    for payload in work {
        // The bulk SQL payload must remain compatible with the real worker.
        let _: chrono::DateTime<chrono::Utc> =
            serde_json::from_value(payload["restriction_marker"].clone()).unwrap();
        assert_eq!(payload["restriction_generation"], 1);
    }
    relay.pool.close().await;
}

#[tokio::test]
async fn mixed_page_preserves_interval_bytes_membership_and_restart_gap() {
    let pool = fixture(8).await;
    capture(&pool, "initial", "a", &counters(8, 100, 50)).await;
    batches::drain(&pool, "initial").await.unwrap();
    sqlx::raw_sql("UPDATE users SET quota_bytes=200 WHERE id='user-0001'; UPDATE users SET enabled=false WHERE id='user-0002'; UPDATE users SET expires_at=clock_timestamp()-interval '1 second' WHERE id='user-0003'; UPDATE users SET quota_bytes=100 WHERE id='user-0004'; UPDATE users SET usage_bytes=9223372036854775800 WHERE id='user-0005'; UPDATE traffic_sample_batches SET instance_id='previous-instance' WHERE sample_id='initial'; DELETE FROM node_assignments WHERE node_id='a' AND user_id='user-0007'")
        .execute(&pool).await.unwrap();
    crate::db::write(&pool, |tx| {
        Box::pin(async move { super::super::access::admit_on(tx, "user-0004").await })
    })
    .await
    .unwrap();
    let mut values = counters(8, 200, 100);
    values.insert("user-0002".into(), json!({"tx":150,"rx":20}));
    values.insert("user-0006".into(), json!({"tx":90,"rx":40}));
    capture(&pool, "mixed", "a", &values).await;
    // A newer/equal baseline is never overwritten, but its item is checkpointed.
    sqlx::query("UPDATE traffic_baselines SET sample_order=(SELECT sample_order FROM traffic_sample_batches WHERE sample_id='mixed') WHERE node_id='a' AND user_id='user-0008'")
        .execute(&pool).await.unwrap();
    batches::drain(&pool, "mixed").await.unwrap();
    assert_eq!(
        usage(&pool).await,
        vec![450, 320, 450, 450, i64::MAX, 280, 150, 150]
    );
    let rows:Vec<(String,i64,i64,Option<String>)>=sqlx::query_as("SELECT user_id,delta_tx,delta_rx,gap_reason FROM traffic_records WHERE sample_id='mixed' ORDER BY user_id")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(rows.len(), 6);
    assert_eq!(
        rows[1],
        (
            "user-0002".into(),
            150,
            20,
            Some("node_restarted_between_samples".into())
        )
    );
    assert_eq!(
        rows[5],
        (
            "user-0006".into(),
            90,
            40,
            Some("node_restarted_between_samples".into())
        )
    );
    let gap: String = sqlx::query_scalar("SELECT reason FROM data_gaps WHERE node_id='a'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(gap, "node_restarted_between_samples");
    let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM background_work WHERE kind='access-revocation'),(SELECT processed_count FROM traffic_sample_batches WHERE sample_id='mixed')")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (4, 7));
    // Clearing and readmitting an existing restriction must use the new epoch,
    // not the generation visible before the marker's AFTER trigger ran.
    sqlx::query("UPDATE users SET access_kick_enqueued_at=NULL WHERE id='user-0004'")
        .execute(&pool)
        .await
        .unwrap();
    capture(&pool, "readmit", "a", &values).await;
    batches::drain(&pool, "readmit").await.unwrap();
    let generations:Vec<i64>=sqlx::query_scalar("SELECT (payload->>'restriction_generation')::bigint FROM background_work WHERE payload->>'user_id'='user-0004' ORDER BY (payload->>'restriction_generation')::bigint")
        .fetch_all(&pool).await.unwrap();
    assert_eq!(generations, vec![1, 3]);
}

#[tokio::test]
async fn failed_bulk_revocation_rolls_back_billing_markers_epochs_and_checkpoints() {
    let pool = fixture(3).await;
    sqlx::query("UPDATE users SET quota_bytes=5")
        .execute(&pool)
        .await
        .unwrap();
    capture(&pool, "sample", "a", &counters(3, 10, 0)).await;
    sqlx::raw_sql("CREATE FUNCTION reject_bulk_work() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='access-revocation' THEN RAISE EXCEPTION 'injected revocation failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_bulk_work BEFORE INSERT ON background_work FOR EACH ROW EXECUTE FUNCTION reject_bulk_work()")
        .execute(&pool).await.unwrap();
    assert!(batches::process(&pool, "sample").await.is_err());
    assert_eq!(usage(&pool).await, vec![0; 3]);
    let counts:(i64,i64,i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM traffic_baselines),(SELECT count(*) FROM traffic_records),(SELECT count(*) FROM users WHERE access_kick_enqueued_at IS NOT NULL),(SELECT count(*) FROM user_access_restriction_epochs),(SELECT count(*) FROM traffic_sample_items WHERE processed_at IS NOT NULL),(SELECT processed_count FROM traffic_sample_batches WHERE sample_id='sample')")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 0, 0, 0));
    sqlx::query("DROP TRIGGER reject_bulk_work ON background_work")
        .execute(&pool)
        .await
        .unwrap();
    batches::drain(&pool, "sample").await.unwrap();
    assert_eq!(usage(&pool).await, vec![10; 3]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM background_work WHERE kind='access-revocation'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        3
    );
}

#[tokio::test]
async fn shared_users_across_large_node_batches_are_billed_once_with_real_triggers() {
    let pool = fixture(1001).await;
    sqlx::query("UPDATE users SET quota_bytes=15")
        .execute(&pool)
        .await
        .unwrap();
    capture(&pool, "a-large", "a", &counters(1001, 7, 3)).await;
    capture(&pool, "b-large", "b", &counters(1001, 7, 3)).await;
    // Keep all production triggers, FKs, cache invalidations and rollups active.
    let (a, b, duplicate) = tokio::join!(
        batches::drain(&pool, "a-large"),
        batches::drain(&pool, "b-large"),
        batches::drain(&pool, "a-large")
    );
    a.unwrap();
    b.unwrap();
    duplicate.unwrap();
    batches::drain(&pool, "a-large").await.unwrap();
    batches::drain(&pool, "b-large").await.unwrap();
    assert_eq!(usage(&pool).await, vec![20; 1001]);
    let counts:(i64,i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM traffic_records),(SELECT count(*) FROM traffic_baselines),(SELECT count(*) FROM background_work WHERE kind='access-revocation'),(SELECT count(*) FROM traffic_sample_items WHERE processed_at IS NOT NULL)")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(counts, (2002, 2002, 1001, 2002));
    let totals: (String, String) =
        sqlx::query_as("SELECT sum(tx)::text,sum(rx)::text FROM user_node_usage")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(totals, ("14014".into(), "6006".into()));
}

#[tokio::test]
#[ignore = "manual billing benchmark; keeps all production triggers enabled"]
async fn benchmark_billing_pages() {
    let delay = Duration::from_millis(
        std::env::var("HYSTERIAX_BILLING_BENCH_DELAY_MS")
            .unwrap_or_else(|_| "0".into())
            .parse()
            .unwrap(),
    );
    let pool = fixture(500).await;
    let relay = BillingRelay::new(&pool, delay).await;
    for users in [100, 200, 500] {
        for attempt in 0..3 {
            let id = format!("bench-{users}-{attempt}");
            capture(
                &pool,
                &id,
                "a",
                &counters(users, (users + attempt + 1) as i64, 10),
            )
            .await;
            let (progress, elapsed, commands) = relay.measure(&id).await;
            assert_eq!(
                progress,
                Progress::Committed {
                    items: users,
                    complete: true
                }
            );
            eprintln!(
                "billing_benchmark users={users} attempt={attempt} response_delay_ms={} elapsed_ms={} commands={commands}",
                delay.as_millis(),
                elapsed.as_millis()
            );
        }
    }
    relay.pool.close().await;
}
