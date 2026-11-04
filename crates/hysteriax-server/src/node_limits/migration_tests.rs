use super::*;

#[tokio::test]
async fn schema_nine_upgrade_preserves_packages_and_admitted_work_and_removes_only_old_scanner() {
    let pool = db::test_pool().await;
    let mut connection = pool.acquire().await.unwrap();
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP SCHEMA {schema} CASCADE; CREATE SCHEMA {schema}; SET search_path TO {schema}"
    )))
    .execute(&mut *connection)
    .await
    .unwrap();
    for migration in db::MIGRATOR.iter().filter(|m| m.version < 10) {
        sqlx::raw_sql(sqlx::AssertSqlSafe(migration.sql.as_ref()))
            .execute(&mut *connection)
            .await
            .unwrap();
    }
    sqlx::raw_sql("INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('ssh','SSH','ssh_password',now(),now()); INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('ssh',1,'cipher',now()); INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,ssh_credential_id,ssh_credential_version,created_at,updated_at) VALUES('node','Node','127.0.0.1',22,'root','node.test',443,':443','hash','cipher','cipher','cipher','ssh',1,now(),now()); INSERT INTO node_packages(node_id,config,usage_bytes,period_id,restricted) VALUES('node','{\"quota_bytes\":100}',17,'period',true); INSERT INTO background_work(id,kind,payload) VALUES('maintenance:node-limits:evaluate','node-limit-scan','{\"after_node\":\"old\"}'),('node-limit:node:1','node-limit-dispatch','{\"node_id\":\"node\"}'); INSERT INTO resource_leases(resource_key) VALUES('work:maintenance:node-limits:evaluate'),('work:node-limit:node:1')")
        .execute(&mut *connection).await.unwrap();
    let before: Value =
        sqlx::query_scalar("SELECT to_jsonb(p) FROM node_packages p WHERE node_id='node'")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
    sqlx::raw_sql(include_str!("../../migrations/0010_lock_contention.sql"))
        .execute(&mut *connection)
        .await
        .unwrap();
    let after: Value = sqlx::query_scalar(
        "SELECT to_jsonb(p)-'next_evaluation_at' FROM node_packages p WHERE node_id='node'",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(before, after);
    let values:(bool,i64,i64,bool)=sqlx::query_as("SELECT (SELECT next_evaluation_at IS NOT NULL FROM node_packages WHERE node_id='node'),(SELECT count(*) FROM background_work),(SELECT count(*) FROM resource_leases),to_regclass(current_schema()||'.analytics_retry_guards') IS NULL")
        .fetch_one(&mut *connection).await.unwrap();
    assert_eq!(values, (true, 1, 1, true));
}
