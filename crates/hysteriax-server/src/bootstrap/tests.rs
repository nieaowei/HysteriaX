use super::*;
#[tokio::test]
async fn startup_requires_explicit_initialization_and_matching_key_and_protocol() {
    let pool = crate::db::test_pool().await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    let other = SecretBox::from_base64("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE").unwrap();
    assert!(check_runtime(&pool, &secrets).await.is_err());
    crate::credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();

    crate::traffic::access::initialize(&pool).await.unwrap();
    finalize(&pool, &secrets).await.unwrap();
    let event_epoch: String =
        sqlx::query_scalar("SELECT epoch FROM event_publication_stream WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('retained','sync','queued','queued',now(),now(),now())")
            .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('retained','retained','{}',now())")
            .execute(&pool).await.unwrap();
    crate::events::publish_pending(&pool).await.unwrap();
    let epoch: String = sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
        .fetch_one(&pool)
        .await
        .unwrap();
    check_runtime(&pool, &secrets).await.unwrap();
    let unchanged: String = sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        epoch, unchanged,
        "normal startup must not rotate cache epochs"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT epoch FROM event_publication_stream WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap(),
        event_epoch
    );
    assert!(check_runtime(&pool, &other).await.is_err());
    assert!(finalize(&pool, &other).await.is_err());
    let unchanged: String = sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        epoch, unchanged,
        "rejected maintenance must not alter cache identity"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT epoch FROM event_publication_stream WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap(),
        event_epoch
    );
    finalize(&pool, &secrets).await.unwrap();
    let rotated: String = sqlx::query_scalar("SELECT epoch FROM cache_version_epoch WHERE id")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(
        epoch, rotated,
        "explicit maintenance invalidates restored Redis state"
    );
    assert_ne!(
        sqlx::query_scalar::<_, String>("SELECT epoch FROM event_publication_stream WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap(),
        event_epoch
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT head FROM event_publication_stream WHERE id")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM event_publications")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM job_events WHERE event_type='retained'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    check_runtime(&pool, &secrets).await.unwrap();
    sqlx::query("UPDATE runtime_contract SET management_protocol=1 WHERE id")
        .execute(&pool)
        .await
        .unwrap();
    assert!(check_runtime(&pool, &secrets).await.is_err());
}

#[tokio::test]
async fn startup_rejects_missing_future_and_modified_migrations_without_writing() {
    for mutation in [
        "DELETE FROM _sqlx_migrations",
        "UPDATE _sqlx_migrations SET checksum='broken'::bytea WHERE version=1",
        "UPDATE _sqlx_migrations SET success=false WHERE version=1",
        "UPDATE _sqlx_migrations SET version=10000 WHERE version=1",
    ] {
        let pool = crate::db::test_pool().await;
        check_schema(&pool).await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(mutation))
            .execute(&pool)
            .await
            .unwrap();
        let before: serde_json::Value = sqlx::query_scalar(
            "SELECT coalesce(jsonb_agg(to_jsonb(m)), '[]') FROM _sqlx_migrations m",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(check_schema(&pool).await.is_err());
        let after: serde_json::Value = sqlx::query_scalar(
            "SELECT coalesce(jsonb_agg(to_jsonb(m)), '[]') FROM _sqlx_migrations m",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(before, after);
    }
}

#[tokio::test]
async fn legacy_history_is_rejected_without_changing_the_database() {
    let pool = crate::db::test_pool().await;
    sqlx::raw_sql("UPDATE _sqlx_migrations SET checksum='legacy checksum'::bytea; INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(74,'legacy schema',true,'legacy checksum'::bytea,0)")
            .execute(&pool).await.unwrap();
    let before: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(check_schema(&pool).await.is_err());
    assert!(crate::db::run_migrations(&pool).await.is_err());
    let history: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(history, before);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_totals WHERE total=0")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}
