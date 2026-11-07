//! Registered only by prepare-pool-diagnostic-build.py in the frozen diagnostic context.
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tracing::instrument::WithSubscriber;
use tracing::{
    Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    Layer,
    layer::{Context, SubscriberExt},
};

type Record = BTreeMap<String, serde_json::Value>;
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Record>>>);
struct Fields(Record);
impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().into(), format!("{value:?}").into());
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), value.into());
    }
}
impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() == "sqlx::pool::phase" {
            let mut fields = Fields(Record::new());
            event.record(&mut fields);
            self.0.lock().unwrap().push(fields.0);
        }
    }
}
fn url() -> String {
    std::env::var("TEST_DATABASE_URL").expect("isolated TEST_DATABASE_URL required")
}
fn options() -> sqlx::postgres::PgPoolOptions {
    super::pool_options()
        .max_connections(1)
        .acquire_slow_threshold(Duration::from_millis(50))
}
fn find(c: &Capture, operation: &str, phase: &str) -> Record {
    let records = c.0.lock().unwrap().clone();
    records
        .iter()
        .find(|r| r["operation"] == operation && r["dominant_phase"] == phase)
        .unwrap_or_else(|| panic!("missing {operation}/{phase} timing: {records:?}"))
        .clone()
}

#[tokio::test]
async fn pool_phase_permit_wait_is_separate_from_ping() {
    let capture = Capture::default();
    async {
        let pool = options().connect(&url()).await.unwrap();
        let held = pool.acquire().await.unwrap();
        let (_, next) = tokio::join!(
            async {
                tokio::time::sleep(Duration::from_millis(180)).await;
                drop(held);
            },
            pool.acquire()
        );
        let next = next.unwrap();
        drop(next);
        pool.close().await;
    }
    .with_subscriber(tracing_subscriber::registry().with(capture.clone()))
    .await;
    let r = find(&capture, "acquire", "permit_wait");
    assert!(r["permit_wait_secs"].as_f64().unwrap() >= 0.15);
    assert!(r["checkout_ping_secs"].as_f64().unwrap() < 0.10);
    assert_eq!(r["pool_max"], 1);
    assert_eq!(r["pool_idle_start"], 0);
    assert_eq!(r["outcome"], "ok");
    assert!(r["connection_id"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn pool_phase_initialization_and_before_acquire_are_distinct() {
    let capture = Capture::default();
    async {
        let pool = options()
            .after_connect(|_, _| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(180)).await;
                    Ok(())
                })
            })
            .before_acquire(|_, _| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(180)).await;
                    Ok(true)
                })
            })
            .connect_lazy(&url())
            .unwrap();
        let first = pool.acquire().await.unwrap();
        drop(first);
        let second = pool.acquire().await.unwrap();
        drop(second);
        pool.close().await;
    }
    .with_subscriber(tracing_subscriber::registry().with(capture.clone()))
    .await;
    assert!(
        find(&capture, "acquire", "initialize")["initialize_secs"]
            .as_f64()
            .unwrap()
            >= 0.15
    );
    assert!(
        find(&capture, "acquire", "before_acquire")["before_acquire_secs"]
            .as_f64()
            .unwrap()
            >= 0.15
    );
}

#[tokio::test]
async fn pool_phase_timeout_preserves_pool_and_records_wait() {
    let capture = Capture::default();
    async {
        let pool = options()
            .acquire_timeout(Duration::from_millis(150))
            .connect(&url())
            .await
            .unwrap();
        let held = pool.acquire().await.unwrap();
        assert!(matches!(
            pool.acquire().await,
            Err(sqlx::Error::PoolTimedOut)
        ));
        drop(held);
        let again = pool.acquire().await.unwrap();
        drop(again);
        pool.close().await;
    }
    .with_subscriber(tracing_subscriber::registry().with(capture.clone()))
    .await;
    let r = find(&capture, "acquire", "permit_wait");
    assert_eq!(r["outcome"], "error");
    assert!(r["permit_wait_secs"].as_f64().unwrap() >= 0.12);
}

#[tokio::test]
async fn pool_phase_release_delay_is_not_checkout_ping() {
    let capture = Capture::default();
    async {
        let pool = options()
            .after_release(|connection, _| {
                Box::pin(async move {
                    sqlx::raw_sql("ROLLBACK").execute(connection).await?;
                    tokio::time::sleep(Duration::from_millis(1100)).await;
                    Ok(true)
                })
            })
            .connect(&url())
            .await
            .unwrap();
        let mut held = pool.acquire().await.unwrap();
        // Run the same return future in this subscriber scope; Tokio's detached
        // return task does not inherit a test-local default dispatcher.
        let returning = held.return_to_pool();
        let (_, next) = tokio::join!(returning, pool.acquire());
        let next = next.unwrap();
        drop(next);
        pool.close().await;
    }
    .with_subscriber(tracing_subscriber::registry().with(capture.clone()))
    .await;
    let r = find(&capture, "release", "release_reset");
    assert!(r["release_reset_secs"].as_f64().unwrap() >= 1.0);
    let acquired = find(&capture, "acquire", "permit_wait");
    assert_eq!(r["connection_id"], acquired["connection_id"]);
    assert!(acquired["checkout_ping_secs"].as_f64().unwrap() < 0.10);
}
