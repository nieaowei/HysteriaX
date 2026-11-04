//! Manual worker-integration acceptance. Every measured transaction uses real
//! triggers/leases; synthetic import (outside measurement) bypasses triggers and
//! validates its graph explicitly. No SSH/HTTP endpoint is contacted.
use super::*;
use crate::db::test_relay::SqlRelay;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};

async fn scalar(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn seed(pool: &PgPool, users: i64, nodes: i64) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL session_replication_role=replica")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('bench-ssh','SSH','ssh_password',now(),now()); INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('bench-ssh',1,'fixture',now()); INSERT INTO authorization_groups(id,name,created_at,updated_at) VALUES('bench','Benchmark',now(),now())")
        .execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) SELECT 'u-'||lpad(i::text,8,'0'),'User '||i,now(),now() FROM generate_series(1,$1::bigint)i")
        .bind(users).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at) SELECT 'connection:'||id,id,'connection',id,'fixture',now() FROM users")
        .execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,ssh_credential_id,ssh_credential_version,created_at,updated_at) SELECT 'n-'||lpad(i::text,8,'0'),'Node '||i,'127.0.0.1',1,'fixture','node.invalid',443,':443','node-'||i,'fixture','fixture','fixture','bench-ssh',1,now(),now() FROM generate_series(1,$1::bigint)i")
        .bind(nodes).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql("INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT 'bench',id,now() FROM users; INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT 'bench',id,now() FROM nodes")
        .execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,created_at) SELECT 'u-'||lpad(i::text,8,'0'),'n-'||lpad((1+((i-1)*10+k)%$2)::text,8,'0'),now() FROM generate_series(1,$1::bigint)i CROSS JOIN generate_series(0,9)k")
        .bind(users).bind(nodes).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO node_packages(node_id,period_id,config,next_reset_at,usage_bytes) SELECT id,id,'{\"cycle\":\"monthly\"}',clock_timestamp()-interval '1 second',1 FROM nodes")
        .execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(scalar(pool, "SELECT count(*) FROM users").await, users);
    assert_eq!(scalar(pool, "SELECT count(*) FROM nodes").await, nodes);
    assert_eq!(
        scalar(pool, "SELECT count(*) FROM node_assignments").await,
        users * 10
    );
    assert_eq!(scalar(pool,"SELECT count(*) FROM node_assignments a LEFT JOIN users u ON u.id=a.user_id LEFT JOIN nodes n ON n.id=a.node_id WHERE u.id IS NULL OR n.id IS NULL").await,0);
    assert_eq!(scalar(pool,"SELECT count(*) FROM nodes n LEFT JOIN credential_versions v ON v.credential_id=n.ssh_credential_id AND v.version=n.ssh_credential_version WHERE v.credential_id IS NULL").await,0);
    sqlx::raw_sql("ANALYZE users; ANALYZE nodes; ANALYZE node_packages; ANALYZE node_assignments; ANALYZE authorization_group_users; ANALYZE authorization_group_nodes")
        .execute(pool).await.unwrap();
}

fn distribution(mut values: Vec<f64>) -> Value {
    if values.is_empty() {
        return json!({"count":0,"p95_ms":0.0,"max_ms":0.0});
    }
    values.sort_by(f64::total_cmp);
    json!({"count":values.len(),"p50_ms":values[values.len()/2],
        "p95_ms":values[((values.len() as f64*0.95).ceil() as usize).saturating_sub(1)],
        "max_ms":values.last().unwrap()})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "manual 100k-user/10k-node lock contention acceptance; use scripts/verify-lock-contention.py"]
