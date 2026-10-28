use super::*;
use serde_json::json;

async fn initialized() -> (PgPool, crate::security::SecretBox) {
    let pool = crate::db::test_pool_with_max_connections(8).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    crate::credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    crate::traffic::work::initialize(&pool).await.unwrap();
    crate::traffic::access::initialize(&pool).await.unwrap();
    crate::bootstrap::finalize(&pool, &secrets).await.unwrap();
    (pool, secrets)
}
async fn claim(pool: &PgPool, key: &str) -> Execution {
    let mut tx = pool.begin().await.unwrap();
    let lease = acquire(&mut tx, key, "owner", "work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    lease
}

#[tokio::test]
async fn maintenance_fences_identical_restored_owner_generation_and_work() {
    let (pool, secrets) = initialized().await;
    let old = claim(&pool, "epoch-test:a").await;
    let mut tx = pool.begin().await.unwrap();
    let (operation, _) = old
        .dispatch(&mut tx, "step", &json!({"test":true}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    crate::bootstrap::finalize(&pool, &secrets).await.unwrap();
    let epoch: String = sqlx::query_scalar("SELECT hysteriax_execution_epoch()")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(epoch, old.epoch);
    // A restored counter can collide exactly; expiry alone is not the fence.
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()+interval '1 minute' WHERE resource_key=$1")
        .bind(&old.resource).execute(&pool).await.unwrap();
    let mut current = old.clone();
    current.epoch = epoch;
    let mut tx = pool.begin().await.unwrap();
    current.guard(&mut tx).await.unwrap();
    assert!(old.guard(&mut tx).await.is_err());
    assert!(old.release(&mut tx).await.is_err());
    assert!(old.dispatch(&mut tx, "late", &json!({})).await.is_err());
    assert!(
        old.verified(&mut tx, &operation, &json!({"late":true}))
            .await
            .is_err()
    );
    // Even a counter-colliding new context cannot acknowledge an old intent.
    assert!(
        current
            .verified(&mut tx, &operation, &json!({"late":true}))
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    assert!(!old.renew(&pool).await.unwrap());
    let scope = Scope {
        pool: pool.clone(),
        leases: vec![old],
        sequence: Default::default(),
    };
    assert!(observe_current(&scope).await.is_err());
    assert!(
        observe_current(&Scope {
            leases: vec![current],
            ..scope
        })
        .await
        .is_ok()
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM execution_operations WHERE operation_id=$1")
            .bind(operation)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "dispatching");
}

#[tokio::test]
async fn maintenance_expires_claims_without_clearing_unknown_operations() {
    let (pool, secrets) = initialized().await;
    let safe = claim(&pool, "epoch-test:safe").await;
    let uncertain = claim(&pool, "epoch-test:unknown").await;
    let mut tx = pool.begin().await.unwrap();
    let (operation, _) = uncertain
        .dispatch(&mut tx, "remote", &json!({"digest":"fixed"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(e)-'epoch' FROM execution_operations e WHERE operation_id=$1",
    )
    .bind(&operation)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE resource_leases SET blocked=true WHERE resource_key=$1")
        .bind(&uncertain.resource)
        .execute(&pool)
        .await
        .unwrap();
    crate::bootstrap::finalize(&pool, &secrets).await.unwrap();
    let after: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(e)-'epoch' FROM execution_operations e WHERE operation_id=$1",
    )
    .bind(&operation)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    assert!(!safe.renew(&pool).await.unwrap());
    let next = claim(&pool, &safe.resource).await;
    assert_eq!(next.generation, safe.generation + 1);
    assert_ne!(next.epoch, safe.epoch);
    let mut tx = pool.begin().await.unwrap();
    assert!(
        acquire(&mut tx, &uncertain.resource, "next", "next-work")
            .await
            .unwrap()
            .is_none()
    );
    tx.commit().await.unwrap();
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT blocked FROM resource_leases WHERE resource_key=$1")
            .bind(&uncertain.resource)
            .fetch_one(&pool)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn maintenance_waits_for_guarded_result_transaction_before_epoch_publication() {
    let (pool, secrets) = initialized().await;
    let old = claim(&pool, "epoch-test:held").await;
    let mut held = pool.begin().await.unwrap();
    old.guard(&mut held).await.unwrap();
    let (p, s) = (pool.clone(), secrets.clone());
    let mut maintenance = tokio::spawn(async move { crate::bootstrap::finalize(&p, &s).await });
    // Observe the actual lock waiter, rather than relying on scheduling sleeps.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE relation='resource_leases'::regclass AND mode='ExclusiveLock' AND NOT granted)").fetch_one(&pool).await.unwrap();
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert!(!maintenance.is_finished());
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT hysteriax_execution_epoch()")
            .fetch_one(&pool)
            .await
            .unwrap(),
        old.epoch
    );
    let p = pool.clone();
    let mut waiting_claim = tokio::spawn(async move { claim(&p, "epoch-test:new").await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting:bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE relation='resource_leases'::regclass AND mode='RowExclusiveLock' AND NOT granted)").fetch_one(&pool).await.unwrap();
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert!(!waiting_claim.is_finished());
    held.commit().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), &mut maintenance)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let new = tokio::time::timeout(std::time::Duration::from_secs(5), &mut waiting_claim)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(new.epoch, old.epoch);
    assert!(new.renew(&pool).await.unwrap());
    let mut tx = pool.begin().await.unwrap();
    assert!(old.guard(&mut tx).await.is_err());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn rejected_maintenance_contract_rolls_back_epoch_and_lease_expiry_together() {
    let (pool, secrets) = initialized().await;
    let old = claim(&pool, "epoch-test:rollback").await;
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(r) FROM resource_leases r")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_epoch_contract() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject maintenance contract'; END $$; CREATE TRIGGER reject_epoch_contract BEFORE INSERT OR UPDATE ON runtime_contract FOR EACH ROW EXECUTE FUNCTION reject_epoch_contract()")
        .execute(&pool).await.unwrap();
    assert!(crate::bootstrap::finalize(&pool, &secrets).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT hysteriax_execution_epoch()")
            .fetch_one(&pool)
            .await
            .unwrap(),
        old.epoch
    );
    assert_eq!(
        sqlx::query_scalar::<_, Value>("SELECT to_jsonb(r) FROM resource_leases r")
            .fetch_one(&pool)
            .await
            .unwrap(),
        before
    );
    assert!(old.renew(&pool).await.unwrap());
}
