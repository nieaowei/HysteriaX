use super::*;
use serde_json::Value;

async fn snapshot(pool: &PgPool) -> Vec<(String, Value)> {
    let tables:Vec<String>=sqlx::query_scalar("SELECT tablename FROM pg_tables WHERE schemaname=current_schema() AND tablename<>'_sqlx_migrations' ORDER BY tablename")
        .fetch_all(pool).await.unwrap();
    let mut result = vec![];
    for table in tables {
        let quoted = table.replace('"', "\"\"");
        let data:Value=sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT coalesce(jsonb_agg(to_jsonb(r) ORDER BY to_jsonb(r)::text),'[]'::jsonb) FROM \"{quoted}\" r")))
            .fetch_one(pool).await.unwrap();
        result.push((table, data));
    }
    result
}

#[tokio::test]
async fn cutover_removes_only_global_triggers_and_preserves_every_business_table() {
    let pool = test_pool_through_version(4, 64).await;
    sqlx::query("INSERT INTO users(id,name,enabled,usage_bytes,created_at,updated_at) VALUES('retained','Retained',true,123456789,clock_timestamp(),clock_timestamp())").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('retained','sync','failed','retained',clock_timestamp(),clock_timestamp(),clock_timestamp())").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO job_events(job_id,event_type,payload_json,created_at) VALUES('retained','job.failed','{\"retained\":true}',clock_timestamp())").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('retained','retained-digest','Retained',clock_timestamp())").execute(&pool).await.unwrap();
    let old:i64=sqlx::query_scalar("SELECT count(*) FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes'").fetch_one(&pool).await.unwrap();
    assert!(old > 0);
    let native:Vec<String>=sqlx::query_scalar("SELECT pg_get_triggerdef(t.oid) FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname<>'hysteriax_serialize_writes' ORDER BY pg_get_triggerdef(t.oid)").fetch_all(&pool).await.unwrap();
    let before = snapshot(&pool).await;
    sqlx::raw_sql(include_str!(
        "../../migrations/0065_remove_global_write_lock.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(snapshot(&pool).await, before);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM pg_proc WHERE pronamespace=current_schema()::regnamespace AND proname='hysteriax_serialize_writes'").fetch_one(&pool).await.unwrap(),0);
    let after:Vec<String>=sqlx::query_scalar("SELECT pg_get_triggerdef(t.oid) FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace ORDER BY pg_get_triggerdef(t.oid)").fetch_all(&pool).await.unwrap();
    assert_eq!(after, native);
    sqlx::raw_sql(include_str!(
        "../../migrations/0065_remove_global_write_lock.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(snapshot(&pool).await, before);
}

#[tokio::test]
async fn runtime_writes_do_not_take_retired_locks_even_when_the_old_key_is_held() {
    let pool = test_pool_with_max_connections(4).await;
    let held = hold_retired_global_write_lock(&pool).await.unwrap();
    for (id, serial) in [("committed", false), ("serializable", true)] {
        let held_count = if serial {
            serializable(&pool, |tx| Box::pin(insert_and_count(tx, id)))
                .await
                .unwrap()
        } else {
            write(&pool, |tx| Box::pin(insert_and_count(tx, id)))
                .await
                .unwrap()
        };
        assert_eq!(held_count, 0);
    }
    held.rollback().await.unwrap();
}

async fn insert_and_count(
    tx: &mut Transaction<'static, Postgres>,
    id: &'static str,
) -> Result<i64, sqlx::Error> {
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES($1,$1,$1,clock_timestamp())").bind(id).execute(&mut **tx).await?;
    sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE pid=pg_backend_pid() AND locktype='advisory' AND objsubid=1 AND classid::bigint*4294967296+objid::bigint IN (62177345902811,62177345902812)").fetch_one(&mut **tx).await
}