async fn benchmark_lock_contention() {
    let users: i64 = std::env::var("HYSTERIAX_LOCK_BENCH_USERS")
        .unwrap_or("100000".into())
        .parse()
        .unwrap();
    let nodes: i64 = std::env::var("HYSTERIAX_LOCK_BENCH_NODES")
        .unwrap_or("10000".into())
        .parse()
        .unwrap();
    let seconds: u64 = std::env::var("HYSTERIAX_LOCK_BENCH_SECONDS")
        .unwrap_or("900".into())
        .parse()
        .unwrap();
    let instances: usize = std::env::var("HYSTERIAX_LOCK_BENCH_INSTANCES")
        .unwrap_or("1".into())
        .parse()
        .unwrap();
    let delay: u64 = std::env::var("HYSTERIAX_LOCK_BENCH_DELAY_MS")
        .unwrap_or("25".into())
        .parse()
        .unwrap();
    assert!(matches!(instances, 1 | 3) && nodes >= 10 && users >= nodes);
    let direct = db::test_pool_with_max_connections(24).await;
    seed(&direct, users, nodes).await;
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&direct)
        .await
        .unwrap();
    let scheduled:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='node_packages' AND column_name='next_evaluation_at')")
        .fetch_one(&direct).await.unwrap();
    let mut relays = Vec::new();
    for _ in 0..instances {
        relays.push(SqlRelay::new(&direct, Duration::from_millis(delay), 32).await);
    }
    let mut ping_ms = Vec::new();
    for relay in &relays {
        let mut connection = relay.pool.acquire().await.unwrap();
        for attempt in 0..12 {
            let began = Instant::now();
            sqlx::query("SELECT 1::bigint")
                .execute(&mut *connection)
                .await
                .unwrap();
            if attempt > 0 {
                ping_ms.push(began.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let errors = Arc::new(AtomicUsize::new(0));
    let writes = Arc::new(AtomicUsize::new(0));
    let mut workers = tokio::task::JoinSet::new();
    let started = Instant::now();
    for (index, relay) in relays.iter().enumerate() {
        {
            let pool = relay.pool.clone();
            let stop = stop.clone();
            let errors = errors.clone();
            workers.spawn(async move {
                while !stop.load(Ordering::SeqCst) {
                    match maintenance::pass(&pool, &format!("bench-{index}")).await {
                        Ok(value) => {
                            let count = serde_json::to_value(value).unwrap().as_u64().unwrap_or(0);
                            if count == 0 {
                                tokio::time::sleep(Duration::from_secs(1)).await;
                            }
                        }
                        Err(error) => {
                            if errors.fetch_add(1, Ordering::SeqCst) < 10 {
                                eprintln!("BENCH_ERROR {error}");
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            });
        }
        {
            let pool = relay.pool.clone();
            let stop = stop.clone();
            let errors = errors.clone();
            workers.spawn(async move {
                while !stop.load(Ordering::SeqCst) {
                    if let Err(error) = crate::analytics::refresh(&pool).await
                        && errors.fetch_add(1, Ordering::SeqCst) < 10
                    {
                        eprintln!("BENCH_ERROR analytics {error}");
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            });
        }
        let secrets =
            crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .unwrap();
        let app = crate::state::AppState::new(relay.pool.clone(), secrets);
        for _ in 0..4 {
            let app = app.clone();
            let stop = stop.clone();
            let errors = errors.clone();
            workers.spawn(async move {
                while !stop.load(Ordering::SeqCst) {
                    match crate::background::claim(&app.pool, "traffic", &app.instance_id).await {
                        Ok(Some(work)) => {
                            crate::execution::supervise(
                                app.pool.clone(),
                                work.leases.clone(),
                                async {
                                    let result = crate::traffic::sample_work(&app, &work).await;
                                    if let Err(error) = &result
                                        && errors.fetch_add(1, Ordering::SeqCst) < 10
                                    {
                                        eprintln!("BENCH_ERROR billing {error}");
                                    }
                                    if let Err(error) =
                                        crate::background::finish(&app.pool, &work, result.is_ok())
                                            .await
                                        && errors.fetch_add(1, Ordering::SeqCst) < 10
                                    {
                                        eprintln!("BENCH_ERROR finish {error}");
                                    }
                                },
                            )
                            .await;
                        }
                        Ok(None) => tokio::time::sleep(Duration::from_millis(100)).await,
                        Err(error) => {
                            if errors.fetch_add(1, Ordering::SeqCst) < 10 {
                                eprintln!("BENCH_ERROR {error}");
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            });
        }
    }
    let producing = Arc::new(AtomicBool::new(true));
    let cycle = Uuid::new_v4();
    sqlx::query("INSERT INTO sampling_cycles(id,kind,period_start,deadline_at,expected_nodes) VALUES($1,'lock-benchmark',clock_timestamp(),clock_timestamp()+interval '1 day',$2)")
        .bind(cycle).bind(nodes).execute(&direct).await.unwrap();
    let mut producers = tokio::task::JoinSet::new();
    for index in 0..16 {
        let pool = relays[index % instances].pool.clone();
        let producing = producing.clone();
        let writes = writes.clone();
        let errors = errors.clone();
        producers.spawn(async move {
            let mut sequence=index as i64;
            let mut latencies=Vec::new();
            while producing.load(Ordering::SeqCst) {
                let node=format!("n-{:08}",1+sequence%nodes);sequence+=16;
                let user=format!("u-{:08}",1+sequence%users);
                let at=Instant::now();
                let result=db::write(&pool,move |tx| {
                    let (node,user)=(node.clone(),user.clone());
                    Box::pin(async move {
                        sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE /* lock-contention-probe */")
                            .bind(&node).execute(&mut **tx).await?;
                        sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,cycle_started_at,status,traffic_status,connections,expected_nodes) VALUES($1,$2,clock_timestamp(),clock_timestamp(),'ok','ok',jsonb_build_object($3::text,1),$4) ON CONFLICT(cycle_id,node_id) DO UPDATE SET sampled_at=excluded.sampled_at,connections=excluded.connections")
                            .bind(cycle).bind(&node).bind(&user).bind(nodes).execute(&mut **tx).await?;
                        Ok::<_,sqlx::Error>(())
                    })
                }).await;
                if result.is_ok(){writes.fetch_add(1,Ordering::SeqCst);latencies.push(at.elapsed().as_secs_f64()*1000.0);}
                else if let Err(error)=result && errors.fetch_add(1,Ordering::SeqCst)<10 {eprintln!("BENCH_ERROR source {error}");}
            }
            latencies
        });
    }
    let mut node_waits = std::collections::HashMap::<String, f64>::new();
    let mut reset_finished = None;
    let mut injected = 0_i64;
    let mut next_injection = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        let waits:Vec<(String,f64)>=sqlx::query_as("SELECT l.pid::text||':'||l.waitstart::text,extract(epoch FROM clock_timestamp()-l.waitstart)::double precision*1000 FROM pg_locks l JOIN pg_stat_activity a ON a.pid=l.pid WHERE NOT l.granted AND l.waitstart IS NOT NULL AND a.datname=current_database() AND a.query LIKE '%/* lock-contention-probe */%' AND a.pid<>pg_backend_pid()")
            .fetch_all(&direct).await.unwrap();
        for (key, ms) in waits {
            node_waits
                .entry(key)
                .and_modify(|v| *v = v.max(ms))
                .or_insert(ms);
        }
        if reset_finished.is_none()
            && scalar(
                &direct,
                "SELECT count(*) FROM node_packages WHERE usage_bytes<>0",
            )
            .await
                == 0
        {
            reset_finished = Some(started.elapsed().as_secs_f64());
        }
        if Instant::now() >= next_injection {
            injected += 1;
            let node = format!("n-{:08}", 1 + (injected - 1) * 10 % nodes);
            let user = format!("u-{:08}", 1 + (injected - 1) % (nodes / 10));
            // Find one actually assigned user; the import creates exactly ten
            // consecutive node links for each user.
            let user: String = sqlx::query_scalar(
                "SELECT user_id FROM node_assignments WHERE node_id=$1 ORDER BY user_id LIMIT 1",
            )
            .bind(&node)
            .fetch_optional(&direct)
            .await
            .unwrap()
            .unwrap_or(user);
            let sample = format!("bench:{injected}");
            let billing_cycle = Uuid::new_v4();
            let mut tx = direct.begin().await.unwrap();
            sqlx::query("SET LOCAL lock_timeout='30s'")
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
                .bind(&node)
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("INSERT INTO sampling_cycles(id,kind,period_start,deadline_at,expected_nodes) VALUES($1,$2,clock_timestamp(),clock_timestamp()+interval '1 day',1)")
                .bind(billing_cycle).bind(format!("lock-billing:{billing_cycle}")).execute(&mut *tx).await.unwrap();
            sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,cycle_started_at,status,traffic_status,connections,expected_nodes,pending) VALUES($1,$2,clock_timestamp(),clock_timestamp(),'ok','failed',jsonb_build_object($3::text,1),1,true)")
                .bind(billing_cycle).bind(&node).bind(&user).execute(&mut *tx).await.unwrap();
            sqlx::query("INSERT INTO traffic_sample_batches(sample_id,node_id,cycle_id,state,payload_digest,instance_id,item_count,clear_instance_verified) VALUES($1,$2,$3,'ready','fixture','fixture',1,true)")
                .bind(&sample).bind(&node).bind(billing_cycle).execute(&mut *tx).await.unwrap();
            sqlx::query("INSERT INTO traffic_sample_items(sample_id,node_id,user_id,tx_total,rx_total,sampled_at) VALUES($1,$2,$3,1,0,clock_timestamp())")
                .bind(&sample).bind(&node).bind(&user).execute(&mut *tx).await.unwrap();
            sqlx::query("INSERT INTO background_work(id,kind,cycle_id,node_id,deadline_at) VALUES($1,'traffic',$2,$3,clock_timestamp()+interval '1 day')")
                .bind(&sample).bind(billing_cycle).bind(&node).execute(&mut *tx).await.unwrap();
            tx.commit().await.unwrap();
            next_injection = Instant::now() + Duration::from_secs(1);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    producing.store(false, Ordering::SeqCst);
    let mut latencies = Vec::new();
    while let Some(result) = producers.join_next().await {
        latencies.extend(result.unwrap());
    }
    let drain_started = Instant::now();
    let pending_sql = "SELECT (SELECT count(*) FROM analytics_dirty_cycles)+(SELECT count(*) FROM traffic_dirty_hours)+(SELECT count(*) FROM online_dirty_minutes)+(SELECT count(*) FROM job_retry_state_dirty)+(SELECT count(*) FROM traffic_sample_batches WHERE state IN ('capturing','ready'))+(SELECT count(*) FROM node_packages WHERE usage_bytes<>0)";
    while drain_started.elapsed() < Duration::from_secs(60)
        && scalar(&direct, pending_sql).await > 0
    {
        if reset_finished.is_none()
            && scalar(
                &direct,
                "SELECT count(*) FROM node_packages WHERE usage_bytes<>0",
            )
            .await
                == 0
        {
            reset_finished = Some(started.elapsed().as_secs_f64());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if reset_finished.is_none()
        && scalar(
            &direct,
            "SELECT count(*) FROM node_packages WHERE usage_bytes<>0",
        )
        .await
            == 0
    {
        reset_finished = Some(started.elapsed().as_secs_f64());
    }
    let pending = scalar(&direct, pending_sql).await;
    stop.store(true, Ordering::SeqCst);
    while let Some(result) = workers.join_next().await {
        result.unwrap();
    }
    let ledger = scalar(&direct, "SELECT count(*) FROM traffic_records").await;
    let usage = scalar(&direct, "SELECT sum(usage_bytes)::bigint FROM users").await;
    let unbilled = scalar(
        &direct,
        "SELECT count(*) FROM traffic_sample_items WHERE processed_at IS NULL",
    )
    .await;
    let raw:(i64,i64)=sqlx::query_as("SELECT count(*)::bigint,(SELECT count(DISTINCT key)::bigint FROM online_samples s CROSS JOIN LATERAL jsonb_each(s.connections) e WHERE s.cycle_id=$1) FROM online_samples WHERE cycle_id=$1")
        .bind(cycle).fetch_one(&direct).await.unwrap();
    let cached:Option<(i64,i64)>=sqlx::query_as("SELECT connections::bigint,users FROM online_cycle_metrics WHERE cycle_id=$1 AND scope_node_id=''")
        .bind(cycle).fetch_optional(&direct).await.unwrap();
    let rollup = scalar(
        &direct,
        "SELECT coalesce(sum(tx),0)::bigint FROM traffic_hour_totals WHERE source='users'",
    )
    .await;
    let report = json!({"schema":db::MIGRATOR.iter().last().unwrap().version,"users":users,"nodes":nodes,"assignments":users*10,
        "instances":instances,"database_response_delay_ms":delay,"select1_latency":distribution(ping_ms),"measured_seconds":seconds,"actual_seconds":started.elapsed().as_secs_f64(),
        "profile":"real package/analytics/billing workers; 16 source writers; one durable ready page per second; no SSH/HTTP",
        "seed_triggers_disabled_only_during_import":true,"measured_triggers_enabled":true,"scheduled_evaluation":scheduled,
        "first_due_burst_seconds":reset_finished,"source_writes":writes.load(Ordering::SeqCst),"source_transaction_latency":distribution(latencies),
        "observed_node_lock_waits":distribution(node_waits.into_values().collect()),"errors":errors.load(Ordering::SeqCst),
        "ready_batches_injected":injected,"ledger_rows":ledger,"usage_bytes":usage,"unbilled_items":unbilled,
        "raw_online":raw,"cached_online":cached,"traffic_hour_tx":rollup,
        "drain_seconds":drain_started.elapsed().as_secs_f64(),"pending_after_drain":pending,
        "database_commands":relays.iter().map(|r|r.commands.load(Ordering::SeqCst)).sum::<usize>()});
    if let Ok(path) = std::env::var("HYSTERIAX_LOCK_BENCH_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    println!("LOCK_CONTENTION_REPORT {report}");
    for relay in &relays {
        relay.pool.close().await;
    }
    drop(relays);
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&direct)
        .await
        .unwrap();
    direct.close().await;
    assert_eq!(ledger, injected, "every injected ready page must bill once");
    assert_eq!(usage, injected, "usage must match the immutable ledger");
    assert_eq!(unbilled, 0);
    assert_eq!(
        pending, 0,
        "derived and ready queues must drain within sixty seconds"
    );
    assert_eq!(
        cached,
        Some(raw),
        "derived online values must equal the final raw source"
    );
    assert_eq!(
        rollup, injected,
        "hourly totals must equal the immutable ledger"
    );
    if scheduled {
        assert_eq!(
            errors.load(Ordering::SeqCst),
            0,
            "no source or worker errors are acceptable"
        );
        assert!(
            reset_finished.is_some_and(|s| s <= 60.0),
            "ten thousand initially due packages must evaluate within sixty seconds"
        );
    }
}
