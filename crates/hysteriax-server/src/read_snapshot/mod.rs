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
mod tests;
