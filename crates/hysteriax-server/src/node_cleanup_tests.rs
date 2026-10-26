use super::*;

#[tokio::test]
#[ignore = "requires an explicitly restored local production snapshot"]
async fn restored_snapshot_removal_completes_while_reads_and_writes_continue() {
    let url = std::env::var("NODE_CLEANUP_SNAPSHOT_URL")
        .expect("explicit disposable snapshot URL required");
    let pool = db::pool_options()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        database.starts_with("hx_readyz_repro_"),
        "only a disposable hx_readyz_repro_ database is allowed"
    );
    let node = std::env::var("NODE_CLEANUP_SNAPSHOT_NODE").expect("snapshot target node required");
    let key = std::env::var("NODE_CLEANUP_SNAPSHOT_MASTER_KEY")
        .expect("explicit disposable snapshot master key required");
    let secrets = SecretBox::from_base64(&key).unwrap();
    crate::bootstrap::check_runtime(&pool, &secrets)
        .await
        .expect("snapshot must be initialized by --migrate-only before the test");
    sqlx::query("UPDATE jobs SET status='cancelled',finished_at=now() WHERE status IN ('queued','running','waiting_confirmation')").execute(&pool).await.unwrap();
    let revision: i64 = sqlx::query_scalar("SELECT desired_revision FROM nodes WHERE id=$1")
        .bind(&node)
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut before = Vec::new();
    for table in [
        "traffic_records",
        "node_network_samples",
        "online_samples",
        "proxy_probe_samples",
        "traffic_minute_totals",
        "traffic_hour_totals",
        "user_node_usage",
        "probe_minute_totals",
    ] {
        let counts:Vec<(String,i64)>=sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT node_id,count(*) FROM {table} WHERE node_id<>$1 GROUP BY node_id ORDER BY node_id"))).bind(&node).fetch_all(&pool).await.unwrap();
        before.push((table, counts));
    }
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('snapshot-probe',$1,'Snapshot validation',now())").bind(crate::security::token_digest("snapshot-probe-token")).execute(&pool).await.unwrap();
    let state = AppState::new(pool.clone(), secrets);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = crate::api::router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let started = std::time::Instant::now();
    let receipt = client
        .delete(format!(
            "{base}/api/v2/nodes/{node}/record?expected_revision={revision}"
        ))
        .bearer_auth("snapshot-probe-token")
        .header("Idempotency-Key", uuid::Uuid::new_v4().to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(receipt.status(), StatusCode::ACCEPTED);
    let accepted = started.elapsed();
    let job = claim_next(&pool).await.unwrap().unwrap();
    let id = job.id.clone();
    let worker = tokio::spawn(async move { execute_one(state, job).await });
    let mut reads = 0;
    let mut writes = 0;
    let mut max_read = Duration::ZERO;
    let mut max_write = Duration::ZERO;
    let mut last_progress = std::time::Instant::now() - Duration::from_secs(5);
    let completed = tokio::time::timeout(Duration::from_secs(180), async {
        while !worker.is_finished() {
            if last_progress.elapsed() >= Duration::from_secs(5) {
                let progress: (String, String, Option<i64>, Option<String>) = sqlx::query_as(
                    "SELECT status,stage,(result_json#>>'{result,removed_history_rows}')::bigint,result_json#>>'{result,cleanup_cursor,table}' FROM jobs WHERE id=$1",
                ).bind(&id).fetch_one(&pool).await.unwrap();
                println!("snapshot progress elapsed={:?} status={} stage={} removed={:?} table={:?} reads={reads} writes={writes} max_read={max_read:?} max_write={max_write:?}", started.elapsed(), progress.0, progress.1, progress.2, progress.3);
                last_progress = std::time::Instant::now();
            }
            let at = std::time::Instant::now();
            assert_eq!(
                client
                    .get(format!("{base}/readyz"))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
            max_read = max_read.max(at.elapsed());
            reads += 1;
            let at = std::time::Instant::now();
            tokio::time::timeout(Duration::from_secs(3), db::write(&pool, |tx| {
                Box::pin(async move {
                    sqlx::query("UPDATE admin_tokens SET label='Snapshot validation' WHERE id='snapshot-probe'")
                        .execute(&mut **tx).await?;
                    Ok::<_, sqlx::Error>(())
                })
            }))
            .await.unwrap().unwrap();
            max_write = max_write.max(at.elapsed());
            writes += 1;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    println!(
        "snapshot observation completed={} elapsed={:?} reads={reads} writes={writes} max_read={max_read:?} max_write={max_write:?}",
        completed.is_ok(),
        started.elapsed()
    );
    completed.expect("snapshot cleanup exceeded 180 seconds; inspect committed progress above");
    worker.await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "succeeded"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    for (table, counts) in before {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM {table} WHERE node_id=$1"
            )))
            .bind(&node)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0,
            "{table}"
        );
        let all_after: Vec<(String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT node_id,count(*) FROM {table} GROUP BY node_id ORDER BY node_id"
        )))
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(all_after, counts, "other nodes in {table}");
    }
    println!(
        "snapshot accepted={accepted:?}, completed={:?}, readiness_checks={reads}, writer_checks={writes}, max_read={max_read:?}",
        started.elapsed()
    );
    server.abort();
}
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
};

