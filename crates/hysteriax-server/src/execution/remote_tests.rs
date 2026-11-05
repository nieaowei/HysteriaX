use super::*;
use crate::execution::{self, CURRENT, Scope};
use sqlx::Row;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[tokio::test]
async fn factory_receives_committed_context_and_is_not_reinvoked_for_an_uncertain_step() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let mut tx = pool.begin().await.unwrap();
    let lease = execution::acquire(&mut tx, "node:factory", "owner", "work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    for attempt in 0..2 {
        let scope = Scope {
            lifecycle: None,
            pool: pool.clone(),
            leases: vec![lease.clone()],
            sequence: Default::default(),
        };
        let (calls, pool) = (count.clone(), pool.clone());
        let result: anyhow::Result<()> = CURRENT.scope(scope,with_context("ssh-command","digest".into(),serde_json::json!({"protocol":"test"}),move |context| {
                calls.fetch_add(1,Ordering::SeqCst);
                async move {
                    let c=context.unwrap();
                    let row=sqlx::query("SELECT resource_key,owner,generation,epoch,work_id,step,state,intent FROM execution_operations WHERE operation_id=$1").bind(&c.operation_id).fetch_one(&pool).await?;
                    assert_eq!(row.get::<String,_>("state"),"dispatching");
                    assert_eq!(row.get::<String,_>("resource_key"),c.resource);assert_eq!(row.get::<String,_>("epoch"),c.epoch);
                    assert_eq!(row.get::<String,_>("owner"),c.owner);assert_eq!(row.get::<i64,_>("generation"),c.generation);
                    assert_eq!(row.get::<String,_>("work_id"),c.work);assert_eq!(row.get::<String,_>("step"),c.step);
                    assert_eq!(row.get::<Value,_>("intent"),serde_json::json!({"kind":"ssh-command","request_digest":"digest","verification":{"protocol":"test"}}));
                    anyhow::bail!("lost response")
                }
            },|_|Value::Null)).await;
        assert!(result.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 1, "attempt {attempt}");
    }
}
