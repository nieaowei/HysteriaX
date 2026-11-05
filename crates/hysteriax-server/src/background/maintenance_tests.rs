use super::*;
use serde_json::json;
async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn due(pool: &PgPool) {
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second'")
        .execute(pool)
        .await
        .unwrap();
}
async fn finish(pool: &PgPool, work: &Work, more: bool) -> Result<()> {
    let work = work.clone();
    db::write(pool, move |tx| {
        let work = work.clone();
        Box::pin(async move {
            work.finish_on(tx, more, if more { 1 } else { 86400 }, None)
                .await
        })
    })
    .await
}
#[tokio::test]
async fn shared_maintenance_is_claimed_once_and_healthy_owners_are_not_reset() {
    let pool = db::test_pool_with_max_connections(4).await;
    let (a, b) = tokio::join!(
        ensure(&pool, "a", "test-maintenance", json!({"cursor":1})),
        ensure(&pool, "a", "test-maintenance", json!({"cursor":1}))
    );
    a.unwrap();
    b.unwrap();
    let (a, b) = tokio::join!(
        claim(&pool, "test-maintenance", "one"),
        claim(&pool, "test-maintenance", "two")
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.is_some(), b.is_some());
    let work = a.or(b).unwrap();
    ensure(&pool, "a", "test-maintenance", json!({"cursor":2}))
        .await
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM background_work WHERE state='running' AND payload->>'cursor'='1'"
        )
        .await,
        1
    );
    assert_eq!(
        crate::background::expire(&pool).await.unwrap(),
        0,
        "sampling expiry must not expire nodeless maintenance"
    );
    assert!(
        claim(&pool, "test-maintenance", "three")
            .await
            .unwrap()
            .is_none()
    );
    ensure(&pool, "b", "test-maintenance", json!({}))
        .await
        .unwrap();
    let other = claim(&pool, "test-maintenance", "three")
        .await
        .unwrap()
        .unwrap();
    assert_ne!(work.id, other.id);
    let outcome=execution::supervise(pool.clone(),work.leases.clone(),db::write::<_,anyhow::Error,_>(&pool,|tx|Box::pin(async move {
            sqlx::query("UPDATE background_work SET error_code='guarded-write' WHERE id='maintenance:a'").execute(&mut **tx).await?;Ok(())
        }))).await;
    outcome.unwrap().unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM background_work WHERE error_code='guarded-write'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn maintenance_takeover_fences_old_progress_and_completion_and_reuses_the_key() {
    let pool = db::test_pool_with_max_connections(3).await;
    ensure(&pool, "a", "test-maintenance", json!({"cursor":7}))
        .await
        .unwrap();
    let old = claim(&pool, "test-maintenance", "old")
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let new = claim(&pool, "test-maintenance", "new")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new.id, old.id);
    assert_eq!(new.payload, old.payload);
    assert!(new.leases[0].generation > old.leases[0].generation);
    assert!(finish(&pool, &old, false).await.is_err());
    let rejected = execution::supervise(pool.clone(), old.leases.clone(), async {
        panic!("stale work was polled")
    })
    .await;
    assert!(rejected.is_none());
    new.retry(&pool).await.unwrap();
    assert_eq!(count(&pool,"SELECT count(*) FROM background_work WHERE state='queued' AND error_code='maintenance_deferred'").await,1);
    assert!(
        claim(&pool, "test-maintenance", "next")
            .await
            .unwrap()
            .is_none()
    );
    due(&pool).await;
    let next = claim(&pool, "test-maintenance", "next")
        .await
        .unwrap()
        .unwrap();
    finish(&pool, &next, false).await.unwrap();
    ensure(&pool, "a", "test-maintenance", json!({}))
        .await
        .unwrap();
    assert!(
        claim(&pool, "test-maintenance", "early")
            .await
            .unwrap()
            .is_none()
    );
    due(&pool).await;
    ensure(&pool, "a", "test-maintenance", json!({}))
        .await
        .unwrap();
    let recurring = claim(&pool, "test-maintenance", "recurring")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recurring.id, old.id);
    assert!(recurring.leases[0].generation > next.leases[0].generation);
    assert_eq!(recurring.payload, json!({"cursor":7}));
}
#[tokio::test]
async fn claimed_maintenance_rows_are_skipped_and_progress_acknowledgement_is_atomic() {
    let pool = db::test_pool_with_max_connections(3).await;
    ensure(&pool, "a", "test-maintenance", json!({}))
        .await
        .unwrap();
    ensure(&pool, "b", "test-maintenance", json!({}))
        .await
        .unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM background_work WHERE id='maintenance:a' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let work = claim(&pool, "test-maintenance", "owner")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(work.id, "maintenance:b");
    held.rollback().await.unwrap();
    let aborted: Result<()> = db::write(&pool, |tx| {
        let work = work.clone();
        Box::pin(async move {
            work.guard_on(tx).await?;
            sqlx::query(
                "UPDATE background_work SET payload='{\"cursor\":99}' WHERE id='maintenance:b'",
            )
            .execute(&mut **tx)
            .await?;
            work.finish_on(tx, false, 86400, None).await?;
            anyhow::bail!("rollback all progress and acknowledgement")
        })
    })
    .await;
    assert!(aborted.is_err());
    assert_eq!(count(&pool,"SELECT count(*) FROM background_work WHERE id='maintenance:b' AND state='running' AND payload='{}'").await,1);
    assert!(work.leases[0].renew(&pool).await.unwrap());
    finish(&pool, &work, false).await.unwrap();
}
