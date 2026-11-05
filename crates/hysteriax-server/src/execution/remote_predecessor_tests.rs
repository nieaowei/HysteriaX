use super::*;
use crate::execution::{self, CURRENT, Scope};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[tokio::test]
async fn obsolete_scope_cannot_use_a_fresh_predecessor_or_construct_a_payload() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let mut tx = pool.begin().await.unwrap();
    let lease = execution::acquire(&mut tx, "node:old-head", "owner", "work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let epoch = uuid::Uuid::new_v4().to_string();
    sqlx::query("UPDATE cache_version_epoch SET epoch=$1 WHERE id")
        .bind(&epoch)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE resource_leases SET epoch=$1")
        .bind(&epoch)
        .execute(&pool)
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let scope = Scope {
        lifecycle: None,
        pool: pool.clone(),
        leases: vec![lease],
        sequence: Default::default(),
    };
    let result: anyhow::Result<()> = CURRENT
        .scope(
            scope,
            with_context(
                "ssh-command",
                "digest".into(),
                serde_json::json!({"expected_head":uuid::Uuid::new_v4().to_string()}),
                move |_| {
                    count.fetch_add(1, Ordering::SeqCst);
                    async { Ok(()) }
                },
                |_| Value::Null,
            ),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM execution_operations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
