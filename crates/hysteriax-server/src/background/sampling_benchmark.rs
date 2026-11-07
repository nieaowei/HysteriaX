//! Explicit finite-cohort concurrency experiment. Never linked into production.
//! Uses the real claim, collection, supervision, completion and expiry functions;
//! cycle admission is frozen before timing so every limit gets the same 60s slot.
use super::*;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::task::JoinSet;

#[path = "sampling_benchmark_fixture.rs"]
mod fixture;

fn distribution(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return json!({"count":0});
    }
    let at = |q: f64| values[((values.len() as f64 * q).ceil() as usize).saturating_sub(1)];
    json!({"count":values.len(),"p50_ms":at(0.5),"p95_ms":at(0.95),"p99_ms":at(0.99),"max_ms":values.last()})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "explicit real-PG/SSH/Hysteria cohort; use tests/performance/benchmark-sampling-concurrency.py"]
async fn sampling_concurrency() {
    use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_filter(tracing_subscriber::filter::LevelFilter::INFO),
        )
        .try_init()
        .unwrap();
    let number = |key: &str, default: usize| {
        std::env::var(key)
            .ok()
            .map(|s| s.parse().unwrap())
            .unwrap_or(default)
    };
    let parallel = number("HX_SAMPLING_PARALLEL", 4);
    let active = number("HX_SAMPLING_ACTIVE_NODES", 6);
    let delay = number("HX_SAMPLING_DB_DELAY_MS", 25);
    assert!((1..=64).contains(&parallel) && (1..=10000).contains(&active) && delay <= 100);
    let database = url::Url::parse(&std::env::var("TEST_DATABASE_URL").unwrap()).unwrap();
    assert!(matches!(
        database.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    ));
    let direct = db::test_pool_with_max_connections(8).await;
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&direct)
        .await
        .unwrap();
    let remote = fixture::Remote::start(direct.clone(), active).await;
    remote.seed(&direct, active).await;
    let relay =
        crate::db::test_relay::SqlRelay::new(&direct, Duration::from_millis(delay as u64), 32)
            .await;
    let mut prewarmed = Vec::new();
    for _ in 0..32 {
        prewarmed.push(relay.pool.acquire().await.unwrap());
    }
    for mut connection in prewarmed {
        connection.return_to_pool().await;
    }
    let state = AppState::new(relay.pool.clone(), remote.secrets.clone());
    let heartbeat = tokio::spawn(execution::run_heartbeats(state.clone()));
    let cycles = ["traffic", "network", "proxy"];
    let mut ids = Vec::new();
    for kind in cycles {
        ids.push(ensure_cycle(&direct, kind).await.unwrap());
    }
    // Same cohort and fresh full slot for all queues, independent of wall-clock
    // minute phase. Collection/claim still perform every normal deadline check.
    sqlx::query("WITH clock AS MATERIALIZED (SELECT clock_timestamp() t) UPDATE sampling_cycles SET period_start=t,deadline_at=t+interval '60 seconds' FROM clock WHERE id=ANY($1::uuid[])")
        .bind(&ids).execute(&direct).await.unwrap();
    sqlx::query("UPDATE background_work w SET available_at=c.period_start,deadline_at=c.deadline_at FROM sampling_cycles c WHERE c.id=w.cycle_id AND c.id=ANY($1::uuid[])")
        .bind(&ids).execute(&direct).await.unwrap();
    let cohort_start: DateTime<Utc> = sqlx::query_scalar(
        "SELECT min(period_start) FROM sampling_cycles WHERE id=ANY($1::uuid[])",
    )
    .bind(&ids)
    .fetch_one(&direct)
    .await
    .unwrap();
    let metrics = Arc::new(Mutex::new(Vec::new()));
    let waits = Arc::new(Mutex::new(Vec::new()));
    let monitor = {
        let (pool, waits) = (state.pool.clone(), waits.clone());
        tokio::spawn(async move {
            loop {
                let start = Instant::now();
                match pool.acquire().await {
                    Ok(mut c) => {
                        waits
                            .lock()
                            .unwrap()
                            .push(start.elapsed().as_secs_f64() * 1000.0);
                        c.return_to_pool().await;
                    }
                    Err(e) => tracing::warn!(%e,"benchmark pool acquisition failed"),
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
    };
    let expiration = tokio::spawn(run_expiry(state.clone()));
    let start = Instant::now();
    let mut queues = JoinSet::new();
    for kind in cycles {
        let (state, metrics) = (state.clone(), metrics.clone());
        queues.spawn(async move {
            let mut tasks=JoinSet::new();
            let mut timer=tokio::time::interval(Duration::from_secs(1));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    Some(result)=tasks.join_next(),if !tasks.is_empty()=>{result.unwrap();},
                    _=timer.tick()=>{
                        while tasks.len()<parallel {
                            let began=Instant::now();
                            let work=match claim(&state.pool,kind,&state.instance_id).await {
                                Ok(Some(w))=>w,Ok(None)=>break,Err(e)=>{tracing::warn!(%e,"benchmark claim failed");break;}
                            };
                            let claim_ms=began.elapsed().as_secs_f64()*1000.0;
                            let (app,metrics)=(state.clone(),metrics.clone());
                            tasks.spawn(async move {
                                let work_id=work.id.clone();
                                let outcome=execution::supervise(app.pool.clone(),work.leases.clone(),async {
                                    let began=Instant::now();
                                    let collected=match kind {
                                        "traffic"=>crate::traffic::sample_work(&app,&work).await,
                                        "network"=>crate::node_limits::sample_work(&app,&work).await,
                                        "proxy"=>crate::proxy_monitor::sample_work(&app,&work).await,
                                        _=>unreachable!(),
                                    };
                                    let collection_ms=began.elapsed().as_secs_f64()*1000.0;
                                    let error=collected.as_ref().err().map(ToString::to_string);
                                    let began=Instant::now();
                                    let finished=finish(&app.pool,&work,collected.is_ok()).await;
                                    json!({"id":work.id,"kind":kind,"claim_ms":claim_ms,"collection_ms":collection_ms,
                                        "finish_ms":began.elapsed().as_secs_f64()*1000.0,"error":error,
                                        "finish_error":finished.err().map(|e|e.to_string())})
                                }).await;
                                let value=match outcome {
                                    execution::Supervised::Returned(v)=>v,
                                    other=>json!({"id":work_id,"kind":kind,"supervision_error":format!("{other:?}")}),
                                };
                                metrics.lock().unwrap().push(value);
                            });
                        }
                        if tasks.is_empty() {
                            let remaining:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_work WHERE kind=$1 AND state IN ('queued','running'))")
                                .bind(kind).fetch_one(&state.pool).await.unwrap();
                            if !remaining {break;}
                        }
                    }
                }
            }
        });
    }
    tokio::time::timeout(Duration::from_secs(190), async {
        while let Some(r) = queues.join_next().await {
            r.unwrap();
        }
    })
    .await
    .expect("bounded cohort did not settle");
    let elapsed = start.elapsed().as_secs_f64();
    monitor.abort();
    expiration.abort();
    heartbeat.abort();
    for task in [monitor, expiration, heartbeat] {
        let _ = task.await;
    }
    let tasks = metrics.lock().unwrap().clone();
    let states:Value=sqlx::query_scalar("SELECT coalesce(jsonb_object_agg(kind,states),'{}') FROM (SELECT kind,jsonb_object_agg(state,n) states FROM (SELECT kind,state,count(*) n FROM background_work WHERE cycle_id=ANY($1::uuid[]) GROUP BY kind,state) s GROUP BY kind) t")
        .bind(&ids).fetch_one(&direct).await.unwrap();
    let timings:Value=sqlx::query_scalar("SELECT jsonb_object_agg(kind,timing) FROM (SELECT kind,jsonb_build_object('started',count(started_at),'start_p95_ms',percentile_cont(0.95) WITHIN GROUP(ORDER BY extract(epoch FROM started_at-$2)*1000),'complete_p95_ms',percentile_cont(0.95) WITHIN GROUP(ORDER BY extract(epoch FROM finished_at-$2)*1000) FILTER(WHERE state='complete'),'finished_within_slot',count(*) FILTER(WHERE state='complete' AND finished_at<=deadline_at)) timing FROM background_work WHERE cycle_id=ANY($1::uuid[]) GROUP BY kind) s")
        .bind(&ids).bind(cohort_start).fetch_one(&direct).await.unwrap();
    let accounting:Value=sqlx::query_scalar("SELECT jsonb_build_object('captured',(SELECT count(*) FROM traffic_sample_batches WHERE payload_digest IS NOT NULL),'complete_batches',(SELECT count(*) FROM traffic_sample_batches WHERE state='complete'),'records',(SELECT count(*) FROM traffic_records),'bytes',(SELECT coalesce(sum(usage_bytes),0) FROM users),'record_bytes',(SELECT coalesce(sum(delta_tx+delta_rx),0) FROM traffic_records),'duplicate_records',(SELECT count(*) FROM (SELECT sample_id,user_id FROM traffic_records GROUP BY sample_id,user_id HAVING count(*)>1) d),'uncertain_gaps',(SELECT count(*) FROM data_gaps WHERE reason='traffic_clear_uncertain'),'probe_ok',(SELECT count(*) FROM proxy_probe_samples WHERE status='ok'),'probe_failed',(SELECT count(*) FROM proxy_probe_samples WHERE status<>'ok'),'network_ok',(SELECT count(*) FROM node_network_samples WHERE gap_reason IS NULL),'active_leases',(SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL))")
        .fetch_one(&direct).await.unwrap();
    assert_eq!(accounting["bytes"], accounting["record_bytes"]);
    assert_eq!(accounting["duplicate_records"], 0);
    let probe_reasons: Value = sqlx::query_scalar("SELECT coalesce(jsonb_object_agg(reason,n),'{}') FROM (SELECT coalesce(reason,'ok') reason,count(*) n FROM proxy_probe_samples GROUP BY reason) r")
        .fetch_one(&direct).await.unwrap();
    let clear_requests = remote.clears.lock().unwrap().clone();
    assert!(
        clear_requests.values().all(|n| *n == 1),
        "destructive read repeated within one cohort"
    );
    let phases:Value=cycles.into_iter().map(|kind| {
        let items:Vec<_>=tasks.iter().filter(|v|v["kind"]==kind).collect();
        (kind.into(),json!({"claim":distribution(items.iter().filter_map(|v|v["claim_ms"].as_f64()).collect()),
            "collection":distribution(items.iter().filter_map(|v|v["collection_ms"].as_f64()).collect()),
            "finish":distribution(items.iter().filter_map(|v|v["finish_ms"].as_f64()).collect())}))
    }).collect::<serde_json::Map<_,_>>().into();
    println!(
        "SAMPLING_CONCURRENCY {}",
        json!({"schema":schema,"parallel_per_queue":parallel,"active_nodes":active,
        "users":100000,"nodes":10000,"assignments":1000000,"slot_seconds":60,"db_pool":32,"db_response_delay_ms":delay,
        "elapsed_seconds":elapsed,"states":states,"timings":timings,"phases":phases,"accounting":accounting,
        "pool_acquire":distribution(waits.lock().unwrap().clone()),"probe_reasons":probe_reasons,"wire_commands":relay.commands.load(std::sync::atomic::Ordering::SeqCst),
        "ssh_connections":remote.connections.load(std::sync::atomic::Ordering::SeqCst),"clear_requests":clear_requests,
        "scope":"local finite cohort, real SSH and Hysteria proxy; synthetic user/network counters; not production capacity acceptance",
        "tasks":tasks})
    );
    remote.stop().await;
    relay.pool.close().await;
    direct.close().await;
}
