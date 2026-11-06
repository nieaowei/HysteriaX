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

#[tokio::test]
async fn release_rejects_each_stale_identity_and_preserves_the_live_lease() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let lease = claim(&pool, "work:release", "owner", "work").await.unwrap();
    let mut invalid = Vec::new();
    let mut other = lease.clone();
    other.owner = "another-owner".into();
    invalid.push(other);
    let mut other = lease.clone();
    other.generation += 1;
    invalid.push(other);
    let mut other = lease.clone();
    other.work = "another-work".into();
    invalid.push(other);
    let mut other = lease.clone();
    other.epoch = uuid::Uuid::new_v4().to_string();
    invalid.push(other);
    let mut other = lease.clone();
    other.resource = "work:missing".into();
    invalid.push(other);
    for stale in invalid {
        let mut tx = pool.begin().await.unwrap();
        assert!(stale.release(&mut tx).await.is_err());
        tx.rollback().await.unwrap();
        lease
            .guard(&mut pool.acquire().await.unwrap())
            .await
            .unwrap();
    }
    for mutation in [
        "UPDATE resource_leases SET blocked=true WHERE resource_key='work:release'",
        "UPDATE resource_leases SET blocked=false,expires_at=clock_timestamp()-interval '1 second' WHERE resource_key='work:release'",
    ] {
        sqlx::query(mutation).execute(&pool).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert!(lease.release(&mut tx).await.is_err());
        tx.rollback().await.unwrap();
        let owner: Option<String> = sqlx::query_scalar(
            "SELECT owner FROM resource_leases WHERE resource_key='work:release'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(owner.as_deref(), Some("owner"));
    }
}

#[tokio::test]
async fn release_preserves_every_unverified_remote_operation_barrier() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let lease = claim(&pool, "node:release", "owner", "work").await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let (operation, _) = lease.dispatch(&mut tx, "change", &json!({})).await.unwrap();
    tx.commit().await.unwrap();
    for state in ["dispatching", "reconciling", "needs_attention"] {
        sqlx::query("UPDATE execution_operations SET state=$2 WHERE operation_id=$1")
            .bind(&operation)
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert!(!lease.release(&mut tx).await.unwrap());
        tx.commit().await.unwrap();
        lease
            .guard(&mut pool.acquire().await.unwrap())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn waiting_release_cannot_clear_a_replacement_owner() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let lease = claim(&pool, "work:release", "owner", "work").await.unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query(
        "SELECT resource_key FROM resource_leases WHERE resource_key='work:release' FOR UPDATE",
    )
    .fetch_one(&mut *held)
    .await
    .unwrap();
    let mut connection = pool.acquire().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    let release = tokio::spawn(async move { lease.release(&mut connection).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')")
                .bind(pid).fetch_one(&pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    sqlx::query("UPDATE resource_leases SET owner='replacement',generation=generation+1,work_id='new-work' WHERE resource_key='work:release'")
        .execute(&mut *held).await.unwrap();
    held.commit().await.unwrap();
    assert!(release.await.unwrap().is_err());
    let (owner, work): (String, String) = sqlx::query_as(
        "SELECT owner,work_id FROM resource_leases WHERE resource_key='work:release'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((owner.as_str(), work.as_str()), ("replacement", "new-work"));
}

#[tokio::test]
async fn waiting_release_observes_a_remote_intent_committed_by_the_lock_holder() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let lease = claim(&pool, "node:release", "owner", "work").await.unwrap();
    let mut held = pool.begin().await.unwrap();
    lease.guard(&mut held).await.unwrap();
    let mut pending = pool.begin().await.unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *pending)
        .await
        .unwrap();
    let releasing = lease.clone();
    let release = tokio::spawn(async move {
        let result = releasing.release(&mut pending).await;
        pending.commit().await.unwrap();
        result
    });
    tokio::time::timeout(std::time::Duration::from_secs(1),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')")
                .bind(pid).fetch_one(&pool).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    let (operation, _) = lease
        .dispatch(&mut held, "change", &json!({}))
        .await
        .unwrap();
    held.commit().await.unwrap();
    assert!(
        !release.await.unwrap().unwrap(),
        "a newly committed remote operation must keep its lease"
    );
    lease
        .guard(&mut pool.acquire().await.unwrap())
        .await
        .unwrap();
    let status: String =
        sqlx::query_scalar("SELECT state FROM execution_operations WHERE operation_id=$1")
            .bind(operation)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "dispatching");
}
