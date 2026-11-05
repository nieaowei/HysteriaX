use super::*;
use crate::execution::{self, CURRENT, Scope};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[tokio::test]
async fn native_intent_and_receipt_retries_never_reinvoke_the_network_factory() {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let mut tx = pool.begin().await.unwrap();
    let lease = execution::acquire(&mut tx, "node:retries", "owner", "work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::raw_sql("CREATE SEQUENCE intent_attempts; CREATE SEQUENCE receipt_attempts; CREATE SEQUENCE operation_binding MINVALUE 0 START 0; CREATE FUNCTION retry_remote_intent() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('intent_attempts')=1 THEN PERFORM setval('operation_binding',hashtextextended(NEW.operation_id,0) & 9223372036854775807,true); RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='retry intent'; END IF; IF (hashtextextended(NEW.operation_id,0) & 9223372036854775807)<>(SELECT last_value FROM operation_binding) THEN RAISE EXCEPTION 'operation ID changed on retry'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_remote_intent BEFORE INSERT ON execution_operations FOR EACH ROW EXECUTE FUNCTION retry_remote_intent(); CREATE FUNCTION retry_remote_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state='verified' AND nextval('receipt_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40P01',MESSAGE='retry receipt'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_remote_receipt BEFORE UPDATE ON execution_operations FOR EACH ROW EXECUTE FUNCTION retry_remote_receipt();")
            .execute(&pool).await.unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let scope = Scope {
        pool: pool.clone(),
        leases: vec![lease],
        sequence: Default::default(),
    };
    let result = CURRENT
        .scope(
            scope,
            with_context(
                "ssh-command",
                "digest".into(),
                serde_json::json!({"protocol":"test"}),
                move |_| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async { Ok(42) }
                },
                |v| serde_json::json!({"value":v}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(result, 42);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM execution_operations WHERE state='verified'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM intent_attempts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM receipt_attempts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}
