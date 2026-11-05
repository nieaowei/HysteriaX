use super::*;
use crate::{db, error::ApiError};
use std::io::Write;
use std::sync::Mutex;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);
struct Writer(Buffer);
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
    type Writer = Writer;
    fn make_writer(&'a self) -> Writer {
        Writer(self.clone())
    }
}
impl Buffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}
fn subscriber(buffer: Buffer) -> impl Subscriber + Send + Sync {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .without_time()
                .with_writer(buffer)
                .with_filter(tracing_subscriber::filter::LevelFilter::INFO),
        )
        .with(
            QueryLayer.with_filter(tracing_subscriber::filter::filter_fn(|m| {
                m.target() == "sqlx::query"
            })),
        )
}
const CONFLICT: &str =
    "DO $body$ BEGIN RAISE EXCEPTION 'diagnostic-secret' USING ERRCODE='40001'; END $body$";

#[test]
fn sql_literals_comments_and_identifiers_are_not_logged() {
    let input = "SELECT 'secret-one', E'secret-two\\\'still-secret', \"secret-column\", $tag$secret-three$tag$, $1, 1234 /* outer /* secret-four */ nested */ -- secret-five\n FROM users WHERE id='密钥'";
    let output = redact(input);
    assert!(output.contains("$1"));
    assert!(output.contains("FROM users"));
    for secret in ["secret", "1234", "密钥"] {
        assert!(!output.contains(secret), "{output}");
    }
    assert_eq!(redact("DO $$ secret-six $$"), "DO ?");
    assert!(!redact(r"SELECT 'first\', 'second-secret'").contains("secret"));
    assert_eq!(
        server_functions(
            "SQL statement \"secret\"\nPL/pgSQL function safe_name() line 9 at SQL statement"
        ),
        "safe_name"
    );
    assert_eq!(redact("SELECT 'unterminated-secret"), "SELECT ?");
    assert!(redact(&"SELECT long_identifier ".repeat(1000)).len() < 2052);
}

#[tokio::test]
async fn retry_success_retains_original_sql_and_metadata_without_error_or_secret() {
    let pool = db::test_pool().await;
    let buffer = Buffer::default();
    let mut count = 0;
    async {
        let value = db::serializable::<_, ApiError, _>(&pool, |tx| {
            count += 1;
            let first = count == 1;
            Box::pin(async move {
                if first {
                    sqlx::query(CONFLICT)
                        .execute(&mut **tx)
                        .await
                        .map_err(|e| {
                            ApiError::from(anyhow::Error::new(e).context("guarded update"))
                        })?;
                }
                Ok(42)
            })
        })
        .await
        .unwrap();
        assert_eq!(value, 42);
    }
    .with_subscriber(subscriber(buffer.clone()))
    .await;
    assert_eq!(count, 2);
    let text = buffer.text();
    assert!(text.contains("outcome=\"retry\""), "{text}");
    assert!(text.contains("outcome=\"succeeded_after_retry\""), "{text}");
    assert!(text.contains("sqlstate=40001"), "{text}");
    assert!(text.contains("statement=\"DO ?\""), "{text}");
    assert!(text.contains("phase=\"operation\""), "{text}");
    assert!(!text.contains("ERROR"), "{text}");
    assert!(!text.contains("diagnostic-secret"), "{text}");
    assert!(text.contains("retry_success_retains_original_sql"));
    let ids: Vec<_> = text
        .lines()
        .filter(|l| l.contains("database transaction outcome"))
        .map(|l| {
            l.split("transaction_id=")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], ids[1]);
}

#[tokio::test]
async fn native_exhaustion_records_five_retries_and_one_final_error() {
    let pool = db::test_pool().await;
    let buffer = Buffer::default();
    let mut count = 0;
    async {
        let result = db::serializable::<(), sqlx::Error, _>(&pool, |tx| {
            count += 1;
            Box::pin(async move {
                sqlx::query(CONFLICT).execute(&mut **tx).await?;
                Ok(())
            })
        })
        .await;
        assert!(result.is_err());
    }
    .with_subscriber(subscriber(buffer.clone()))
    .await;
    let text = buffer.text();
    assert_eq!(count, 6);
    assert_eq!(text.matches("outcome=\"retry\"").count(), 5, "{text}");
    assert_eq!(
        text.matches("outcome=\"retry_exhausted\"").count(),
        1,
        "{text}"
    );
    assert_eq!(text.matches("ERROR").count(), 1, "{text}");
    assert!(!text.contains("diagnostic-secret"));
}

