//! Transactional flow totals and idempotently rebuilt online-cycle analytics.
use crate::{db, error::ApiError, state::AppState};
use sqlx::PgPool;
pub async fn refresh(pool: &PgPool) -> Result<(), ApiError> {
    let dirty: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM analytics_dirty_cycles) OR EXISTS(SELECT 1 FROM traffic_dirty_hours) OR EXISTS(SELECT 1 FROM online_dirty_minutes) OR EXISTS(SELECT 1 FROM job_retry_state_dirty)")
        .fetch_one(pool)
        .await?;
    if !dirty {
        return Ok(());
    }
    let mut tx = db::begin_write(pool).await?;
    refresh_on(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}
pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
    loop {
        interval.tick().await;
        if let Err(error) = refresh(&state.pool).await {
            tracing::error!(code = error.code, "analytics refresh failed");
        }
    }
}

async fn refresh_on(connection: &mut sqlx::PgConnection) -> Result<(), ApiError> {
    let cycles: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT cycle_id FROM analytics_dirty_cycles ORDER BY cycle_id")
            .fetch_all(&mut *connection)
            .await?;
    if !cycles.is_empty() {
        sqlx::query("DELETE FROM online_cycle_metrics WHERE cycle_id=ANY($1)")
            .bind(&cycles)
            .execute(&mut *connection)
            .await?;
        sqlx::query(include_str!("analytics/refresh_cycles.sql"))
            .bind(&cycles)
            .execute(&mut *connection)
            .await?;
        sqlx::query("DELETE FROM analytics_dirty_cycles WHERE cycle_id=ANY($1)")
            .bind(&cycles)
            .execute(&mut *connection)
            .await?;
    }
    for statement in include_str!("analytics/refresh_rollups.sql")
        .split(';')
        .filter(|s| !s.trim().is_empty())
    {
        sqlx::query(statement).execute(&mut *connection).await?;
    }
    let dirty: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM job_retry_state_dirty)")
        .fetch_one(&mut *connection)
        .await?;
    if dirty {
        let roots: Vec<String> = sqlx::query_scalar(include_str!("analytics/retry_roots.sql"))
            .fetch_all(&mut *connection)
            .await?;
        sqlx::query("DELETE FROM job_retry_chain_state WHERE root_id=ANY($1)")
            .bind(&roots)
            .execute(&mut *connection)
            .await?;
        sqlx::query("DELETE FROM job_retry_chain_members WHERE root_id=ANY($1)")
            .bind(&roots)
            .execute(&mut *connection)
            .await?;
        sqlx::query(include_str!("analytics/refresh_retries.sql"))
            .bind(&roots)
            .execute(&mut *connection)
            .await?;
        sqlx::query("DELETE FROM job_retry_state_dirty")
            .execute(&mut *connection)
            .await?;
    }

    Ok(())
}

pub async fn snapshot(
    pool: &PgPool,
    end: chrono::DateTime<chrono::Utc>,
    needs_cycles: bool,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, ApiError> {
    for _ in 0..3 {
        let mut tx = pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        let dirty = if needs_cycles {
            sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM analytics_dirty_cycles d WHERE EXISTS(SELECT 1 FROM online_samples s WHERE s.cycle_id=d.cycle_id AND s.cycle_started_at<$1) OR EXISTS(SELECT 1 FROM online_cycle_metrics m WHERE m.cycle_id=d.cycle_id AND m.started_at<$1))").bind(end).fetch_one(&mut *tx).await?
        } else {
            false
        };
        if !dirty {
            return Ok(tx);
        }
        tx.rollback().await?;
        refresh(pool).await?;
    }
    let mut tx = db::begin_write(pool).await?;
    refresh_on(&mut tx).await?;
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use serde_json::json;
    pub(super) async fn seed() -> PgPool {
        let pool = crate::db::test_pool().await;
        sqlx::query(
            "INSERT INTO users(id,name,created_at,updated_at) VALUES('u','User',now(),now())",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT 'n'||i,'Node '||i,'127.0.0.1',22,'root','private_key','unused','example.test',443,':443','hash'||i,'unused','unused','{}',now(),now() FROM generate_series(1,2)i").execute(&pool).await.unwrap();
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
}

#[cfg(test)]
#[path = "analytics/followup_tests.rs"]
mod followup_tests;
