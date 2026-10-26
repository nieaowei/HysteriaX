//! A cache key and every database read used to fill it share one transaction.
//! Readers take short connection borrows; nested helpers never start a new
//! snapshot. The scope owns commit/rollback, including request cancellation.
use crate::error::ApiError;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use std::{
    future::Future,
    ops::{Deref, DerefMut},
    sync::Arc,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

type Tx = Transaction<'static, Postgres>;
#[derive(Clone)]
struct Snapshot {
    transaction: Arc<Mutex<Option<Tx>>>,
    cacheable: bool,
}
tokio::task_local! { static CURRENT: Snapshot; }

enum Borrow {
    Owned(Tx),
    Shared(OwnedMutexGuard<Option<Tx>>),
}
pub struct Reader(Borrow);
impl Deref for Reader {
    type Target = PgConnection;
    fn deref(&self) -> &PgConnection {
        match &self.0 {
            Borrow::Owned(tx) => tx,
            Borrow::Shared(tx) => tx.as_ref().expect("snapshot is open"),
        }
    }
}
impl DerefMut for Reader {
    fn deref_mut(&mut self) -> &mut PgConnection {
        match &mut self.0 {
            Borrow::Owned(tx) => tx,
            Borrow::Shared(tx) => tx.as_mut().expect("snapshot is open"),
        }
    }
}
impl Reader {
    pub fn owned(tx: Tx) -> Self {
        Self(Borrow::Owned(tx))
    }
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        match self.0 {
            Borrow::Owned(tx) => tx.commit().await,
            Borrow::Shared(_) => Ok(()), // The enclosing scope commits.
        }
    }
}
pub async fn current() -> Option<Reader> {
    let snapshot = CURRENT.try_with(Clone::clone).ok()?;
    Some(Reader(Borrow::Shared(
        snapshot.transaction.lock_owned().await,
    )))
}
pub fn cacheable() -> bool {
    CURRENT.try_with(|s| s.cacheable).unwrap_or(true)
}
pub async fn transaction(pool: &PgPool) -> Result<Tx, sqlx::Error> {
    pool.begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .await
}
pub async fn begin(pool: &PgPool) -> Result<Reader, sqlx::Error> {
    if let Some(reader) = current().await {
        return Ok(reader);
    }
    Ok(Reader::owned(transaction(pool).await?))
}
pub async fn scope<T>(
    pool: &PgPool,
    future: impl Future<Output = Result<T, ApiError>>,
) -> Result<T, ApiError> {
    if CURRENT.try_with(|_| ()).is_ok() {
        return future.await;
    }
    scope_with(transaction(pool).await?, future).await
}
pub async fn scope_with<T>(
    mut tx: Tx,
    future: impl Future<Output = Result<T, ApiError>>,
) -> Result<T, ApiError> {
    // Existing authoritative index refreshes have a bounded write fallback.
    // Serve that fallback without reading or publishing any cache entries.
    let (isolation, read_only): (String, String) = sqlx::query_as(
        "SELECT current_setting('transaction_isolation'),current_setting('transaction_read_only')",
    )
    .fetch_one(&mut *tx)
    .await?;
    let snapshot = Snapshot {
        transaction: Arc::new(Mutex::new(Some(tx))),
        cacheable: isolation == "repeatable read" && read_only == "on",
    };
    let result = CURRENT.scope(snapshot.clone(), future).await;
    let tx = snapshot
        .transaction
        .lock()
        .await
        .take()
        .expect("snapshot is open");
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            drop(tx);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        extract::{Extension, Path, State},
        middleware,
        routing::get,
    };
    use serde_json::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    #[tokio::test]
    async fn http_cache_key_and_nested_user_reads_use_the_snapshot_before_a_concurrent_commit() {
        coherent_http_fill(false).await;
    }
    #[tokio::test]
    async fn redis_http_fill_keeps_the_database_snapshot_across_a_concurrent_commit() {
        if std::env::var("TEST_REDIS_URL").is_err() {
            return;
        }
        coherent_http_fill(true).await;
    }
    async fn coherent_http_fill(use_redis: bool) {
        let pool = crate::db::test_pool_with_max_connections(3).await;
        sqlx::query(
            "INSERT INTO users(id,name,created_at,updated_at) VALUES('user','Before',now(),now())",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut state = crate::state::AppState::new(
            pool.clone(),
            crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .unwrap(),
        );
        let mut backend = None;
        if use_redis {
            let redis = Arc::new(
                crate::redis_cache::RedisCache::new(
                    &std::env::var("TEST_REDIS_URL").unwrap(),
                    format!("snapshot-test:{}", uuid::Uuid::new_v4()),
                    state.secrets.clone(),
                )
                .unwrap(),
            );
            Arc::get_mut(&mut state.caches).unwrap().responses = crate::cache::Cache::new(
                16,
                1024 * 1024,
                1024 * 1024,
                |v: &crate::cache::CachedResponse| serde_json::to_vec(v).unwrap().len(),
            )
            .with_redis(Some(redis.clone()), "responses");
            backend = Some(redis);
        }
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let (handler_entered, handler_release, handler_calls) =
            (entered.clone(), release.clone(), calls.clone());
        let app = Router::new()
            .route(
                "/api/v2/users/{id}",
                get(
                    move |State(state): State<crate::state::AppState>, Path(id): Path<String>| {
                        let (entered, release, calls) = (
                            handler_entered.clone(),
                            handler_release.clone(),
                            handler_calls.clone(),
                        );
                        async move {
                            // The response-cache middleware has already selected its key.
                            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                                entered.notify_one();
                                release.notified().await;
                            }
                            crate::api::users::get(State(state), Path(id)).await
                        }
                    },
                ),
            )
            .route_layer(middleware::from_fn_with_state(
                state.clone(),
                crate::cache::middleware,
            ))
            .layer(Extension(crate::api::AdminActor { id: "test".into() }))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let url = format!("http://{address}/api/v2/users/user");
        let first_url = url.clone();
        let first = tokio::spawn(async move {
            reqwest::get(first_url)
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        });
        tokio::time::timeout(std::time::Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        let mut write = pool.begin().await.unwrap();
        sqlx::query("UPDATE users SET name='After',usage_bytes=7 WHERE id='user'")
            .execute(&mut *write)
            .await
            .unwrap();
        sqlx::query("INSERT INTO authorization_groups(id,name,created_at,updated_at) VALUES('group','New group',now(),now())").execute(&mut *write).await.unwrap();
        sqlx::query(
            "INSERT INTO authorization_group_users(group_id,user_id,created_at) VALUES('group','user',now())",
        )
        .execute(&mut *write)
        .await
        .unwrap();
        write.commit().await.unwrap();
        release.notify_one();
        let old = first.await.unwrap();
        assert_eq!(old["name"], "Before");
        assert_eq!(old["usage_bytes"], 0);
        assert_eq!(old["authorization_groups"].as_array().unwrap().len(), 0);
        let fresh: Value = reqwest::get(&url)
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(fresh["name"], "After");
        assert_eq!(fresh["usage_bytes"], 7);
        assert_eq!(fresh["authorization_groups"].as_array().unwrap().len(), 1);
        let hit: Value = reqwest::get(&url)
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(hit, fresh);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "old and new versions fill separate cache entries"
        );
        if let Some(redis) = backend {
            let (revision, _) = crate::cache_domains::policy(
                &state,
                "/api/v2/users/user",
                &[],
                std::time::Duration::from_secs(5),
            )
            .await
            .unwrap();
            let key = redis.key(
                "responses",
                &format!("test|{revision}|/api/v2/users/user|[]"),
            );
            let exists: Option<i64> = redis
                .command(redis::cmd("EXISTS").arg(key).to_owned())
                .await;
            assert_eq!(
                exists,
                Some(1),
                "must exercise Redis, without silently falling back to process memory"
            );
        }
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn cancellation_releases_the_only_connection_and_restores_transaction_settings() {
        let pool = crate::db::test_pool().await;
        let entered = Arc::new(Notify::new());
        let child_pool = pool.clone();
        let child_entered = entered.clone();
        let task = tokio::spawn(async move {
            scope(&child_pool, async {
                let mut reader = begin(&child_pool).await?;
                sqlx::query("SELECT 1").execute(&mut *reader).await?;
                reader.commit().await?;
                child_entered.notify_one();
                std::future::pending::<()>().await;
                Ok(())
            })
            .await
        });
        entered.notified().await;
        task.abort();
        let _ = task.await;
        let mut connection =
            tokio::time::timeout(std::time::Duration::from_secs(2), pool.acquire())
                .await
                .unwrap()
                .unwrap();
        let read_only: String = sqlx::query_scalar("SHOW transaction_read_only")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(read_only, "off");
        sqlx::query(
            "INSERT INTO users(id,name,created_at,updated_at) VALUES('after','After',now(),now())",
        )
        .execute(&mut *connection)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn write_fallbacks_do_not_read_or_publish_cache_entries() {
        let pool = crate::db::test_pool().await;
        let cache = crate::cache::Cache::new(4, 128, 128, |v: &String| v.len());
        cache
            .get(
                "key".into(),
                std::time::Duration::from_secs(60),
                false,
                || async { Ok::<_, ApiError>("cached".into()) },
            )
            .await
            .unwrap();
        let tx = pool.begin().await.unwrap();
        let value = scope_with(tx, async {
            assert!(!cacheable());
            let value = cache
                .get(
                    "key".into(),
                    std::time::Duration::from_secs(60),
                    false,
                    || async { Ok::<_, ApiError>("fresh".into()) },
                )
                .await?;
            Ok(value)
        })
        .await
        .unwrap();
        assert_eq!(&**value, "fresh");
        let value = cache
            .get(
                "key".into(),
                std::time::Duration::from_secs(60),
                false,
                || async {
                    panic!("must retain the previously cached value");
                    #[allow(unreachable_code)]
                    Ok::<_, ApiError>("wrong".into())
                },
            )
            .await
            .unwrap();
        assert_eq!(&**value, "cached");
    }
}