#[tokio::test]
async fn deferred_commit_failure_is_attributed_to_commit_and_not_rollback() {
    let pool = db::test_pool().await;
    sqlx::raw_sql("CREATE SEQUENCE diagnostic_commit_attempt; CREATE TABLE diagnostic_commit(value integer); CREATE FUNCTION diagnostic_deferred_conflict() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('diagnostic_commit_attempt')=1 THEN RAISE EXCEPTION 'diagnostic-secret' USING ERRCODE='40001'; END IF; RETURN NULL; END $$; CREATE CONSTRAINT TRIGGER diagnostic_commit_trigger AFTER INSERT ON diagnostic_commit DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION diagnostic_deferred_conflict();").execute(&pool).await.unwrap();
    let buffer = Buffer::default();
    async {
        db::serializable::<(), ApiError, _>(&pool, |tx| {
            Box::pin(async move {
                sqlx::query("INSERT INTO diagnostic_commit(value) VALUES($1)")
                    .bind(42_i32)
                    .execute(&mut **tx)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
    }
    .with_subscriber(subscriber(buffer.clone()))
    .await;
    let text = buffer.text();
    assert!(text.contains("phase=\"commit\""), "{text}");
    assert!(text.contains("statement=\"COMMIT\""), "{text}");
    assert!(text.contains("diagnostic_deferred_conflict"), "{text}");
    assert!(!text.contains("diagnostic-secret"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM diagnostic_commit")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn concurrent_attempts_do_not_share_statement_or_error_context() {
    let pool = db::test_pool_with_max_connections(2).await;
    let mut first_tx = pool.begin().await.unwrap();
    let mut second_tx = pool.begin().await.unwrap();
    let buffer = Buffer::default();
    async {
        let one = attempt(async {
            phase("operation");
            let e = sqlx::query(CONFLICT)
                .execute(&mut *first_tx)
                .await
                .unwrap_err();
            record(&e);
        });
        let two = attempt(async {
            phase("guard");
            let e = sqlx::query(
                "DO $$ BEGIN RAISE EXCEPTION 'second-secret' USING ERRCODE='40P01'; END $$",
            )
            .execute(&mut *second_tx)
            .await
            .unwrap_err();
            record(&e);
        });
        let ((_, a), (_, b)) = tokio::join!(one, two);
        assert_eq!(a.failure.sqlstate, "40001");
        assert_eq!(b.failure.sqlstate, "40P01");
        assert_eq!(a.failure.phase, "operation");
        assert_eq!(b.failure.phase, "guard");
        assert_ne!(
            a.failure.statement.fingerprint,
            b.failure.statement.fingerprint
        );
        assert_eq!(a.failure.statement.sql, "DO ?");
        assert_eq!(b.failure.statement.sql, "DO ?");
    }
    .with_subscriber(subscriber(buffer))
    .await;
}

#[tokio::test]
async fn business_conflicts_are_not_retried_and_dropped_calls_are_counted() {
    let pool = db::test_pool().await;
    let mut count = 0;
    let e = db::write::<(), ApiError, _>(&pool, |_| {
        count += 1;
        Box::pin(async { Err(ApiError::conflict("revision changed")) })
    })
    .await
    .unwrap_err();
    assert_eq!(e.code, "revision_conflict");
    assert_eq!(count, 1);
    let cancelled = tokio::time::timeout(
        Duration::from_millis(25),
        db::write::<(), sqlx::Error, _>(&pool, |_| Box::pin(std::future::pending())),
    )
    .await;
    assert!(cancelled.is_err());
    let registry = OPERATIONS.get().unwrap().read().unwrap();
    let cancelled: u64 = registry
        .iter()
        .filter(|((name, _), _)| {
            name.contains("business_conflicts_are_not_retried_and_dropped_calls_are_counted")
        })
        .map(|(_, c)| c.cancelled.load(Ordering::Relaxed))
        .sum();
    assert_eq!(cancelled, 1);
}

#[tokio::test]
async fn successful_internal_rollback_preserves_the_validation_failure_statement() {
    let pool = db::test_pool().await;
    let buffer = Buffer::default();
    async {
        let mut tx = pool.begin().await.unwrap();
        let (_, report) = attempt(async {
            phase("finalization_validate");
            let error = sqlx::query(CONFLICT).execute(&mut *tx).await.unwrap_err();
            // Match a finalization validator that cleans up before returning its
            // original sqlx error to the outer transaction diagnostic boundary.
            cleanup(tx.rollback()).await.unwrap();
            record(&error);
        })
        .await;
        assert_eq!(report.failure.phase, "finalization_validate");
        assert_eq!(report.failure.statement.sql, "DO ?");
        assert_eq!(report.failure.sqlstate, "40001");
    }
    .with_subscriber(subscriber(buffer))
    .await;
}

#[tokio::test]
async fn a_business_rejection_after_retry_still_has_a_visible_terminal_outcome() {
    let pool = db::test_pool().await;
    let buffer = Buffer::default();
    let mut count = 0;
    async {
        let error = db::serializable::<(), ApiError, _>(&pool, |tx| {
            count += 1;
            let first = count == 1;
            Box::pin(async move {
                if first {
                    sqlx::query(CONFLICT).execute(&mut **tx).await?;
                }
                Err(ApiError::conflict("revision changed"))
            })
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, "revision_conflict");
    }
    .with_subscriber(subscriber(buffer.clone()))
    .await;
    let text = buffer.text();
    assert_eq!(count, 2);
    assert!(text.contains("outcome=\"retry\""), "{text}");
    assert!(text.contains("outcome=\"failed\""), "{text}");
    assert!(!text.contains("ERROR"), "{text}");
    assert!(!text.contains("succeeded_after_retry"), "{text}");
}
