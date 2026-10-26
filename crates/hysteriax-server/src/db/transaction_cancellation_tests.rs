use super::*;
use sqlx::Connection;
use std::{
    future::{Future, poll_fn},
    task::Poll,
    time::Duration,
};

async fn cancel_pending(future: impl Future) {
    let mut future = std::pin::pin!(future);
    let pending = poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await;
    assert!(
        pending,
        "must cancel while the database response is pending"
    );
}

async fn assert_reusable(pool: &PgPool) {
    let mut connection = tokio::time::timeout(Duration::from_secs(3), pool.acquire())
        .await
        .unwrap()
        .unwrap();
    assert!(!connection.is_in_transaction());
    let settings: (String, String) = sqlx::query_as(
        "SELECT current_setting('transaction_isolation'),current_setting('transaction_read_only')",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(settings, ("read committed".into(), "off".into()));
    // An autocommit SQL error must not poison the next borrower with 25P02.
    let error = sqlx::query("SELECT 1/0")
        .execute(&mut *connection)
        .await
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("22012")
    );
    drop(connection);
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn cancelled_begin_and_custom_begin_restore_the_only_pooled_connection() {
    let pool = test_pool().await;
    for statement in [
        "BEGIN",
        "BEGIN ISOLATION LEVEL SERIALIZABLE",
        "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY",
    ] {
        let mut connection = pool.acquire().await.unwrap();
        cancel_pending(connection.begin_with(statement)).await;
        drop(connection);
        assert_reusable(&pool).await;
        // A fresh isolation change must not produce 25001 after reuse.
        let tx = pool
            .begin_with("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .await
            .unwrap();
        tx.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn cancelled_nested_operation_does_not_publish_partial_writes() {
    let pool = test_pool().await;
    sqlx::query("CREATE TABLE cancellation_savepoint(value int)")
        .execute(&pool)
        .await
        .unwrap();
    let mut outer = pool.begin().await.unwrap();
    sqlx::query("INSERT INTO cancellation_savepoint VALUES(7)")
        .execute(&mut *outer)
        .await
        .unwrap();
    cancel_pending((*outer).begin()).await;
    // The owning operation is cancelled too; never continue an operation after
    // cancelling one of its database awaits or publish a partial result.
    drop(outer);
    assert_reusable(&pool).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cancellation_savepoint")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn cancelled_commit_rollback_and_failed_begin_do_not_poison_reuse() {
    let pool = test_pool().await;
    let tx = pool.begin().await.unwrap();
    cancel_pending(tx.commit()).await;
    assert_reusable(&pool).await;
    let tx = pool.begin().await.unwrap();
    cancel_pending(tx.rollback()).await;
    assert_reusable(&pool).await;
    assert!(
        pool.begin_with("BEGIN ISOLATION LEVEL invalid_isolation")
            .await
            .is_err()
    );
    assert_reusable(&pool).await;
}

#[tokio::test]
async fn failed_or_untracked_server_transaction_is_cleaned_or_discarded() {
    let pool = test_pool().await;
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .execute(&mut *connection)
        .await
        .unwrap();
    assert!(!connection.is_in_transaction());
    drop(connection);
    assert_reusable(&pool).await;
    let mut connection = pool.acquire().await.unwrap();
    let old_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    sqlx::query("BEGIN")
        .execute(&mut *connection)
        .await
        .unwrap();
    assert!(
        sqlx::query("SELECT 1/0")
            .execute(&mut *connection)
            .await
            .is_err()
    );
    drop(connection);
    let new_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(old_pid, new_pid, "an aborted session must be discarded");
    assert_reusable(&pool).await;
}

#[tokio::test]
async fn session_reset_deadline_discards_a_connection_with_unfinished_io() {
    let pool = test_pool().await;
    let mut connection = pool.acquire().await.unwrap();
    let old_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    // A simple-query frame is sent on the first poll, so releasing the borrow
    // leaves seven seconds of server work ahead of the pool reset command.
    cancel_pending(sqlx::raw_sql("SELECT pg_sleep(7)").execute(&mut *connection)).await;
    drop(connection);
    // The fixture acquisition budget is also five seconds. A waiter may
    // expire just as cleanup discards the socket; the next admission must
    // obtain a fresh session rather than the dirty one.
    let mut replacement = tokio::time::timeout(Duration::from_secs(9), async {
        loop {
            match pool.acquire().await {
                Ok(connection) => break connection,
                Err(sqlx::Error::PoolTimedOut) => continue,
                Err(error) => panic!("replacement connection failed: {error}"),
            }
        }
    })
    .await
    .unwrap();
    let new_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *replacement)
        .await
        .unwrap();
    assert_ne!(
        old_pid, new_pid,
        "a reset deadline must discard the session"
    );
    drop(replacement);
    assert_reusable(&pool).await;
}
