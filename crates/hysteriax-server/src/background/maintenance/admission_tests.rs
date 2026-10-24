use super::*;

#[tokio::test]
async fn repeated_admission_does_not_wait_for_a_healthy_work_transaction() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    ensure(
        &pool,
        "busy",
        "held-kind",
        serde_json::json!({"source":"original"}),
    )
    .await
    .unwrap();
    let work = claim(&pool, "held-kind", "healthy-owner")
        .await
        .unwrap()
        .unwrap();
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(w) FROM background_work w WHERE id=$1")
            .bind(&work.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let mut held = pool.begin().await.unwrap();
    work.guard_on(&mut held).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_millis(750),
        ensure(
            &pool,
            "busy",
            "held-kind",
            serde_json::json!({"source":"replacement"}),
        ),
    )
    .await
    .expect("admission must not wait behind a healthy worker")
    .unwrap();
    ensure(&pool, "other", "other-kind", serde_json::json!({}))
        .await
        .unwrap();
    let other = claim(&pool, "other-kind", "other-owner")
        .await
        .unwrap()
        .expect("unrelated work must remain claimable");
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(w) FROM background_work w WHERE id=$1")
            .bind(&work.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        before, after,
        "repeat admission must preserve the exact healthy ownership and source"
    );
    assert_ne!(other.id, work.id);
    held.rollback().await.unwrap();
}

#[tokio::test]
async fn due_completed_admission_skips_a_locked_row_and_requeues_it_after_release() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    ensure(
        &pool,
        "due",
        "kind",
        serde_json::json!({"source":"original"}),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE background_work SET state='complete',finished_at=clock_timestamp(),available_at=clock_timestamp()-interval '1 second',error_code='previous' WHERE id='maintenance:due'").execute(&pool).await.unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM background_work WHERE id='maintenance:due' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_millis(750),
        ensure(
            &pool,
            "due",
            "kind",
            serde_json::json!({"source":"replacement"}),
        ),
    )
    .await
    .expect("due admission must skip a busy row")
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM background_work WHERE id='maintenance:due'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        "complete"
    );
    held.rollback().await.unwrap();
    ensure(
        &pool,
        "due",
        "kind",
        serde_json::json!({"source":"replacement"}),
    )
    .await
    .unwrap();
    let row=sqlx::query("SELECT state,payload,error_code,finished_at FROM background_work WHERE id='maintenance:due'").fetch_one(&pool).await.unwrap();
    assert_eq!(row.get::<String, _>("state"), "queued");
    assert_eq!(
        row.get::<serde_json::Value, _>("payload"),
        serde_json::json!({"source":"original"})
    );
    assert!(row.get::<Option<String>, _>("error_code").is_none());
    assert!(
        row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("finished_at")
            .is_none()
    );
}
