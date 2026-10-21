use super::*;
use chrono::{DateTime, Duration, Utc};
use sqlx::Row;

#[tokio::test]
async fn hourly_rollups_match_raw_before_after_refresh_and_corrections() {
    let pool = super::tests::seed().await;
    let anchor: DateTime<Utc> = "2026-10-14T12:37:00.123456Z".parse().unwrap();
    sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,gap_reason,sampled_at) SELECT 'roll-'||i,CASE WHEN i%2=0 THEN 'n1' ELSE 'n2' END,'u','instance',0,0,i,i*2,CASE WHEN i%23=0 THEN 'gap' END,$1::timestamptz-i*interval '37 seconds' FROM generate_series(1,1800)i").bind(anchor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,gap_reason,sampled_at) SELECT node_id,'period',delta_tx,delta_rx,gap_reason,sampled_at FROM traffic_records").execute(&pool).await.unwrap();
    type Reading = (DateTime<Utc>, i64, i64, i64, bool);
    for phase in 0..4 {
        if phase == 1 || phase == 3 {
            refresh(&pool).await.unwrap();
        }
        if phase == 2 {
            sqlx::query("UPDATE traffic_records SET delta_tx=9999,sampled_at=sampled_at-interval '1 hour' WHERE id='roll-80'").execute(&pool).await.unwrap();
            sqlx::query("DELETE FROM traffic_records WHERE id IN ('roll-1','roll-2')")
                .execute(&pool)
                .await
                .unwrap();
        }
        for (source, table) in [
            ("users", "traffic_records"),
            ("network", "node_network_samples"),
        ] {
            for (days, unit) in [(1, "hour"), (7, "rolling_day"), (30, "rolling_day")] {
                for scope in [None, Some("n1")] {
                    let start = anchor - Duration::days(days);
                    let actual: Vec<Reading> = sqlx::query_as(
                        &include_str!("traffic_history.sql").replace("RAW_TABLE", table),
                    )
                    .bind(unit)
                    .bind(source)
                    .bind(start)
                    .bind(anchor)
                    .bind(scope)
                    .fetch_all(&pool)
                    .await
                    .unwrap();
                    let expected:Vec<Reading>=sqlx::query_as(&format!("SELECT date_bin($1::interval,sampled_at,$2) bucket,sum(delta_tx)::bigint,sum(delta_rx)::bigint,count(DISTINCT node_id)::bigint,bool_or(gap_reason IS NOT NULL OR baseline_only) FROM {table} WHERE sampled_at>=$2 AND sampled_at<$3 AND ($4::text IS NULL OR node_id=$4) GROUP BY bucket ORDER BY bucket"))
                        .bind(if days==1 {"1 hour"} else {"24 hours"}).bind(start).bind(anchor).bind(scope).fetch_all(&pool).await.unwrap();
                    assert_eq!(
                        actual, expected,
                        "phase {phase}, {source}, {days}, {scope:?}"
                    );
                }
            }
        }
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM traffic_hour_totals")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(count > 0);
    sqlx::query("DELETE FROM nodes WHERE id='n1'")
        .execute(&pool)
        .await
        .unwrap();
    refresh(&pool).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_hour_totals WHERE node_id='n1'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn online_minute_rollups_preserve_weighted_averages_unknowns_and_dirty_fallback() {
    let pool = super::tests::seed().await;
    let anchor: DateTime<Utc> = "2026-10-14T12:37:17Z".parse().unwrap();
    for i in 0..30 {
        let cycle = uuid::Uuid::new_v4();
        let at = anchor - Duration::seconds(13 * i);
        sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,cycle_started_at,status,traffic_status,connections,expected_nodes) VALUES($1,'n1',$2,$2,$3,'ok',$4,1)")
            .bind(cycle).bind(at).bind(if i%7==0 {"failed"} else {"ok"}).bind(if i%3==0 {serde_json::json!({})} else {serde_json::json!({"u":i+1})}).execute(&pool).await.unwrap();
    }
    refresh(&pool).await.unwrap();
    for phase in 0..3 {
        if phase == 1 {
            sqlx::query("UPDATE online_cycle_metrics SET users=users+2,connections=connections+3 WHERE users IS NOT NULL").execute(&pool).await.unwrap();
        }
        if phase == 2 {
            refresh(&pool).await.unwrap();
        }
        for end in [anchor, anchor + Duration::seconds(43)] {
            for (unit, days) in [("hour", 1), ("rolling_day", 7), ("rolling_day", 30)] {
                let start = end - Duration::days(days);
                for scope in [None, Some("n1"), Some("n2")] {
                    let actual = sqlx::query(include_str!("online_history.sql"))
                        .bind(unit)
                        .bind("UTC")
                        .bind(start)
                        .bind(end)
                        .bind(scope)
                        .fetch_all(&pool)
                        .await
                        .unwrap();
                    let raw = sqlx::query("SELECT date_bin(CASE WHEN $1='hour' THEN interval '1 hour' ELSE interval '24 hours' END,started_at,$2) bucket,avg(users)::double precision users_avg,max(users)::double precision users_peak,avg(connections)::double precision connections_avg,max(connections)::double precision connections_peak,min(covered)::bigint covered_nodes,bool_or(incomplete) incomplete,min(traffic_covered)::bigint covered,max(expected)::bigint expected,bool_or(traffic_incomplete) traffic_incomplete FROM online_cycle_metrics WHERE scope_node_id=coalesce($4,'') AND started_at>=$2 AND started_at<$3 GROUP BY bucket ORDER BY bucket")
                        .bind(unit).bind(start).bind(end).bind(scope).fetch_all(&pool).await.unwrap();
                    assert_eq!(actual.len(), raw.len());
                    for (a, b) in actual.iter().zip(raw.iter()) {
                        assert_eq!(
                            a.get::<DateTime<Utc>, _>("bucket"),
                            b.get::<DateTime<Utc>, _>("bucket")
                        );
                        for field in [
                            "users_avg",
                            "users_peak",
                            "connections_avg",
                            "connections_peak",
                        ] {
                            assert_eq!(
                                a.get::<Option<f64>, _>(field),
                                b.get::<Option<f64>, _>(field),
                                "{field}"
                            );
                        }
                        for field in ["covered_nodes", "covered", "expected"] {
                            assert_eq!(a.get::<i64, _>(field), b.get::<i64, _>(field), "{field}");
                        }
                        for field in ["incomplete", "traffic_incomplete"] {
                            assert_eq!(a.get::<bool, _>(field), b.get::<bool, _>(field), "{field}");
                        }
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn network_snapshot_is_read_only_and_does_not_drain_cycle_queue() {
    let pool = super::tests::seed().await;
    sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,cycle_started_at,status,expected_nodes) VALUES($1,'n1',now(),now()-interval '1 hour','failed',1)").bind(uuid::Uuid::new_v4()).execute(&pool).await.unwrap();
    let mut tx = snapshot(&pool, Utc::now(), false).await.unwrap();
    let readonly: String = sqlx::query_scalar("SHOW transaction_read_only")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(readonly, "on");
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM analytics_dirty_cycles")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let tx = snapshot(&pool, Utc::now(), true).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM analytics_dirty_cycles")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1,
        "online snapshots use the source overlay without draining another worker's queue"
    );
}

#[tokio::test]
async fn task_counters_and_retry_state_match_raw_after_status_and_chain_changes() {
    let pool = super::tests::seed().await;
    sqlx::query("INSERT INTO jobs(id,node_id,kind,status,stage,payload_json,available_at,created_at,updated_at,finished_at) VALUES('root','n1','kick','failed','failed','{\"node_limit\":true}',now(),now(),now(),now())").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,node_id,kind,status,stage,retry_of_job_id,available_at,created_at,updated_at) VALUES('retry','n1','kick','queued','queued','root',now(),now(),now())").execute(&pool).await.unwrap();
    for status in ["queued", "cancelled", "failed", "succeeded"] {
        sqlx::query(
            "UPDATE jobs SET status=$1,finished_at=now(),error_message='latest' WHERE id='retry'",
        )
        .bind(status)
        .execute(&pool)
        .await
        .unwrap();
        let raw: Vec<(String, i64)> =
            sqlx::query_as("SELECT status,count(*) FROM jobs GROUP BY status ORDER BY status")
                .fetch_all(&pool)
                .await
                .unwrap();
        let totals: Vec<(String, i64)> =
            sqlx::query_as("SELECT status,total FROM job_status_totals ORDER BY status")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(raw, totals);
        let a:(i64,i64,i64)=sqlx::query_as("SELECT pending,pending_limit,failed_limit FROM job_node_kick_totals WHERE node_id='n1'").fetch_one(&pool).await.unwrap();
        let b:(i64,i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE status IN ('queued','running')),count(*) FILTER(WHERE status IN ('queued','running') AND payload_json->>'node_limit'='true'),count(*) FILTER(WHERE status='failed' AND payload_json->>'node_limit'='true') FROM jobs WHERE node_id='n1' AND kind='kick'").fetch_one(&pool).await.unwrap();
        assert_eq!(a, b);
        let cutoff = Utc::now() - Duration::hours(24);
        let dirty = sqlx::query(include_str!("../overview/summary_failures.sql"))
            .bind(cutoff)
            .fetch_all(&pool)
            .await
            .unwrap();
        refresh(&pool).await.unwrap();
        let clean = sqlx::query(include_str!("../overview/summary_failures.sql"))
            .bind(cutoff)
            .fetch_all(&pool)
            .await
            .unwrap();
        let value = |r: &sqlx::postgres::PgRow| {
            (
                r.get::<String, _>("id"),
                r.get::<String, _>("status"),
                r.get::<Option<String>, _>("error_message"),
                r.get::<Option<DateTime<Utc>>, _>("finished_at"),
            )
        };
        assert_eq!(
            dirty.iter().map(value).collect::<Vec<_>>(),
            clean.iter().map(value).collect::<Vec<_>>()
        );
    }
    sqlx::query("DELETE FROM jobs WHERE id='retry'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM nodes WHERE id='n1'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT failed_limit FROM job_node_kick_totals WHERE node_id='n1'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT sum(total)::bigint FROM job_status_totals")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn aligned_online_history_skips_cycles_until_an_edge_or_dirty_minute_needs_them() {
    let pool = super::tests::seed().await;
    let anchor: DateTime<Utc> = "2026-10-15T12:00:00Z".parse().unwrap();
    sqlx::query("INSERT INTO online_cycle_metrics(cycle_id,scope_node_id,started_at,users,connections,covered,expected,incomplete,traffic_covered,traffic_incomplete) SELECT gen_random_uuid(),scope,$1::timestamptz-i*interval '10 seconds',1,2,1,1,false,1,false FROM generate_series(1,2400) i CROSS JOIN unnest(ARRAY['','n1']) scope")
        .bind(anchor).execute(&pool).await.unwrap();
    refresh(&pool).await.unwrap();
    sqlx::raw_sql(
        "ANALYZE online_cycle_metrics; ANALYZE online_minute_totals; ANALYZE online_dirty_minutes",
    )
    .execute(&pool)
    .await
    .unwrap();

    fn cycle_scan_loops(plan: &serde_json::Value) -> u64 {
        let own = if plan["Relation Name"] == "online_cycle_metrics" {
            plan["Actual Loops"].as_u64().unwrap_or(0)
        } else {
            0
        };
        own + plan["Plans"]
            .as_array()
            .map(|children| children.iter().map(cycle_scan_loops).sum::<u64>())
            .unwrap_or(0)
    }
    for phase in 0..3 {
        if phase == 2 {
            sqlx::query("UPDATE online_cycle_metrics SET connections=connections+3 WHERE started_at=$1::timestamptz-interval '10 seconds'")
                .bind(anchor).execute(&pool).await.unwrap();
        }
        let end = anchor
            + if phase == 1 {
                Duration::seconds(17)
            } else {
                Duration::zero()
            };
        for scope in [None, Some("n1")] {
            let mut tx = pool.begin().await.unwrap();
            sqlx::raw_sql("SET LOCAL jit=off; SET LOCAL plan_cache_mode=force_custom_plan")
                .execute(&mut *tx)
                .await
                .unwrap();
            let explain = format!(
                "EXPLAIN (ANALYZE, FORMAT JSON) {}",
                include_str!("online_history.sql")
            );
            let row = sqlx::query(&explain)
                .bind("hour")
                .bind("UTC")
                .bind(end - Duration::days(1))
                .bind(end)
                .bind(scope)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            let plan: serde_json::Value = row.get("QUERY PLAN");
            assert_eq!(
                cycle_scan_loops(&plan[0]["Plan"]) > 0,
                phase != 0,
                "scope={scope:?}, phase={phase}"
            );
            tx.rollback().await.unwrap();
        }
    }
}

#[tokio::test]
async fn rollup_refresh_reads_only_dirty_ranges_and_skips_empty_queues() {
    let pool = super::tests::seed().await;
    let anchor: DateTime<Utc> = "2026-10-15T12:00:00Z".parse().unwrap();
    sqlx::query("INSERT INTO traffic_minute_totals(source,node_id,minute,tx,rx,samples,incomplete_samples) SELECT 'users','n1',$1::timestamptz-i*interval '1 minute',1,2,1,0 FROM generate_series(1,20000)i")
        .bind(anchor).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO online_cycle_metrics(cycle_id,scope_node_id,started_at,users,connections,covered,expected,incomplete,traffic_covered,traffic_incomplete) SELECT gen_random_uuid(),'n1',$1::timestamptz-i*interval '1 minute',1,2,1,1,false,1,false FROM generate_series(1,6000)i")
        .bind(anchor).execute(&pool).await.unwrap();
    sqlx::raw_sql("DELETE FROM traffic_dirty_hours; DELETE FROM online_dirty_minutes; ANALYZE traffic_minute_totals; ANALYZE online_cycle_metrics").execute(&pool).await.unwrap();
    sqlx::raw_sql("INSERT INTO traffic_hour_totals SELECT source,node_id,hysteriax_hour(minute),sum(tx),sum(rx),sum(samples),sum(incomplete_samples) FROM traffic_minute_totals GROUP BY source,node_id,hysteriax_hour(minute); INSERT INTO online_minute_totals SELECT scope_node_id,hysteriax_minute(started_at),count(*),count(users),coalesce(sum(users),0),max(users),count(connections),coalesce(sum(connections),0),max(connections),min(covered),max(expected),bool_or(incomplete),min(traffic_covered),bool_or(traffic_incomplete) FROM online_cycle_metrics GROUP BY scope_node_id,hysteriax_minute(started_at)")
        .execute(&pool).await.unwrap();
    fn read_rows(plan: &serde_json::Value, table: &str) -> f64 {
        let own = if plan["Relation Name"] == table {
            plan["Actual Rows"].as_f64().unwrap_or(0.0)
                * plan["Actual Loops"].as_f64().unwrap_or(0.0)
        } else {
            0.0
        };
        own + plan["Plans"]
            .as_array()
            .map(|children| {
                children
                    .iter()
                    .map(|child| read_rows(child, table))
                    .sum::<f64>()
            })
            .unwrap_or(0.0)
    }
    for dirty in [false, true] {
        if dirty {
            sqlx::query("INSERT INTO traffic_dirty_hours VALUES('users','n1',$1::timestamptz-interval '1 hour')").bind(anchor).execute(&pool).await.unwrap();
            sqlx::query(
                "INSERT INTO online_dirty_minutes VALUES('n1',$1::timestamptz-interval '1 minute')",
            )
            .bind(anchor)
            .execute(&pool)
            .await
            .unwrap();
        }
        let hours: serde_json::Value = sqlx::query_scalar(
            "SELECT coalesce(jsonb_agg(to_jsonb(d)),'[]'::jsonb) FROM traffic_dirty_hours d",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let minutes: serde_json::Value = sqlx::query_scalar(
            "SELECT coalesce(jsonb_agg(to_jsonb(d)),'[]'::jsonb) FROM online_dirty_minutes d",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        for statement in include_str!("refresh_rollups.sql")
            .split(';')
            .filter(|s| s.contains("ctid=ANY"))
        {
            let table = if statement.contains("traffic_hour_totals") {
                "traffic_hour_totals"
            } else {
                "online_minute_totals"
            };
            let mut tx = pool.begin().await.unwrap();
            sqlx::raw_sql("SET LOCAL jit=off; SET LOCAL plan_cache_mode=force_custom_plan")
                .execute(&mut *tx)
                .await
                .unwrap();
            let row = sqlx::query(&format!("EXPLAIN (ANALYZE,FORMAT JSON) {statement}"))
                .bind(if table.starts_with("traffic_") {
                    &hours
                } else {
                    &minutes
                })
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            let plan: serde_json::Value = row.get("QUERY PLAN");
            let scanned = read_rows(&plan[0]["Plan"], table);
            assert!(scanned < 100.0, "{table}: {scanned} rows, plan={plan}");
            assert_eq!(scanned > 0.0, dirty, "{table}");
            tx.rollback().await.unwrap();
        }
        for statement in include_str!("refresh_rollups.sql")
            .split(';')
            .filter(|s| s.contains("INSERT INTO"))
        {
            let select = statement
                .replace("INSERT INTO traffic_hour_totals\n", "")
                .replace("INSERT INTO online_minute_totals\n", "");
            let table = if select.contains("traffic_minute_totals") {
                "traffic_minute_totals"
            } else {
                "online_cycle_metrics"
            };
            let mut tx = pool.begin().await.unwrap();
            sqlx::raw_sql("SET LOCAL jit=off; SET LOCAL plan_cache_mode=force_custom_plan")
                .execute(&mut *tx)
                .await
                .unwrap();
            let row = sqlx::query(&format!("EXPLAIN (ANALYZE,FORMAT JSON) {select}"))
                .bind(if table.starts_with("traffic_") {
                    &hours
                } else {
                    &minutes
                })
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            let plan: serde_json::Value = row.get("QUERY PLAN");
            let scanned = read_rows(&plan[0]["Plan"], table);
            assert!(scanned < 100.0, "{table}: {scanned} rows, plan={plan}");
            assert_eq!(scanned > 0.0, dirty, "{table}");
            tx.rollback().await.unwrap();
        }
    }
    refresh(&pool).await.unwrap();
    let samples: i64 = sqlx::query_scalar(
        "SELECT samples FROM traffic_hour_totals WHERE source='users' AND node_id='n1' AND hour='2026-10-15T11:00:00Z'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(samples, 60);
    let users: i64 = sqlx::query_scalar(
        "SELECT users_sum::bigint FROM online_minute_totals WHERE scope_node_id='n1' AND minute='2026-10-15T11:59:00Z'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(users, 1);
}
