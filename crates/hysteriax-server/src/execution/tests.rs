use super::*;
use serde_json::json;

async fn claim(pool: &PgPool, resource: &str, owner: &str, work: &str) -> Option<Execution> {
    let mut tx = pool.begin().await.unwrap();
    let context = acquire(&mut tx, resource, owner, work).await.unwrap();
    tx.commit().await.unwrap();
    context
}
async fn expire(pool: &PgPool, resource: &str) {
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key=$1")
            .bind(resource).execute(pool).await.unwrap();
}

#[tokio::test]
async fn expiry_fences_previous_owner_and_database_work_can_be_taken_over() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let a = claim(&pool, "node:a", "first", "work").await.unwrap();
    assert!(claim(&pool, "node:a", "second", "other").await.is_none());
    assert!(
        claim(&pool, "node:b", "second", "unrelated")
            .await
            .is_some()
    );
    expire(&pool, "node:a").await;
    assert!(!a.renew(&pool).await.unwrap());
    let b = claim(&pool, "node:a", "second", "work").await.unwrap();
    assert_eq!(b.generation, a.generation + 1);
    let mut tx = pool.begin().await.unwrap();
    assert!(a.guard(&mut tx).await.is_err());
    b.guard(&mut tx).await.unwrap();
    assert!(b.release(&mut tx).await.unwrap());
    tx.commit().await.unwrap();
    let c = claim(&pool, "node:a", "third", "next").await.unwrap();
    assert_eq!(c.generation, b.generation + 1);
}

#[tokio::test]
async fn ambiguous_external_request_blocks_takeover_and_replay() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let a = claim(&pool, "dns:zone", "first", "change").await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let (operation, fresh) = a
        .dispatch(&mut tx, "create", &json!({"name":"example.test"}))
        .await
        .unwrap();
    assert!(fresh);
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let (same, fresh) = a
        .dispatch(&mut tx, "create", &json!({"name":"example.test"}))
        .await
        .unwrap();
    assert_eq!(same, operation);
    assert!(!fresh);
    assert!(!a.release(&mut tx).await.unwrap());
    tx.commit().await.unwrap();
    expire(&pool, "dns:zone").await;
    assert!(claim(&pool, "dns:zone", "second", "change").await.is_none());
    assert!(
        claim(&pool, "dns:zone", "third", "different-change")
            .await
            .is_none()
    );
    let (blocked,state): (bool,String)=sqlx::query_as("SELECT l.blocked,o.state FROM resource_leases l JOIN execution_operations o USING(resource_key) WHERE l.resource_key='dns:zone'")
            .fetch_one(&pool).await.unwrap();
    assert!(blocked);
    assert_eq!(state, "reconciling");
    let mut tx = pool.begin().await.unwrap();
    assert!(
        a.verified(&mut tx, &operation, &json!({"receipt":"late"}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn verified_operation_allows_release_and_live_owner_is_not_stolen() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    heartbeat(&pool, "first").await.unwrap();
    heartbeat(&pool, "second").await.unwrap();
    let a = claim(&pool, "node:a", "first", "deploy").await.unwrap();
    assert!(a.renew(&pool).await.unwrap());
    assert!(claim(&pool, "node:a", "second", "deploy").await.is_none());
    let mut tx = pool.begin().await.unwrap();
    let (operation, _) = a
        .dispatch(&mut tx, "install", &json!({"digest":"abc"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    a.verified(&mut tx, &operation, &json!({"remote_digest":"abc"}))
        .await
        .unwrap();
    assert!(a.release(&mut tx).await.unwrap());
    tx.commit().await.unwrap();
    assert!(claim(&pool, "node:a", "second", "next").await.is_some());
}
