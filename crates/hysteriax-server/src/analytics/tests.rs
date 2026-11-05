use super::*;
use chrono::{DateTime, Utc};
use serde_json::json;
pub(super) async fn seed() -> PgPool {
    let pool = crate::db::test_pool().await;
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('u','User',now(),now())")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("WITH fixture_ssh AS (INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('fixture-ssh','Fixture SSH','ssh_password',now(),now()) ON CONFLICT DO NOTHING), fixture_ssh_version AS (INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('fixture-ssh',1,'unused',now()) ON CONFLICT DO NOTHING) INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at,ssh_credential_id,ssh_credential_version) SELECT 'n'||i,'Node '||i,'127.0.0.1',22,'root','example.test',443,':443','hash'||i,'unused','unused','{}',now(),now(),'fixture-ssh',1 FROM generate_series(1,2)i").execute(&pool).await.unwrap();
    pool
}
#[tokio::test]
async fn flow_models_follow_updates_bulk_deletes_and_never_change_billing() {
    let pool = seed().await;
    sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) VALUES('a','n1','u','instance',0,0,10,20,'2026-01-01T00:00:01Z'),('b','n1','u','instance',0,0,30,40,'2026-01-01T00:00:02Z')").execute(&pool).await.unwrap();
    let total: (i64, i64) = sqlx::query_as(
        "SELECT tx::bigint,rx::bigint FROM user_node_usage WHERE user_id='u' AND node_id='n1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(total, (40, 60));
    sqlx::query("UPDATE traffic_records SET node_id='n2',delta_tx=50,sampled_at='2026-01-01T00:01:01Z' WHERE id='a'").execute(&pool).await.unwrap();
    let totals: Vec<(String, i64)> =
        sqlx::query_as("SELECT node_id,tx::bigint FROM user_node_usage ORDER BY node_id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(totals, vec![("n1".into(), 30), ("n2".into(), 50)]);
    sqlx::query("UPDATE traffic_records SET sampled_at='2026-01-01T00:02:00Z'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM traffic_records")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_minute_totals")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_node_usage")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id='u'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) VALUES('c','n1','u','instance',0,0,10,20,now()),('d','n1','u','instance',0,0,30,40,now())").execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM nodes WHERE id='n1'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM user_node_usage")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn cycle_metrics_deduplicate_users_and_rebuild_late_changed_and_deleted_samples() {
    let pool = seed().await;
    let cycle = uuid::Uuid::new_v4();
    let at: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
    for (node, count) in [("n1", 2), ("n2", 3)] {
        sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,cycle_started_at,status,traffic_status,connections,expected_nodes) VALUES($1,$2,$3,$3,'ok','ok',$4,2)").bind(cycle).bind(node).bind(at).bind(json!({"u":count})).execute(&pool).await.unwrap();
    }
    refresh(&pool).await.unwrap();
    refresh(&pool).await.unwrap();
    let (users,connections,covered,incomplete):(i64,i64,i64,bool)=sqlx::query_as("SELECT users,connections::bigint,covered,incomplete FROM online_cycle_metrics WHERE cycle_id=$1 AND scope_node_id=''").bind(cycle).fetch_one(&pool).await.unwrap();
    assert_eq!((users, connections, covered, incomplete), (1, 5, 2, false));
    sqlx::query("UPDATE online_samples SET connections='{\"u\":7,\"other\":1}',traffic_status='failed' WHERE node_id='n2'").execute(&pool).await.unwrap();
    refresh(&pool).await.unwrap();
    let totals:(i64,i64,bool)=sqlx::query_as("SELECT users,connections::bigint,traffic_incomplete FROM online_cycle_metrics WHERE cycle_id=$1 AND scope_node_id=''").bind(cycle).fetch_one(&pool).await.unwrap();
    assert_eq!(totals, (2, 10, true));
    sqlx::query("DELETE FROM nodes WHERE id='n2'")
        .execute(&pool)
        .await
        .unwrap();
    refresh(&pool).await.unwrap();
    let covered:(i64,bool)=sqlx::query_as("SELECT covered,incomplete FROM online_cycle_metrics WHERE cycle_id=$1 AND scope_node_id=''").bind(cycle).fetch_one(&pool).await.unwrap();
    assert_eq!(covered, (1, true));
    sqlx::query("DELETE FROM online_samples")
        .execute(&pool)
        .await
        .unwrap();
    refresh(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM online_cycle_metrics")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}
