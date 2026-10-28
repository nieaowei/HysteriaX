use super::*;
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
