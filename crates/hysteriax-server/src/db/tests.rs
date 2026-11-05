use super::{MIGRATOR, serializable, test_pool, test_pool_with_max_connections};
use std::time::Duration;

#[tokio::test]
async fn serializable_retries_write_skew_from_a_fresh_snapshot() {
    let pool = test_pool_with_max_connections(4).await;
    sqlx::raw_sql("CREATE TABLE retry_counter(id int PRIMARY KEY, value bigint NOT NULL); INSERT INTO retry_counter VALUES(1,0);")
            .execute(&pool).await.unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..2 {
        let pool = pool.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            let mut first = true;
            serializable::<_, sqlx::Error, _>(&pool, move |connection| {
                let wait = first;
                first = false;
                let barrier = barrier.clone();
                Box::pin(async move {
                    let old: i64 = sqlx::query_scalar("SELECT value FROM retry_counter WHERE id=1")
                        .fetch_one(&mut **connection)
                        .await?;
                    if wait {
                        barrier.wait().await;
                    }
                    sqlx::query("UPDATE retry_counter SET value=$1 WHERE id=1")
                        .bind(old + 1)
                        .execute(&mut **connection)
                        .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT value FROM retry_counter WHERE id=1")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn serializable_bounds_retries_and_preserves_business_conflicts() {
    use crate::error::ApiError;
    let pool = test_pool().await;
    let mut attempts = 0;
    let error = serializable::<(), sqlx::Error, _>(&pool, |connection| {
        attempts += 1;
        Box::pin(async move {
            sqlx::query("DO $$ BEGIN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END $$;")
                .execute(&mut **connection)
                .await?;
            Ok(())
        })
    })
    .await
    .unwrap_err();
    assert_eq!(attempts, 6);
    assert_eq!(
        ApiError::from(anyhow::Error::from(error)).status,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    let mut attempts = 0;
    let error = serializable::<(), ApiError, _>(&pool, |_| {
        attempts += 1;
        Box::pin(async { Err(ApiError::conflict("revision changed")) })
    })
    .await
    .unwrap_err();
    assert_eq!(attempts, 1);
    assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
}

#[tokio::test]
async fn fresh_database_has_postgresql_native_columns_and_repeatable_migrations() {
    let pool = test_pool().await;
    super::run_migrations(&pool).await.unwrap();
    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, MIGRATOR.iter().last().unwrap().version);

    let types: Vec<(String, String)> = sqlx::query_as(
            "SELECT column_name, data_type FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'users' AND column_name IN ('enabled', 'expires_at', 'quota_reset_at', 'usage_bytes') ORDER BY column_name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        types,
        vec![
            ("enabled".to_owned(), "boolean".to_owned()),
            (
                "expires_at".to_owned(),
                "timestamp with time zone".to_owned()
            ),
            (
                "quota_reset_at".to_owned(),
                "timestamp with time zone".to_owned()
            ),
            ("usage_bytes".to_owned(), "bigint".to_owned()),
        ]
    );

    let non_native_timestamps: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = current_schema() AND column_name LIKE '%\\_at' ESCAPE '\\' AND data_type != 'timestamp with time zone'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(non_native_timestamps, 0);

    let json_types: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = current_schema() AND column_name IN ('payload_json', 'result_json', 'logs_json', 'detail_json') AND data_type = 'jsonb'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(json_types, 5);

    let buckets: Vec<(String, i64, i16, i16)> = sqlx::query_as(
            "SELECT domain,count(*),min(bucket),max(bucket) FROM cache_bucket_versions GROUP BY domain ORDER BY domain",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(buckets.len(), 17);
    assert!(
        buckets
            .iter()
            .all(|(_, count, min, max)| (*count, *min, *max) == (64, 0, 63))
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT total FROM audit_totals WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT head FROM event_publication_stream WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert!(sqlx::query_scalar::<_, bool>(
            "SELECT to_regclass('authorization_group_previews') IS NULL AND to_regclass('traffic_history_cleanup_runs') IS NULL AND to_regclass('traffic_history_cleanup_nodes') IS NULL AND to_regclass('cache_generation') IS NULL AND to_regclass('cache_domain_versions') IS NULL",
        ).fetch_one(&pool).await.unwrap());
    let epoch: String = sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
        .fetch_one(&pool)
        .await
        .unwrap();
    let other = test_pool().await;
    let other_epoch: String = sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
        .fetch_one(&other)
        .await
        .unwrap();
    assert_ne!(
        epoch, other_epoch,
        "independent installations must not share cache epochs"
    );
}

#[tokio::test]
async fn direct_writes_have_bounded_lock_waits() {
    let pool = test_pool_with_max_connections(2).await;
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT * FROM cache_bucket_versions WHERE domain='nodes' AND bucket=0 FOR UPDATE")
        .execute(&mut *holder)
        .await
        .unwrap();
    let error = tokio::time::timeout(
            Duration::from_secs(3),
            sqlx::query("UPDATE cache_bucket_versions SET generation=generation+1 WHERE domain='nodes' AND bucket=0").execute(&pool),
        )
        .await
        .expect("direct writers must release their connection after a bounded wait")
        .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("55P03")
    );
    holder.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT 1::bigint")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
