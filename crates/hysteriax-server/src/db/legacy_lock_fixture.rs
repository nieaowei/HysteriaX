//! Test-only adversarial lock holder. Runtime code cannot call this module.
//! Existing regressions hold the retired key to prove business transactions
//! never depend on it. This is not the production transaction implementation.
//! Ordinary fixture setup uses `pool.begin()` and writer probes use `db::write`.
use sqlx::{PgPool, Postgres, Transaction};
use std::time::Duration;

pub async fn hold_retired_global_write_lock(
    pool: &PgPool,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    let deadline = tokio::time::Instant::now() + pool.options().get_acquire_timeout();
    loop {
        let mut tx = pool.begin().await?;
        let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(62177345902811)")
            .fetch_one(&mut *tx)
            .await?;
        if acquired {
            crate::execution::guard_current(&mut tx).await?;
            return Ok(tx);
        }
        tx.rollback().await?;
        if tokio::time::Instant::now() >= deadline {
            return Err(sqlx::Error::Protocol(
                "retired global write lock fixture acquisition exceeded its deadline".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(rand::random_range(25..75))).await;
    }
}
