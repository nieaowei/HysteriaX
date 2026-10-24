use super::*;
use sqlx::Row;

fn uses_index(plan: &Value, name: &str) -> bool {
    (plan["Index Name"] == name && plan["Actual Loops"].as_u64().unwrap_or(0) > 0)
        || plan["Plans"]
            .as_array()
            .is_some_and(|children| children.iter().any(|child| uses_index(child, name)))
}

#[tokio::test]
async fn selective_history_search_and_status_paging_use_indexes() {
    let pool = crate::db::test_pool().await;
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at,error_message) SELECT 'indexed-'||i,'sync','failed','failed',now(),now(),now(),CASE WHEN i=1 THEN 'rare-needle\\%_result' ELSE 'ordinary message '||i END FROM generate_series(1,6000)i")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) SELECT 'audit-index-'||i,'admin','node.created','node',CASE WHEN i=1 THEN 'rare-needle\\%_result' ELSE 'ordinary-'||i END,'{}',now() FROM generate_series(1,6000)i")
        .execute(&pool).await.unwrap();
    sqlx::raw_sql("ANALYZE jobs; ANALYZE audit_records")
        .execute(&pool)
        .await
        .unwrap();
    let pattern = literal_search_pattern("rare-needle\\%_");
    for (table, expression, index) in [
        (
            "jobs",
            "hysteriax_search_text(id,kind,stage,status,node_name,node_id,resource_name,resource_id,error_message)",
            "jobs_search_trgm",
        ),
        (
            "audit_records",
            "hysteriax_search_text(id,action,actor,entity_type,entity_id)",
            "audit_search_trgm",
        ),
    ] {
        let mut tx = pool.begin().await.unwrap();
        sqlx::raw_sql("SET LOCAL jit=off; SET LOCAL plan_cache_mode=force_custom_plan")
            .execute(&mut *tx)
            .await
            .unwrap();
        let query = format!(
            "SELECT count(*) FROM {table} WHERE ($1='' OR {expression} LIKE lower($1) ESCAPE '\\')"
        );
        let actual: i64 = sqlx::query_scalar(&query)
            .bind(&pattern)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(actual, 1);
        let row = sqlx::query(&format!("EXPLAIN (ANALYZE,FORMAT JSON) {query}"))
            .bind(&pattern)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        let plan: Value = row.get("QUERY PLAN");
        assert!(uses_index(&plan[0]["Plan"], index), "{plan}");
        tx.rollback().await.unwrap();
    }
    for (table, sort, index) in [
        ("jobs", "status", "jobs_status_page"),
        ("audit_records", "action", "audit_action_page"),
    ] {
        let row = sqlx::query(&format!(
            "EXPLAIN (ANALYZE,FORMAT JSON) SELECT * FROM {table} ORDER BY {sort},id LIMIT 25"
        ))
        .fetch_one(&pool)
        .await
        .unwrap();
        let plan: Value = row.get("QUERY PLAN");
        assert!(uses_index(&plan[0]["Plan"], index), "{plan}");
    }
}

#[tokio::test]
async fn audit_totals_follow_bulk_changes_rollback_and_truncate() {
    let pool = crate::db::test_pool().await;
    let count = || sqlx::query_scalar::<_, i64>("SELECT total FROM audit_totals WHERE id");
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) SELECT 'a'||i,'admin','test','test','test','{}',now() FROM generate_series(1,100)i").execute(&pool).await.unwrap();
    assert_eq!(count().fetch_one(&pool).await.unwrap(), 100);
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM audit_records WHERE id IN ('a1','a2')")
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count().fetch_one(&mut *tx).await.unwrap(), 98);
    tx.rollback().await.unwrap();
    assert_eq!(count().fetch_one(&pool).await.unwrap(), 100);
    sqlx::query("DELETE FROM audit_records WHERE id IN ('a1','a2')")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count().fetch_one(&pool).await.unwrap(), 98);
    sqlx::query("TRUNCATE audit_records")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count().fetch_one(&pool).await.unwrap(), 0);
}

#[tokio::test]
async fn query_index_upgrade_can_roll_back_without_losing_source_rows() {
    // This exercises the 0025–0030 rollback chain against its original schema.
    // Later feature migrations intentionally block these old downgrade scripts.
    let pool = crate::db::test_pool_through_version(1, 30).await;
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES('kept','admin','test','test','test','{}',now())").execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../../scripts/rollback-ssh-test-confirmation.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../../scripts/rollback-traffic-record-indexes.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../../scripts/rollback-redis-quota-rank.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../../scripts/rollback-overview-node-pagination.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../../../scripts/rollback-query-optimization.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 24);
    let source: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_records WHERE id='kept'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(source, 1);
    // Re-upgrade through the maintenance boundary, which restores runtime
    // timeouts afterward. A raw SQLx run inherits the one-second runtime lock
    // deadline while other isolated schemas share its database migration lock.
    crate::db::run_migrations(&pool).await.unwrap();
    let total: i64 = sqlx::query_scalar("SELECT total FROM audit_totals WHERE id")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(total, 1);
}
