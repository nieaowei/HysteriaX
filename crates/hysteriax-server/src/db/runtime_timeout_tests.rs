use super::*;
use sqlx::migrate::{Migration, MigrationType};

async fn reconnect(fixture: &PgPool, maintenance: bool) -> PgPool {
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(fixture)
        .await
        .unwrap();
    let url = std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap();
    let options = connect_options(&url)
        .unwrap()
        .options([("search_path", schema)]);
    pool_options_for_maintenance(maintenance)
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
}
async fn settings(pool: &PgPool) -> (String, String, String) {
    sqlx::query_as("SELECT current_setting('lock_timeout'),current_setting('statement_timeout'),current_setting('idle_in_transaction_session_timeout')")
        .fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn runtime_timeouts_free_paused_statement_and_transaction_before_a_fresh_lease_expires() {
    let observer = test_pool_with_max_connections(3).await;
    let runtime = reconnect(&observer, false).await;
    assert_eq!(
        settings(&runtime).await,
        ("1s".into(), "20s".into(), "20s".into())
    );
    let mut tx = runtime.begin().await.unwrap();
    let old = crate::execution::acquire(&mut tx, "node:paused", "old", "paused-work")
        .await
        .unwrap()
        .unwrap();
    // Model a pause near expiry without shortening the production lease policy.
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()+interval '5 seconds' WHERE resource_key=$1")
        .bind(&old.resource).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let mut held = runtime.begin().await.unwrap();
    old.guard(&mut held).await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let (query_result, query_done) = tokio::sync::oneshot::channel();
    let (resume, paused) = tokio::sync::oneshot::channel();
    let started = tokio::time::Instant::now();
    let client = tokio::spawn(async move {
        let error = sqlx::query("SELECT pg_sleep(120)")
            .execute(&mut *held)
            .await
            .unwrap_err();
        query_result
            .send(
                error
                    .as_database_error()
                    .and_then(|e| e.code())
                    .map(|c| c.into_owned()),
            )
            .unwrap();
        // Leave the client unresponsive after the server cancels the statement.
        // No Rust cancellation, rollback, or socket close releases its locks.
        paused.await.unwrap();
        let stale_write = sqlx::query(
            "UPDATE resource_leases SET work_id='stale-write' WHERE resource_key='node:paused'",
        )
        .execute(&mut *held)
        .await;
        assert!(stale_write.is_err());
        assert!(held.commit().await.is_err());
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(25), query_done)
            .await
            .unwrap()
            .unwrap()
            .as_deref(),
        Some("57014")
    );
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let alive: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1)")
                    .bind(pid)
                    .fetch_one(&observer)
                    .await
                    .unwrap();
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(crate::execution::LEASE_SECONDS as u64));
    let mut tx = observer.begin().await.unwrap();
    let new = crate::execution::acquire(&mut tx, "node:paused", "new", "recovered-work")
        .await
        .unwrap()
        .unwrap();
    assert!(new.generation > old.generation);
    tx.commit().await.unwrap();
    resume.send(()).unwrap();
    client.await.unwrap();
    let current: (String, String) = sqlx::query_as(
        "SELECT owner,work_id FROM resource_leases WHERE resource_key='node:paused'",
    )
    .fetch_one(&observer)
    .await
    .unwrap();
    assert_eq!(current, ("new".into(), "recovered-work".into()));
    assert!(!old.renew(&observer).await.unwrap());
    assert_eq!(
        settings(&runtime).await,
        ("1s".into(), "20s".into(), "20s".into()),
        "replacement connection inherits runtime deadlines"
    );
    runtime.close().await;
}

#[tokio::test]
async fn runtime_timeouts_are_disabled_for_migration_and_restored_after_success_and_failure() {
    let fixture = test_pool_with_max_connections(2).await;
    let runtime = reconnect(&fixture, false).await;
    let maintenance = reconnect(&fixture, true).await;
    assert_eq!(
        settings(&maintenance).await,
        ("1s".into(), "0".into(), "0".into())
    );
    for fail in [false, true] {
        let body = if fail {
            "RAISE EXCEPTION 'intentional migration failure';"
        } else {
            "NULL;"
        };
        let sql = format!(
            "DO $$ BEGIN IF current_setting('lock_timeout')<>'0' OR current_setting('statement_timeout')<>'0' OR current_setting('idle_in_transaction_session_timeout')<>'0' THEN RAISE EXCEPTION 'runtime timeout leaked into migration'; END IF; {body} END $$;"
        );
        let migration = Migration::new(
            10000 + i64::from(fail),
            "timeout fixture".into(),
            MigrationType::Simple,
            sqlx::SqlSafeStr::into_sql_str(sqlx::AssertSqlSafe(sql)),
            false,
        );
        let migrator = Migrator {
            migrations: vec![migration].into(),
            ignore_missing: true,
            locking: true,
            no_tx: false,
            ..Migrator::DEFAULT
        };
        let result = migrate_with(&runtime, &migrator).await;
        assert_eq!(result.is_err(), fail);
        assert_eq!(
            settings(&runtime).await,
            ("1s".into(), "20s".into(), "20s".into())
        );
    }
    runtime.close().await;
    maintenance.close().await;
}
