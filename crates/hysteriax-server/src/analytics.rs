//! Transactional flow totals and idempotently rebuilt online-cycle analytics.
use crate::{db, error::ApiError, state::AppState};
use serde_json::Value;
use sqlx::PgPool;
pub async fn refresh(pool: &PgPool) -> Result<(), sqlx::Error> {
    let mut committed = false;
    loop {
        let dirty: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM analytics_dirty_cycles) OR EXISTS(SELECT 1 FROM traffic_dirty_hours) OR EXISTS(SELECT 1 FROM online_dirty_minutes) OR EXISTS(SELECT 1 FROM job_retry_state_dirty)")
            .fetch_one(pool).await?;
        if !dirty {
            return Ok(());
        }
        if committed {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let processed = db::write(pool, |tx| {
            Box::pin(async move { refresh_on(tx, Some(256)).await })
        })
        .await?;
        if processed == 0 {
            return Ok(());
        } // Other refreshers own the remaining keys.
        committed = true;
    }
}
pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
    loop {
        interval.tick().await;
        if let Err(error) = refresh(&state.pool).await {
            tracing::error!(%error, "analytics refresh failed");
        }
    }
}

async fn refresh_on(
    connection: &mut sqlx::PgConnection,
    max_cycles: Option<i64>,
) -> Result<usize, sqlx::Error> {
    let limit = max_cycles.unwrap_or(256).clamp(1, 256);
    sqlx::query("SET LOCAL jit=off")
        .execute(&mut *connection)
        .await?;
    let claimed:Vec<(uuid::Uuid,i64)>=sqlx::query_as("SELECT cycle_id,generation FROM analytics_dirty_cycles ORDER BY cycle_id LIMIT $1 FOR UPDATE SKIP LOCKED")
        .bind(limit).fetch_all(&mut *connection).await?;
    let cycles: Vec<_> = claimed.iter().map(|(id, _)| *id).collect();
    let generations: Vec<_> = claimed.iter().map(|(_, generation)| *generation).collect();
    if !cycles.is_empty() {
        sqlx::query("DELETE FROM online_cycle_metrics m WHERE m.cycle_id=ANY($1) AND NOT EXISTS(SELECT 1 FROM online_samples s WHERE s.cycle_id=m.cycle_id AND (m.scope_node_id='' OR s.node_id=m.scope_node_id))")
            .bind(&cycles).execute(&mut *connection).await?;
        sqlx::query(include_str!("analytics/refresh_cycles.sql"))
            .bind(&cycles)
            .execute(&mut *connection)
            .await?;
        sqlx::query("DELETE FROM analytics_dirty_cycles d USING unnest($1::uuid[],$2::bigint[]) c(id,generation) WHERE d.cycle_id=c.id AND d.generation=c.generation")
            .bind(&cycles).bind(&generations).execute(&mut *connection).await?;
    }
    // Rollup inserts carry a node foreign key. Take parent key-share locks
    // before queue rows, so deletion never waits on a queue whose refresher
    // is itself waiting for the deleting node. Missing parents need no lock.
    let candidates: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT node_id FROM traffic_dirty_hours ORDER BY node_id LIMIT $1",
    )
    .bind(limit)
    .fetch_all(&mut *connection)
    .await?;
    let parents: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM nodes WHERE id=ANY($1::text[]) ORDER BY id FOR KEY SHARE SKIP LOCKED",
    )
    .bind(&candidates)
    .fetch_all(&mut *connection)
    .await?;
    let hours:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('source',source,'node_id',node_id,'hour',hour,'generation',generation) FROM traffic_dirty_hours d WHERE d.node_id=ANY($2::text[]) OR (d.node_id=ANY($3::text[]) AND NOT EXISTS(SELECT 1 FROM nodes n WHERE n.id=d.node_id)) ORDER BY source,node_id,hour LIMIT $1 FOR UPDATE SKIP LOCKED")
        .bind(limit).bind(&parents).bind(&candidates).fetch_all(&mut *connection).await?;
    let minutes:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('scope_node_id',scope_node_id,'minute',minute,'generation',generation) FROM online_dirty_minutes ORDER BY scope_node_id,minute LIMIT $1 FOR UPDATE SKIP LOCKED")
        .bind(limit).fetch_all(&mut *connection).await?;
    let count = cycles.len() + hours.len() + minutes.len();
    let (hours, minutes) = (Value::Array(hours), Value::Array(minutes));
    for statement in include_str!("analytics/refresh_rollups.sql")
        .split(';')
        .filter(|s| !s.trim().is_empty())
    {
        let batch = if statement.contains("traffic_hour_totals") {
            &hours
        } else {
            &minutes
        };
        if batch.as_array().is_some_and(|rows| !rows.is_empty()) {
            sqlx::query(statement)
                .bind(batch)
                .execute(&mut *connection)
                .await?;
        }
    }
    sqlx::query("DELETE FROM traffic_dirty_hours d USING jsonb_to_recordset($1::jsonb) c(source text,node_id text,hour timestamptz,generation bigint) WHERE (d.source,d.node_id,d.hour,d.generation)=(c.source,c.node_id,c.hour,c.generation)")
        .bind(&hours).execute(&mut *connection).await?;
    sqlx::query("DELETE FROM online_dirty_minutes d USING jsonb_to_recordset($1::jsonb) c(scope_node_id text,minute timestamptz,generation bigint) WHERE (d.scope_node_id,d.minute,d.generation)=(c.scope_node_id,c.minute,c.generation)")
        .bind(&minutes).execute(&mut *connection).await?;
    let jobs:Vec<(String,i64)>=sqlx::query_as("SELECT job_id,generation FROM job_retry_state_dirty ORDER BY job_id LIMIT $1 FOR UPDATE SKIP LOCKED")
        .bind(limit).fetch_all(&mut *connection).await?;
    if !jobs.is_empty() {
        let ids: Vec<_> = jobs.iter().map(|(id, _)| id.clone()).collect();
        let versions: Vec<_> = jobs.iter().map(|(_, generation)| *generation).collect();
        let roots: Vec<String> = sqlx::query_scalar(include_str!("analytics/retry_roots.sql"))
            .bind(&ids)
            .fetch_all(&mut *connection)
            .await?;
        // Different dirty jobs can belong to the same retry chain. Coordinate
        // that derived resource, without a lock shared by unrelated chains.
        sqlx::query("INSERT INTO analytics_retry_guards(root_id) SELECT id FROM unnest($1::text[]) id ORDER BY id ON CONFLICT DO NOTHING")
            .bind(&roots).execute(&mut *connection).await?;
        sqlx::query("SELECT root_id FROM analytics_retry_guards WHERE root_id=ANY($1::text[]) ORDER BY root_id FOR UPDATE")
            .bind(&roots).fetch_all(&mut *connection).await?;
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
        sqlx::query("DELETE FROM job_retry_state_dirty d USING unnest($1::text[],$2::bigint[]) c(id,generation) WHERE d.job_id=c.id AND d.generation=c.generation")
            .bind(&ids).bind(&versions).execute(&mut *connection).await?;
    }
    Ok(count + jobs.len())
}

pub async fn snapshot(
    pool: &PgPool,
    end: chrono::DateTime<chrono::Utc>,
    needs_cycles: bool,
) -> Result<crate::read_snapshot::Reader, ApiError> {
    if let Some(reader) = crate::read_snapshot::current().await {
        return Ok(reader);
    }
    Ok(crate::read_snapshot::Reader::owned(
        snapshot_transaction(pool, end, needs_cycles).await?,
    ))
}

pub(crate) async fn snapshot_transaction(
    pool: &PgPool,
    _end: chrono::DateTime<chrono::Utc>,
    _needs_cycles: bool,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, ApiError> {
    // History overlays dirty source keys inside this same snapshot. A reader
    // neither drains another worker's queue nor falls back to a global writer.
    Ok(crate::read_snapshot::transaction(pool).await?)
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

#[cfg(test)]
mod node_deletion_tests;

#[cfg(test)]
#[path = "analytics/dirty_queue_tests.rs"]
mod dirty_queue_tests;