async fn fixture(rows: i64) -> AppState {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    sqlx::query("CREATE TABLE node_cleanup_writer_probe(id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY)")
        .execute(&pool).await.unwrap();
    sqlx::raw_sql("INSERT INTO users(id,name,created_at,updated_at) VALUES('user','User',now(),now());
        INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at)
        SELECT 'n'||i,'Node '||i,'127.0.0.1',22,'root','private_key','unused','example.test',443,':443','hash'||i,'unused','unused','{}',now(),now() FROM generate_series(1,2)i;")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at)
        SELECT n||i,n,'user','instance',0,0,10,20,'2026-01-01'::timestamptz+(i/100)*interval '1 second' FROM unnest(ARRAY['n1','n2'])n CROSS JOIN generate_series(1,$1::bigint)i")
        .bind(rows).execute(&pool).await.unwrap();
    AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    )
}

async fn write_probe(pool: &PgPool) -> Result<(), sqlx::Error> {
    db::write(pool, |tx| {
        Box::pin(async move {
            sqlx::query("INSERT INTO node_cleanup_writer_probe DEFAULT VALUES")
                .execute(&mut **tx)
                .await?;
            Ok(())
        })
    })
    .await
}

async fn request(state: &AppState) -> StatusCode {
    crate::api::nodes::remove_record(
        State(state.clone()),
        Path("n1".into()),
        Query(crate::api::nodes::RevisionQuery {
            expected_revision: Some(1),
        }),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn failed_removal_can_be_retried_as_a_linked_local_job() {
    let state = fixture(5000).await;
    assert_eq!(request(&state).await, StatusCode::ACCEPTED);
    let first = claim_next(&state.pool).await.unwrap().unwrap();
    {
        let cleanup = crate::node_cleanup::run_job(&state.pool, &first);
        tokio::pin!(cleanup);
        tokio::select! {
            result = &mut cleanup => panic!("cleanup completed before interruption: {result:?}"),
            _ = async {
                loop {
                    let count:i64=sqlx::query_scalar("SELECT coalesce((result_json#>>'{result,removed_history_rows}')::bigint,0) FROM jobs WHERE id=$1").bind(&first.id).fetch_one(&state.pool).await.unwrap();
                    if count>0 { break; }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            } => {}
        }
    }
    let checkpoint: Value = sqlx::query_scalar("SELECT result_json FROM jobs WHERE id=$1")
        .bind(&first.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    fail(
        &state.pool,
        &first,
        "injected cleanup failure",
        false,
        false,
        false,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM nodes WHERE id='n1'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "delete_failed"
    );
    assert_eq!(request(&state).await, StatusCode::ACCEPTED);
    let retry = claim_next(&state.pool).await.unwrap().unwrap();
    let resumed: Value = sqlx::query_scalar("SELECT result_json FROM jobs WHERE id=$1")
        .bind(&retry.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        resumed, checkpoint,
        "a manual retry must retain committed cleanup progress"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT retry_of_job_id FROM jobs WHERE id=$1")
            .bind(&retry.id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        first.id
    );
    execute_one(state.clone(), retry).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id='n1'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn remaining_large_rollups_are_also_removed_in_the_background() {
    let state = fixture(0).await;
    sqlx::query("INSERT INTO traffic_minute_totals(source,node_id,minute,tx,rx,samples,incomplete_samples) SELECT 'users','n1','2026-01-01'::timestamptz+i*interval '1 minute',10,20,1,0 FROM generate_series(1,2000)i")
        .execute(&state.pool).await.unwrap();
    assert_eq!(request(&state).await, StatusCode::ACCEPTED);
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    execute_one(state.clone(), job).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id='n1'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM traffic_minute_totals WHERE node_id='n1'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn retry_after_history_is_empty_keeps_the_durable_background_path() {
    let state = fixture(1200).await;
    assert_eq!(request(&state).await, StatusCode::ACCEPTED);
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    crate::node_cleanup::run_job(&state.pool, &job)
        .await
        .unwrap();
    fail(
        &state.pool,
        &job,
        "injected failure before metadata deletion",
        false,
        false,
        false,
        false,
    )
    .await
    .unwrap();
    assert_eq!(request(&state).await, StatusCode::ACCEPTED);
    let retry = claim_next(&state.pool).await.unwrap().unwrap();
    execute_one(state.clone(), retry).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id='n1'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn large_record_removal_is_queued_idempotent_and_completes_without_ssh() {
    let state = fixture(3000).await;
    // A separate fixture's holder reproduces the parallel-suite interference
    // without consuming any of the three application pool connections.
    let other = crate::db::test_pool().await;
    sqlx::raw_sql("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,sampled_at)
        SELECT n,'period',10,20,'2026-01-01'::timestamptz+(i/100)*interval '1 second' FROM unnest(ARRAY['n1','n2'])n CROSS JOIN generate_series(1,3000)i;
        INSERT INTO proxy_probe_samples(node_id,revision,status,sampled_at)
        SELECT n,1,'ok','2026-01-01'::timestamptz+i*interval '1 second' FROM unnest(ARRAY['n1','n2'])n CROSS JOIN generate_series(1,3000)i;
        INSERT INTO online_samples(cycle_id,node_id,sampled_at,cycle_started_at,status,traffic_status,connections,expected_nodes)
        SELECT md5(i::text)::uuid,n,'2026-01-01'::timestamptz+i*interval '1 second','2026-01-01'::timestamptz+i*interval '1 second','ok','ok','{\"user\":1}',2 FROM unnest(ARRAY['n1','n2'])n CROSS JOIN generate_series(1,3000)i;")
        .execute(&state.pool).await.unwrap();
    crate::analytics::refresh(&state.pool).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), request(&state))
            .await
            .unwrap(),
        StatusCode::ACCEPTED
    );
    assert_eq!(request(&state).await, StatusCode::ACCEPTED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind='record-remove'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    assert_eq!(job.kind, "record-remove");
    let worker_state = state.clone();
    let worker = tokio::spawn(async move { execute_one(worker_state, job).await });
    let legacy = db::hold_retired_global_write_lock(&other).await.unwrap();
    for _ in 0..10 {
        tokio::time::timeout(Duration::from_secs(2), write_probe(&state.pool))
            .await
            .expect("cleanup must yield to other writers")
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    legacy.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM node_cleanup_writer_probe")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        10
    );
    tokio::time::timeout(Duration::from_secs(30), worker)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id='n1'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    let status:(String,String,bool)=sqlx::query_as("SELECT status,stage,(result_json#>>'{result,remote_uninstall}')::bool FROM jobs WHERE kind='record-remove'").fetch_one(&state.pool).await.unwrap();
    assert_eq!(status, ("succeeded".into(), "record_removed".into(), false));
    for table in [
        "traffic_records",
        "node_network_samples",
        "proxy_probe_samples",
        "online_samples",
        "traffic_minute_totals",
        "traffic_hour_totals",
        "user_node_usage",
        "probe_minute_totals",
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                "SELECT count(*) FROM {table} WHERE node_id='n1'"
            )))
            .fetch_one(&state.pool)
            .await
            .unwrap(),
            0,
            "{table}"
        );
    }
    let surviving: (i64, i64, i64) = sqlx::query_as(
        "SELECT tx::bigint,rx::bigint,samples FROM user_node_usage WHERE node_id='n2'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(surviving, (30000, 60000, 3000));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM online_samples WHERE node_id='n2'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        3000
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM online_cycle_metrics WHERE scope_node_id='n1'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM online_minute_totals WHERE scope_node_id='n1'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id='user'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn interrupted_removal_resumes_committed_batches_after_recovery() {
    let state = fixture(20000).await;
    assert_eq!(request(&state).await, StatusCode::ACCEPTED);
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    let id = job.id.clone();
    let worker_state = state.clone();
    let worker = tokio::spawn(async move { execute_one(worker_state, job).await });
    tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            let removed:i64=sqlx::query_scalar("SELECT coalesce((result_json#>>'{result,removed_history_rows}')::bigint,0) FROM jobs WHERE id=$1").bind(&id).fetch_one(&state.pool).await.unwrap();
            if removed>0 { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    worker.abort();
    let _ = worker.await;
    let checkpoint: Value =
        sqlx::query_scalar("SELECT result_json#>'{result,cleanup_cursor}' FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(checkpoint["table"], "traffic_records");
    assert!(checkpoint["at"].is_string());
    assert!(checkpoint["id"].is_string());
    // Cancelling the Rust future starts an asynchronous PostgreSQL rollback.
    // Recovery intentionally skips a job whose last batch still holds its row
    // lock, so poll as the recurring recovery worker does until rollback ends.
    let job = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            recover_interrupted(&state.pool).await.unwrap();
            if let Some(job) = claim_next(&state.pool).await.unwrap() {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("committed cleanup must become recoverable after cancellation rollback");
    assert_eq!(job.id, id);
    assert_eq!(job.attempts, 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM job_events WHERE job_id=$1 AND event_type='job.recovered'",
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        1
    );
    tokio::time::timeout(Duration::from_secs(30), execute_one(state.clone(), job))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id=$1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "succeeded"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_records WHERE node_id='n2'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        20000
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id='n1'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn old_node_cleanup_seeks_its_own_index_despite_newer_other_node_history() {
    let state = fixture(1200).await;
    for start in (1..=60000_i64).step_by(1000) {
        let mut tx = state.pool.begin().await.unwrap();
        sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at)
            SELECT 'recent'||i,'n2','user','instance',0,0,10,20,'2026-10-01'::timestamptz+i*interval '1 second' FROM generate_series($1::bigint,$2::bigint)i")
            .bind(start).bind(start+999).execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
    }
    sqlx::query("ANALYZE traffic_records")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_incremental_sort=off")
        .execute(&mut *tx)
        .await
        .unwrap();
    let plan = sqlx::query_scalar::<_, String>("EXPLAIN (COSTS OFF) SELECT ctid::text,sampled_at,id::text FROM traffic_records WHERE node_id='n1' ORDER BY sampled_at DESC,id DESC LIMIT 32")
        .fetch_all(&mut *tx).await.unwrap().join("\n");
    assert!(plan.contains("traffic_records_node_cleanup_idx"), "{plan}");
    assert!(!plan.contains("Sort"), "{plan}");
    assert!(plan.contains("Index Cond: (node_id ="), "{plan}");
    tx.rollback().await.unwrap();
    assert_eq!(request(&state).await, StatusCode::ACCEPTED);
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    execute_one(state.clone(), job).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id='n1'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_records WHERE node_id='n2'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        61200
    );
}

#[tokio::test]
async fn retired_global_write_lock_does_not_block_cleanup_or_consume_an_attempt() {
    let state = fixture(1200).await;
    assert_eq!(request(&state).await, StatusCode::ACCEPTED);
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    let id = job.id.clone();
    let holder = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    let worker_state = state.clone();
    let worker = tokio::spawn(async move { execute_one(worker_state, job).await });
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT 1::bigint")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap();
    holder.rollback().await.unwrap();
    let (status, attempts): (String, i64) =
        sqlx::query_as("SELECT status,attempts FROM jobs WHERE id=$1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(status, "succeeded");
    assert_eq!(attempts, 1);
}

#[tokio::test]
async fn analytics_backlog_commits_progress_and_yields_before_it_is_fully_drained() {
    let state = fixture(0).await;
    sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,cycle_started_at,status,traffic_status,connections,expected_nodes)
        SELECT md5(i::text)::uuid,n,'2026-01-01'::timestamptz+i*interval '1 second','2026-01-01'::timestamptz+i*interval '1 second','ok','ok','{\"user\":1}',2 FROM unnest(ARRAY['n1','n2'])n CROSS JOIN generate_series(1,300)i")
        .execute(&state.pool).await.unwrap();
    let pool = state.pool.clone();
    let worker = tokio::spawn(async move { crate::analytics::refresh(&pool).await });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM online_cycle_metrics")
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert!(
                count < 900,
                "the whole backlog must not commit as one transaction"
            );
            if count > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), write_probe(&state.pool))
        .await
        .unwrap()
        .unwrap();
    worker.await.unwrap().unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM online_cycle_metrics")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        900
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM analytics_dirty_cycles")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    let before:String=sqlx::query_scalar("SELECT ctid::text FROM online_cycle_metrics WHERE cycle_id=md5('1')::uuid AND scope_node_id='n2'")
        .fetch_one(&state.pool).await.unwrap();
    sqlx::query("DELETE FROM online_samples WHERE cycle_id=md5('1')::uuid AND node_id='n1'")
        .execute(&state.pool)
        .await
        .unwrap();
    crate::analytics::refresh(&state.pool).await.unwrap();
    let after:String=sqlx::query_scalar("SELECT ctid::text FROM online_cycle_metrics WHERE cycle_id=md5('1')::uuid AND scope_node_id='n2'")
        .fetch_one(&state.pool).await.unwrap();
    assert_eq!(
        before, after,
        "unchanged nodes must not be rewritten or dirty their minute rollups"
    );
    let (covered,incomplete):(i64,bool)=sqlx::query_as("SELECT covered,incomplete FROM online_cycle_metrics WHERE cycle_id=md5('1')::uuid AND scope_node_id=''").fetch_one(&state.pool).await.unwrap();
    assert_eq!((covered, incomplete), (1, true));
}

#[tokio::test]
async fn final_fk_preparation_does_not_hold_the_global_write_lock() {
    let state = fixture(4000).await;
    sqlx::query("ANALYZE traffic_records")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(request(&state).await, StatusCode::ACCEPTED);
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    let output = crate::node_cleanup::run_job(&state.pool, &job)
        .await
        .unwrap();
    // Keep the pre-purge statistics and dead entries, then block the read
    // preparation to prove it cannot block unrelated application writers.
    let mut blocker = state.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE traffic_records IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await
        .unwrap();
    let pool = state.pool.clone();
    let preparation =
        tokio::spawn(async move { crate::node_cleanup::prepare_final_delete(&pool, "n1").await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!preparation.is_finished());
    tokio::time::timeout(Duration::from_millis(500), write_probe(&state.pool))
        .await
        .unwrap()
        .unwrap();
    blocker.rollback().await.unwrap();
    preparation.await.unwrap().unwrap();
    succeed(&state, &job, output).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id='n1'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_records WHERE node_id='n2'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        4000
    );
}

#[tokio::test]
async fn cancelled_cleanup_does_not_report_node_removal() {
    let state = fixture(8000).await;
    assert_eq!(request(&state).await, StatusCode::ACCEPTED);
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    let id = job.id.clone();
    let mut removed = state.removed_nodes.subscribe();
    let worker_state = state.clone();
    let worker = tokio::spawn(async move { execute_one(worker_state, job).await });
    tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            let count:i64=sqlx::query_scalar("SELECT coalesce((result_json#>>'{result,removed_history_rows}')::bigint,0) FROM jobs WHERE id=$1").bind(&id).fetch_one(&state.pool).await.unwrap();
            if count>0 { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    db::write(&state.pool, move |tx| {
        let id = id.clone();
        Box::pin(async move {
            // Match the worker's job-row-first order for cancellation.
            sqlx::query("SELECT id FROM jobs WHERE id=$1 FOR UPDATE")
                .bind(&id)
                .fetch_one(&mut **tx)
                .await?;
            sqlx::query("UPDATE jobs SET status='cancelled' WHERE id=$1")
                .bind(id)
                .execute(&mut **tx)
                .await?;
            Ok::<_, sqlx::Error>(())
        })
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE id='n1'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert!(matches!(
        removed.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}
