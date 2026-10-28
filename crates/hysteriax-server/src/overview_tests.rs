use super::*;
use base64::Engine;

#[tokio::test]
async fn latest_monitoring_samples_use_node_indexes_for_empty_and_stale_nodes() {
    let (state, nodes, _) = fixture().await;
    // All history belongs to one stale node; the other has never been sampled.
    // A global time scan must not walk this history to prove the second is empty.
    sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,status) SELECT md5(i::text)::uuid,$1,now()-interval '2 days'-i*interval '1 second','ok' FROM generate_series(1,20000) i")
        .bind(&nodes[0]).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO proxy_probe_samples(node_id,revision,sampled_at,status,latency_ms) SELECT $1,1,now()-interval '2 days'-i*interval '1 second','ok',i FROM generate_series(1,20000) i")
        .bind(&nodes[0]).execute(&state.pool).await.unwrap();
    for table in ["nodes", "online_samples", "proxy_probe_samples"] {
        sqlx::query(sqlx::AssertSqlSafe(format!("ANALYZE {table}")))
            .execute(&state.pool)
            .await
            .unwrap();
    }
    let sql = include_str!("overview/summary_nodes.sql");
    let rows = sqlx::query(sql)
        .bind(Option::<Vec<String>>::None)
        .fetch_all(&state.pool)
        .await
        .unwrap();
    let empty = rows
        .iter()
        .find(|row| row.get::<String, _>("id") == nodes[1])
        .unwrap();
    assert!(
        empty
            .get::<Option<DateTime<Utc>>, _>("online_sampled_at")
            .is_none()
    );
    assert!(
        empty
            .get::<Option<DateTime<Utc>>, _>("probe_sampled_at")
            .is_none()
    );
    let stale = rows
        .iter()
        .find(|row| row.get::<String, _>("id") == nodes[0])
        .unwrap();
    assert!(stale.get::<DateTime<Utc>, _>("online_sampled_at") < Utc::now() - Duration::days(1));
    assert_eq!(stale.get::<f64, _>("probe_latency_ms"), 1.0);

    fn check_sample_scans(plan: &Value, indexes: &mut Vec<String>) {
        if matches!(
            plan["Relation Name"].as_str(),
            Some("online_samples" | "proxy_probe_samples")
        ) {
            indexes.push(
                plan["Index Name"]
                    .as_str()
                    .expect("latest sample must use an index")
                    .to_owned(),
            );
            assert!(plan["Index Cond"].as_str().unwrap().contains("node_id"));
            assert!(plan["Actual Rows"].as_f64().unwrap() <= 1.0);
            assert!(plan["Rows Removed by Filter"].as_f64().unwrap_or(0.0) < 10.0);
        }
        if let Some(children) = plan["Plans"].as_array() {
            for child in children {
                check_sample_scans(child, indexes);
            }
        }
    }
    // Both overview (all nodes) and the paginated node directory use this SQL.
    for ids in [None, Some(nodes.clone())] {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {sql}"
        )))
        .bind(ids)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        let plan: Value = row.get("QUERY PLAN");
        let mut indexes = Vec::new();
        check_sample_scans(&plan[0]["Plan"], &mut indexes);
        assert!(indexes.contains(&"online_samples_node_latest_idx".to_owned()));
        assert!(indexes.contains(&"proxy_probe_samples_node_latest_idx".to_owned()));
    }
    // Equal timestamps retain the existing id-descending tie break.
    let at = DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();
    for status in ["failed", "ok"] {
        sqlx::query(
            "INSERT INTO online_samples(cycle_id,node_id,sampled_at,status) VALUES($1,$2,$3,$4)",
        )
        .bind(Uuid::new_v4())
        .bind(&nodes[0])
        .bind(at)
        .bind(status)
        .execute(&state.pool)
        .await
        .unwrap();
    }
    let row = sqlx::query(sql)
        .bind(Some(vec![nodes[0].clone()]))
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("online_status"), "ok");
    assert_eq!(row.get::<DateTime<Utc>, _>("online_sampled_at"), at);
}

#[tokio::test]
async fn aligned_traffic_history_never_scans_raw_records() {
    let (state, nodes, user) = fixture().await;
    let anchor: DateTime<Utc> = "2026-10-14T12:00:00Z".parse().unwrap();
    sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) SELECT 'edge-test-'||i,$1,$2,'instance',0,0,1,2,$3::timestamptz-i*interval '1 second' FROM generate_series(1,1200) i")
        .bind(&nodes[0]).bind(&user).bind(anchor).execute(&state.pool).await.unwrap();
    let query = format!(
        "EXPLAIN (ANALYZE, FORMAT JSON) {}",
        include_str!("analytics/traffic_history.sql").replace("RAW_TABLE", "traffic_records")
    );
    fn raw_loops(plan: &Value) -> u64 {
        let own = if plan["Relation Name"] == "traffic_records" {
            plan["Actual Loops"].as_u64().unwrap()
        } else {
            0
        };
        own + plan["Plans"]
            .as_array()
            .map(|children| children.iter().map(raw_loops).sum())
            .unwrap_or(0)
    }
    for (offset, expected_scan) in [(Duration::zero(), false), (Duration::seconds(17), true)] {
        let end = anchor - offset;
        let row = sqlx::query(sqlx::AssertSqlSafe(query.as_str()))
            .bind("hour")
            .bind("users")
            .bind(end - Duration::days(1))
            .bind(end)
            .bind(Option::<String>::None)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let plan: Value = row.get("QUERY PLAN");
        assert_eq!(raw_loops(&plan[0]["Plan"]) > 0, expected_scan);
    }
}

#[tokio::test]
async fn history_metric_selection_preserves_requested_values_and_stable_anchor() {
    let (state, nodes, user) = fixture().await;
    let now = Utc::now();
    let cycle = Uuid::new_v4();
    let body = serde_json::to_vec(&json!({&user:2})).unwrap();
    record_online(
        &state.pool,
        &nodes[0],
        cycle,
        now - Duration::minutes(2),
        1,
        Some(&body),
    )
    .await
    .unwrap();
    finish_traffic_sample(&state.pool, &nodes[0], cycle, "ok").await;
    let mut results = Vec::new();
    for metrics in ["all", "traffic", "quality"] {
        let Json(value) = history_at(
            state.clone(),
            HistoryQuery {
                range: "24h".into(),
                timezone: "Asia/Shanghai".into(),
                node_id: None,
                source: None,
                metrics: Some(metrics.into()),
            },
            now,
        )
        .await
        .unwrap();
        assert_eq!(value["metrics"], metrics);
        results.push(value);
    }
    let all = results[0]["buckets"].as_array().unwrap().last().unwrap();
    let traffic = results[1]["buckets"].as_array().unwrap().last().unwrap();
    let quality = results[2]["buckets"].as_array().unwrap().last().unwrap();
    assert_eq!(all["tx_bytes"], 0);
    assert_eq!(traffic["tx_bytes"], all["tx_bytes"]);
    assert!(traffic["online_users_avg"].is_null());
    assert_eq!(quality["online_users_avg"], all["online_users_avg"]);
    assert_eq!(quality["online_users_avg"], 1.0);
    assert!(quality["tx_bytes"].is_null());
    let query = || HistoryQuery {
        range: "24h".into(),
        timezone: "UTC".into(),
        node_id: None,
        source: None,
        metrics: Some("traffic".into()),
    };
    let Json(value) = history(State(state.clone()), Query(query())).await.unwrap();
    let anchor = value["generated_at"]
        .as_str()
        .unwrap()
        .parse::<DateTime<Utc>>()
        .unwrap();
    assert_eq!(anchor.timestamp() % 60, 0);
    assert_eq!(anchor.timestamp_subsec_nanos(), 0);
    let mut invalid = query();
    invalid.metrics = Some("invalid".into());
    assert_eq!(
        history(State(state), Query(invalid))
            .await
            .unwrap_err()
            .status,
        axum::http::StatusCode::BAD_REQUEST
    );
}

async fn fixture() -> (AppState, Vec<String>, String) {
    fixture_with_connections(3).await
}

async fn fixture_with_connections(max_connections: u32) -> (AppState, Vec<String>, String) {
    let pool = crate::db::test_pool_with_max_connections(max_connections).await;
    let key = base64::engine::general_purpose::STANDARD_NO_PAD.encode([7_u8; 32]);
    let state = AppState::new(pool, crate::security::SecretBox::from_base64(&key).unwrap());
    let mut nodes = Vec::new();
    for name in ["One", "Two"] {
        let (_,Json(created))=crate::api::nodes::create(State(state.clone()),Json(crate::credentials::test_node_request(&state,json!({"name":name,"ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"unused","public_host":"example.test","public_port":443,"listen_addr":":443"})).await)).await.unwrap();
        let id = created["node"]["id"].as_str().unwrap().to_string();
        sqlx::query("UPDATE nodes SET deployed_revision=1,state='deployed',last_sample_at=now() WHERE id=$1").bind(&id).execute(&state.pool).await.unwrap();
        nodes.push(id);
    }
    let (_, Json(created)) = crate::api::users::create(
        State(state.clone()),
        Json(serde_json::from_value(json!({"name":"User","enabled":true})).unwrap()),
    )
    .await
    .unwrap();
    let user = created["id"].as_str().unwrap().to_string();
    for id in &nodes {
        sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES($1,$2,$3,'unused',now())").bind(&user).bind(id).bind(format!("credential-{id}")).execute(&state.pool).await.unwrap();
    }
    (state, nodes, user)
}

#[tokio::test]
async fn summary_deduplicates_users_excludes_probes_and_counts_all_jobs() {
    let (state, nodes, user) = fixture().await;
    let cycle = Uuid::new_v4();
    let at = Utc::now();
    let body = serde_json::to_vec(&json!({&user:2,"monitor-node":1})).unwrap();
    for id in &nodes {
        record_online(&state.pool, id, cycle, at, 2, Some(&body))
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at,finished_at) SELECT 'job-'||s,'sync',CASE WHEN s%2=0 THEN 'failed' ELSE 'queued' END,'queued',now(),now(),now(),CASE WHEN s%2=0 THEN now() ELSE NULL END FROM generate_series(1,250) s").execute(&state.pool).await.unwrap();
    let Json(summary) = get(State(state.clone())).await.unwrap();
    assert_eq!(summary["online_users"], 1);
    assert_eq!(summary["connections"], 4);
    assert_eq!(summary["covered_nodes"], 2);
    assert_eq!(summary["queued_jobs"], 125);
    assert_eq!(summary["failed_jobs_24h"], 125);
    sqlx::query("UPDATE online_samples SET sampled_at=now()-interval '2 minutes' WHERE node_id=$1")
        .bind(&nodes[0])
        .execute(&state.pool)
        .await
        .unwrap();
    let Json(summary) = get(State(state)).await.unwrap();
    assert_eq!(summary["covered_nodes"], 1);
    assert_eq!(summary["connections"], 2);
}

#[tokio::test]
async fn history_uses_cycle_deduplication_and_separates_unknown_from_zero() {
    let (state, nodes, user) = fixture().await;
    let cycle = Uuid::new_v4();
    let at = Utc::now() - Duration::minutes(2);
    let body = serde_json::to_vec(&json!({&user:2})).unwrap();
    for id in &nodes {
        record_online(&state.pool, id, cycle, at, 2, Some(&body))
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) VALUES('t',$1,$2,'instance',100,100,10,20,now()-interval '2 minutes')").bind(&nodes[0]).bind(&user).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO proxy_probe_samples(node_id,revision,status,latency_ms,sampled_at) VALUES($1,1,'ok',10,now()-interval '2 minutes'),($1,1,'failed',NULL,now()-interval '2 minutes 1 second')").bind(&nodes[0]).execute(&state.pool).await.unwrap();
    let Json(history) = history(
        State(state.clone()),
        Query(HistoryQuery {
            metrics: None,
            range: "7d".into(),
            timezone: "Asia/Shanghai".into(),
            node_id: None,
            source: None,
        }),
    )
    .await
    .unwrap();
    let buckets = history["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 7);
    let current = buckets.last().unwrap();
    assert_eq!(current["tx_bytes"], 10);
    assert_eq!(current["rx_bytes"], 20);
    assert_eq!(current["online_users_avg"], 1.0);
    assert_eq!(current["connections_avg"], 4.0);
    assert_eq!(current["probe_attempts"], 2);
    assert_eq!(current["probe_successes"], 1);
    assert_eq!(current["latency_p50_ms"], 10.0);
    assert!(buckets[0]["tx_bytes"].is_null());
    assert_eq!(buckets[0]["incomplete"], true);
    sqlx::query("INSERT INTO data_gaps(id,node_id,opened_at,reason) VALUES('gap',$1,now()-interval '2 days','failed')").bind(&nodes[0]).execute(&state.pool).await.unwrap();
    let Json(history) = super::history(
        State(state),
        Query(HistoryQuery {
            metrics: None,
            range: "30d".into(),
            timezone: "America/New_York".into(),
            node_id: None,
            source: Some("users".into()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(history["buckets"].as_array().unwrap().len(), 30);
    assert_eq!(
        history["buckets"].as_array().unwrap().last().unwrap()["incomplete"],
        true
    );
}

#[tokio::test]
async fn invalid_history_queries_and_missing_node_are_rejected() {
    let (state, _, _) = fixture().await;
    for (range, tz, source, node) in [
        ("90d", "UTC", "users", None),
        ("7d", "invalid", "users", None),
        ("7d", "UTC", "invalid", None),
        ("7d", "UTC", "users", Some("missing")),
    ] {
        assert!(
            history(
                State(state.clone()),
                Query(HistoryQuery {
                    metrics: None,
                    range: range.into(),
                    timezone: tz.into(),
                    node_id: node.map(str::to_owned),
                    source: Some(source.into())
                })
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn online_failed_samples_do_not_erase_successful_history() {
    let (state, nodes, user) = fixture().await;
    let good = serde_json::to_vec(&json!({&user:1})).unwrap();
    let cycle = Uuid::new_v4();
    // Keep the sample and the requested final bucket in the same fixed hour.
    let anchor: DateTime<Utc> = "2026-10-14T12:30:00Z".parse().unwrap();
    let at = anchor - Duration::minutes(2);
    record_online(&state.pool, &nodes[0], cycle, at, 2, Some(&good))
        .await
        .unwrap();
    record_online(&state.pool, &nodes[1], cycle, at, 2, None)
        .await
        .unwrap();
    let Json(value) = history_at(
        state,
        HistoryQuery {
            metrics: None,
            range: "today".into(),
            timezone: "UTC".into(),
            node_id: None,
            source: None,
        },
        anchor,
    )
    .await
    .unwrap();
    let bucket = value["buckets"].as_array().unwrap().last().unwrap();
    assert_eq!(bucket["online_users_avg"], 1.0);
    assert_eq!(bucket["online_incomplete"], true);
    assert_eq!(bucket["covered_nodes"], 1);
}

#[tokio::test]
async fn repeated_dst_hour_has_distinct_buckets_and_spring_day_has_23_hours() {
    let (state, nodes, user) = fixture().await;
    for (id, at, amount) in [
        ("dst-one", "2026-11-01T05:30:00Z", 10_i64),
        ("dst-two", "2026-11-01T06:30:00Z", 20_i64),
    ] {
        sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) VALUES($1,$2,$3,'dst',100,100,$4,0,$5)").bind(id).bind(&nodes[0]).bind(&user).bind(amount).bind(at.parse::<DateTime<Utc>>().unwrap()).execute(&state.pool).await.unwrap();
    }
    let Json(value) = history_at(
        state.clone(),
        HistoryQuery {
            metrics: None,
            range: "today".into(),
            timezone: "America/New_York".into(),
            node_id: None,
            source: None,
        },
        "2026-11-02T04:59:00Z".parse().unwrap(),
    )
    .await
    .unwrap();
    let buckets = value["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 25);
    assert_eq!(
        buckets.iter().filter(|b| !b["tx_bytes"].is_null()).count(),
        2
    );
    assert_eq!(
        buckets
            .iter()
            .filter_map(|b| b["tx_bytes"].as_i64())
            .sum::<i64>(),
        30
    );
    let Json(value) = history_at(
        state,
        HistoryQuery {
            metrics: None,
            range: "today".into(),
            timezone: "America/New_York".into(),
            node_id: None,
            source: None,
        },
        "2026-03-09T03:59:00Z".parse().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(value["buckets"].as_array().unwrap().len(), 23);
}

#[tokio::test]
async fn monitoring_credentials_are_short_lived_separate_from_deployment_credentials() {
    let (state, nodes, _) = fixture().await;
    let node = &nodes[0];
    sqlx::query("UPDATE nodes SET node_token_hash=$2 WHERE id=$1")
        .bind(node)
        .bind(crate::security::token_digest("node-secret"))
        .execute(&state.pool)
        .await
        .unwrap();
    crate::proxy_monitor::token_tests::install_fixture(&state, node, "monitor-secret").await;
    let auth = |credential: &str| {
        serde_json::from_value(json!({"addr":"","auth":credential,"tx":0})).unwrap()
    };
    let result = crate::api::subscriptions::hy2_auth(
        State(state.clone()),
        axum::extract::Path((node.clone(), "node-secret".into())),
        Json(auth("monitor-secret")),
    )
    .await
    .unwrap();
    let result = serde_json::to_value(result.0).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(
        result["id"],
        crate::proxy_monitor::identity(node, &crate::security::token_digest("monitor-secret"))
    );
    sqlx::query("UPDATE monitoring_probe_tokens SET expires_at=now()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    let result = crate::api::subscriptions::hy2_auth(
        State(state),
        axum::extract::Path((node.clone(), "node-secret".into())),
        Json(auth("monitor-secret")),
    )
    .await
    .unwrap();
    assert_eq!(serde_json::to_value(result.0).unwrap()["ok"], false);
}

#[tokio::test]
async fn successful_empty_traffic_is_zero_and_failed_collection_is_incomplete() {
    let (state, nodes, _) = fixture().await;
    let cycle = Uuid::new_v4();
    // Keep the sample and the requested final bucket in the same fixed hour.
    let anchor: DateTime<Utc> = "2026-10-14T12:30:00Z".parse().unwrap();
    let at = anchor - Duration::minutes(2);
    for node in &nodes {
        record_online(&state.pool, node, cycle, at, 2, Some(b"{}"))
            .await
            .unwrap();
        finish_traffic_sample(&state.pool, node, cycle, "ok").await;
    }
    let Json(value) = history_at(
        state.clone(),
        HistoryQuery {
            metrics: None,
            range: "today".into(),
            timezone: "UTC".into(),
            node_id: None,
            source: None,
        },
        anchor,
    )
    .await
    .unwrap();
    let bucket = value["buckets"].as_array().unwrap().last().unwrap();
    assert_eq!(bucket["tx_bytes"], 0);
    assert_eq!(bucket["incomplete"], false);
    assert_eq!(bucket["traffic_covered_nodes"], 2);
    finish_traffic_sample(&state.pool, &nodes[0], cycle, "failed").await;
    let Json(value) = history_at(
        state,
        HistoryQuery {
            metrics: None,
            range: "today".into(),
            timezone: "UTC".into(),
            node_id: None,
            source: None,
        },
        anchor,
    )
    .await
    .unwrap();
    let bucket = value["buckets"].as_array().unwrap().last().unwrap();
    assert_eq!(bucket["tx_bytes"], 0);
    assert_eq!(bucket["incomplete"], true);
    assert_eq!(bucket["traffic_covered_nodes"], 1);
}

#[tokio::test]
async fn rolling_windows_have_exact_bounds_across_midnight_and_dst() {
    let (state, nodes, user) = fixture().await;
    for now in [
        "2026-11-02T04:59:00Z",
        "2026-03-09T03:59:00Z",
        "2026-10-03T16:01:00.123456789Z",
    ] {
        let request_now: DateTime<Utc> = now.parse().unwrap();
        let now = request_now
            - Duration::nanoseconds(i64::from(request_now.timestamp_subsec_nanos() % 1_000));
        for (range, days, count) in [("24h", 1, 24), ("7d", 7, 7), ("30d", 30, 30)] {
            let start = now - Duration::days(days);
            for (index, at) in [
                start - Duration::seconds(1),
                start,
                now - Duration::seconds(1),
                now,
            ]
            .into_iter()
            .enumerate()
            {
                let id = format!("{range}-{now}-{index}");
                sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) VALUES($1,$2,$3,'rolling',100,100,10,20,$4)")
                    .bind(&id).bind(&nodes[0]).bind(&user).bind(at).execute(&state.pool).await.unwrap();
                sqlx::query("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,sampled_at) VALUES($1,'rolling',10,20,$2)")
                    .bind(&nodes[0]).bind(at).execute(&state.pool).await.unwrap();
                let cycle = Uuid::new_v4();
                record_online(
                    &state.pool,
                    &nodes[0],
                    cycle,
                    at,
                    1,
                    Some(&serde_json::to_vec(&json!({&user: 2})).unwrap()),
                )
                .await
                .unwrap();
                finish_traffic_sample(&state.pool, &nodes[0], cycle, "ok").await;
                sqlx::query("INSERT INTO proxy_probe_samples(node_id,revision,status,latency_ms,sampled_at) VALUES($1,1,'ok',10,$2)")
                    .bind(&nodes[0]).bind(at).execute(&state.pool).await.unwrap();
            }
            for timezone in ["Asia/Shanghai", "America/New_York", "UTC"] {
                for source in ["users", "network"] {
                    let Json(value) = history_at(
                        state.clone(),
                        HistoryQuery {
                            metrics: None,
                            range: range.into(),
                            timezone: timezone.into(),
                            node_id: Some(nodes[0].clone()),
                            source: Some(source.into()),
                        },
                        request_now,
                    )
                    .await
                    .unwrap();
                    let buckets = value["buckets"].as_array().unwrap();
                    assert_eq!(buckets.len(), count);
                    assert_eq!(buckets[0]["start"], json!(start));
                    assert_eq!(buckets.last().unwrap()["end"], json!(now));
                    for pair in buckets.windows(2) {
                        assert_eq!(pair[0]["end"], pair[1]["start"]);
                    }
                    assert_eq!(
                        buckets
                            .iter()
                            .filter_map(|b| b["tx_bytes"].as_i64())
                            .sum::<i64>(),
                        20
                    );
                    assert_eq!(
                        buckets
                            .iter()
                            .filter_map(|b| b["rx_bytes"].as_i64())
                            .sum::<i64>(),
                        40
                    );
                    assert_eq!(
                        buckets
                            .iter()
                            .filter_map(|b| b["probe_attempts"].as_i64())
                            .sum::<i64>(),
                        2
                    );
                    for bucket in [buckets.first().unwrap(), buckets.last().unwrap()] {
                        assert_eq!(bucket["online_users_avg"], 1.0);
                        assert_eq!(bucket["connections_avg"], 2.0);
                        assert_eq!(bucket["traffic_covered_nodes"], 1);
                    }
                }
            }
            for table in [
                "traffic_records",
                "node_network_samples",
                "online_samples",
                "proxy_probe_samples",
            ] {
                sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table}")))
                    .execute(&state.pool)
                    .await
                    .unwrap();
            }
        }
    }
}

async fn failed_job(state: &AppState, node: &str, kind: &str, message: &str) -> String {
    let mut tx = state.pool.begin().await.unwrap();
    let id = crate::api::enqueue_job_in_tx(&mut tx, kind, Some(node), Some(1))
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET status='failed',stage='failed',error_message=$2,finished_at=now() WHERE id=$1")
        .bind(&id).bind(message).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    id
}

async fn retry_job(state: &AppState, id: &str) -> Result<String, ApiError> {
    let (_, Json(value)) = crate::api::job_retries::retry(
        State(state.clone()),
        axum::extract::Path(id.into()),
        Json(crate::api::job_retries::RetryRequest {
            expected_revision: 1,
        }),
    )
    .await?;
    Ok(value["job_id"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn explicit_retry_chain_resolves_only_linked_reminders_and_preserves_history() {
    let (state, nodes, _) = fixture().await;
    let original = failed_job(&state, &nodes[0], "ssh-test", "original failure").await;
    let unrelated = failed_job(&state, &nodes[0], "ssh-test", "unrelated failure").await;
    let (left, right) = tokio::join!(retry_job(&state, &original), retry_job(&state, &original));
    let retry = left.unwrap();
    assert_eq!(retry, right.unwrap());
    let parent: String = sqlx::query_scalar("SELECT retry_of_job_id FROM jobs WHERE id=$1")
        .bind(&retry)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(parent, original);
    let Json(summary) = get(State(state.clone())).await.unwrap();
    let failures: Vec<_> = summary["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "job_failed")
        .collect();
    assert_eq!(failures.len(), 2);
    assert!(
        failures
            .iter()
            .any(|i| i["entity_id"] == retry && i["reason"] == "正在重试：original failure")
    );
    assert_eq!(summary["failed_jobs_24h"], 2);
    // A different successful operation on the same node does not resolve either.
    let independent = failed_job(&state, &nodes[0], "ssh-test", "independent").await;
    sqlx::query("UPDATE jobs SET status='succeeded' WHERE id=$1")
        .bind(&independent)
        .execute(&state.pool)
        .await
        .unwrap();
    let Json(summary) = get(State(state.clone())).await.unwrap();
    assert_eq!(
        summary["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["kind"] == "job_failed")
            .count(),
        2
    );
    sqlx::query("UPDATE jobs SET status='failed',error_message='latest failure',finished_at=now() WHERE id=$1").bind(&retry).execute(&state.pool).await.unwrap();
    let Json(summary) = get(State(state.clone())).await.unwrap();
    assert_eq!(
        summary["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["kind"] == "job_failed")
            .count(),
        2
    );
    assert!(
        summary["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["entity_id"] == retry && i["reason"] == "latest failure")
    );
    assert!(retry_job(&state, &original).await.is_err());
    let second = retry_job(&state, &retry).await.unwrap();
    sqlx::query("UPDATE jobs SET status='running' WHERE id=$1")
        .bind(&second)
        .execute(&state.pool)
        .await
        .unwrap();
    let Json(summary) = get(State(state.clone())).await.unwrap();
    assert!(
        summary["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["entity_id"] == second && i["reason"] == "正在重试：latest failure")
    );
    sqlx::query("UPDATE jobs SET status='succeeded',finished_at=now() WHERE id=$1")
        .bind(&second)
        .execute(&state.pool)
        .await
        .unwrap();
    let Json(summary) = get(State(state.clone())).await.unwrap();
    let failures: Vec<_> = summary["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "job_failed")
        .collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["entity_id"], unrelated);
    assert_eq!(summary["failed_jobs_24h"], 3);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE status='failed'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        3
    );
    assert!(retry_job(&state, &second).await.is_err());
}

#[tokio::test]
async fn retry_validation_cancellation_and_expiry_keep_failures_honest() {
    let (state, nodes, _) = fixture().await;
    let original = failed_job(&state, &nodes[0], "deploy", "deploy failure").await;
    let error = crate::api::job_retries::retry(
        State(state.clone()),
        axum::extract::Path(original.clone()),
        Json(crate::api::job_retries::RetryRequest {
            expected_revision: 99,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
    let retry = retry_job(&state, &original).await.unwrap();
    let row = sqlx::query("SELECT kind,payload_json FROM jobs WHERE id=$1")
        .bind(&retry)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("kind"), "sync");
    assert_eq!(row.get::<Value, _>("payload_json")["force"], true);
    sqlx::query("UPDATE jobs SET status='cancelled' WHERE id=$1")
        .bind(&retry)
        .execute(&state.pool)
        .await
        .unwrap();
    let Json(summary) = get(State(state.clone())).await.unwrap();
    assert!(
        summary["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["entity_id"] == retry && i["reason"] == "重试已取消：deploy failure")
    );
    let next = retry_job(&state, &retry).await.unwrap();
    assert_ne!(next, retry);
    sqlx::query("UPDATE jobs SET status='rolled_back',error_message='retry rolled back',finished_at=now() WHERE id=$1").bind(&next).execute(&state.pool).await.unwrap();
    let Json(summary) = get(State(state.clone())).await.unwrap();
    assert!(summary["issues"].as_array().unwrap().iter().any(|i|i["entity_id"]==next && i["reason"]=="重试失败，已回滚：retry rolled back"));
    let recovered = retry_job(&state, &next).await.unwrap();
    sqlx::query("UPDATE jobs SET status='succeeded' WHERE id=$1")
        .bind(&recovered)
        .execute(&state.pool)
        .await
        .unwrap();
    let Json(summary) = get(State(state.clone())).await.unwrap();
    assert!(
        !summary["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "job_failed")
    );

    let unsupported = failed_job(&state, &nodes[0], "uninstall", "uninstall failure").await;
    assert_eq!(
        retry_job(&state, &unsupported).await.unwrap_err().status,
        axum::http::StatusCode::BAD_REQUEST
    );
    sqlx::query("UPDATE nodes SET state='deleting' WHERE id=$1")
        .bind(&nodes[0])
        .execute(&state.pool)
        .await
        .unwrap();
    let blocked = failed_job(&state, &nodes[0], "ssh-test", "blocked").await;
    assert!(retry_job(&state, &blocked).await.is_err());
    sqlx::query("UPDATE jobs SET finished_at=now()-interval '25 hours' WHERE status='failed'")
        .execute(&state.pool)
        .await
        .unwrap();
    let Json(summary) = get(State(state.clone())).await.unwrap();
    assert!(
        !summary["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "job_failed")
    );
    assert_eq!(summary["failed_jobs_24h"], 0);
}

#[tokio::test]
async fn retry_preserves_rollback_target_and_exposes_links_in_job_api() {
    let (state, nodes, _) = fixture().await;
    let original = failed_job(&state, &nodes[0], "rollback", "rollback failure").await;
    assert!(retry_job(&state, &original).await.is_err());
    sqlx::query("UPDATE config_versions SET deployed_success=TRUE WHERE node_id=$1 AND revision=1")
        .bind(&nodes[0])
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET desired_revision=2,deployed_revision=2 WHERE id=$1")
        .bind(&nodes[0])
        .execute(&state.pool)
        .await
        .unwrap();
    let (_, Json(value)) = crate::api::job_retries::retry(
        State(state.clone()),
        axum::extract::Path(original.clone()),
        Json(crate::api::job_retries::RetryRequest {
            expected_revision: 2,
        }),
    )
    .await
    .unwrap();
    let retry = value["job_id"].as_str().unwrap().to_string();
    let row = sqlx::query("SELECT target_revision,kind FROM jobs WHERE id=$1")
        .bind(&retry)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("target_revision"), 1);
    assert_eq!(row.get::<String, _>("kind"), "rollback");
    let detail = crate::api::get_job(State(state.clone()), axum::extract::Path(original.clone()))
        .await
        .unwrap()
        .0;
    assert_eq!(detail["job"]["retry_job_id"], retry);
    let detail = crate::api::get_job(State(state.clone()), axum::extract::Path(retry.clone()))
        .await
        .unwrap()
        .0;
    assert_eq!(detail["job"]["retry_of_job_id"], original);
    assert_eq!(
        retry_job(&state, "missing-job").await.unwrap_err().status,
        axum::http::StatusCode::NOT_FOUND
    );
}

thread_local! {
    static SUMMARY_QUERY_COUNT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

async fn counted_summary(state: &AppState) -> (Value, usize) {
    use tracing_subscriber::prelude::*;

    struct QueryCounter;
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for QueryCounter {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if event.metadata().target() == "sqlx::query" {
                SUMMARY_QUERY_COUNT.with(|count| {
                    if let Some(value) = count.get() {
                        count.set(Some(value + 1));
                    }
                });
            }
        }
    }
    // A permanent dispatcher keeps SQLx DEBUG callsite interest stable while other
    // tests run in parallel. Only this current-thread runtime's requests are counted.
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let subscriber = tracing_subscriber::registry()
            .with(QueryCounter)
            .with(tracing_subscriber::filter::LevelFilter::DEBUG);
        tracing::subscriber::set_global_default(subscriber).expect("install test query counter");
    });
    struct CounterGuard;
    impl Drop for CounterGuard {
        fn drop(&mut self) {
            SUMMARY_QUERY_COUNT.with(|count| count.set(None));
        }
    }
    let _guard = CounterGuard;
    SUMMARY_QUERY_COUNT.with(|count| count.set(Some(0)));
    let Json(value) = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        get(State(state.clone())),
    )
    .await
    .expect("the overview must work with a single connection")
    .unwrap();
    (
        value,
        SUMMARY_QUERY_COUNT.with(|count| count.get().unwrap()),
    )
}

#[tokio::test(flavor = "current_thread")]
async fn summary_uses_constant_queries_and_ignores_unrelated_secrets_for_large_catalogs() {
    let (state, nodes, _) = fixture_with_connections(1).await;
    let (_, small_queries) = counted_summary(&state).await;
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_credential_id,ssh_credential_version,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,desired_revision,deployed_revision,state,last_sample_at,created_at,updated_at) SELECT 'bulk-'||lpad(i::text,3,'0'),'Bulk '||lpad(i::text,3,'0'),n.ssh_host,n.ssh_port,n.ssh_username,n.ssh_credential_id,n.ssh_credential_version,n.public_host,n.public_port,n.listen_addr,'hash-'||i,'SECRET-MUST-NOT-BE-READ','SECRET-MUST-NOT-BE-READ','INVALID-CONFIG-MUST-NOT-BE-READ',1,1,'deployed',now(),now(),now() FROM generate_series(1,235) i CROSS JOIN nodes n WHERE n.id=$1")
        .bind(&nodes[0]).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_packages(node_id,period_id,config,usage_bytes,sampled_at,next_reset_at) SELECT id,id,'{\"quota_bytes\":1000}',right(id,3)::bigint,now(),CASE WHEN id='bulk-235' THEN now()-interval '1 day' ELSE NULL END FROM nodes WHERE id LIKE 'bulk-%'")
        .execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE node_packages SET config=config || '{\"cycle\":\"monthly\"}' WHERE node_id='bulk-235'").execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE node_packages SET config='{\"quota_bytes\":2000}',usage_bytes=468 WHERE node_id='bulk-233'").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO users(id,name,enabled,expires_at,quota_bytes,usage_bytes,created_at,updated_at) SELECT 'bulk-user-'||i,'Bulk user '||i,i=235,now()-interval '1 day',100,100,now(),now() FROM generate_series(1,235) i")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('bulk-user-235','bulk-235','bulk-user-hash','ASSIGNMENT-MUST-NOT-BE-READ',now())")
        .execute(&state.pool).await.unwrap();
    let (value, large_queries) = counted_summary(&state).await;
    assert!(
        (6..=10).contains(&small_queries),
        "observed {small_queries} SQL statements"
    );
    eprintln!(
        "Overview SQL statements: {small_queries} for 2 nodes, {large_queries} for 237 nodes"
    );
    assert_eq!(
        small_queries, large_queries,
        "query count must not grow with directory size"
    );
    assert_eq!(value["node_count"], 237);
    assert_eq!(value["node_states"]["deployed"], 237);
    assert!(
        value.get("nodes").is_none(),
        "summary must not include the unpaginated monitoring directory"
    );
    let rank = value["quota_rank"].as_array().unwrap();
    assert_eq!(
        rank.iter()
            .map(|node| node["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "bulk-233", "bulk-234", "bulk-232", "bulk-231", "bulk-230", "bulk-229", "bulk-228",
            "bulk-227", "bulk-226", "bulk-225"
        ]
    );
    for node in rank {
        assert!(node["package"].is_object() && node["package_usage"].is_object());
        for key in ["ssh", "config", "yaml_preview", "dns_binding"] {
            assert!(node.get(key).is_none());
        }
    }
    let user_issues: Vec<_> = value["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|issue| issue["entity_type"] == "user")
        .collect();
    assert_eq!(user_issues.len(), 2);
    assert!(
        user_issues
            .iter()
            .all(|issue| issue["entity_id"] == "bulk-user-235")
    );
    assert!(!value.to_string().contains("MUST-NOT-BE-READ"));
    let stored_usage: i64 =
        sqlx::query_scalar("SELECT usage_bytes FROM node_packages WHERE node_id='bulk-235'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        stored_usage, 235,
        "the dashboard must not roll over billing records"
    );
}

#[tokio::test]
async fn summary_batches_latest_monitoring_and_preserves_package_and_warning_rules() {
    let (state, nodes, user) = fixture_with_connections(1).await;
    let first = &nodes[0];
    let second = &nodes[1];
    sqlx::query(
        "UPDATE node_packages SET config=$1,usage_bytes=1000,sampled_at=now() WHERE node_id=$2",
    )
    .bind(json!({"quota_bytes":1000,"expires_at":Utc::now()-Duration::days(1)}))
    .bind(first)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE node_packages SET config='{\"quota_bytes\":1000}',usage_bytes=900,sampled_at=now(),gap_reason='sample_gap' WHERE node_id=$1")
        .bind(second).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_alerts(id,node_id,kind,scope,active,created_at) VALUES('a',$1,'expired','test',true,'2000-01-01'),('b',$2,'traffic_warning','test',true,'2000-01-02'),('c',$2,'expiry_warning','test',true,'2000-01-03'),('inactive',$1,'expiring','test',false,'2000-01-01')")
        .bind(first).bind(second).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,node_id,kind,status,stage,payload_json,available_at,created_at,updated_at) VALUES('limit-failed',$1,'kick','failed','failed','{\"node_limit\":true}',now(),now(),now()),('other-failed',$1,'kick','failed','failed','{}',now(),now(),now()),('limit-pending',$1,'kick','queued','queued','{\"node_limit\":true}',now(),now(),now()),('other-pending',$1,'ssh-test','queued','queued','{}',now(),now(),now())")
        .bind(first).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO kick_requests(node_id,user_id,reasons,state,latest_job_id) VALUES($1,'limit-failed-user','{\"node_limit\":true}','needs_attention','limit-failed'),($1,'limit-pending-user','{\"node_limit\":true}','active','limit-pending')")
        .bind(first).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO data_gaps(id,node_id,opened_at,reason,resolved_at) VALUES('open',$1,now(),'test',NULL),('closed',$1,now(),'test',now())")
        .bind(first).execute(&state.pool).await.unwrap();
    for (node, status, counts) in [(first, "failed", 20), (second, "ok", 3)] {
        sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,status,connections) VALUES($1,$2,now()-interval '1 minute','ok',$3),($4,$2,now(),$5,$6)")
            .bind(Uuid::new_v4()).bind(node).bind(json!({&user:1}))
            .bind(Uuid::new_v4()).bind(status).bind(json!({&user:counts}))
            .execute(&state.pool).await.unwrap();
    }
    sqlx::query("INSERT INTO online_samples(cycle_id,node_id,sampled_at,status,connections) SELECT $1,node_id,sampled_at,'ok',$3 FROM online_samples WHERE node_id=$2 ORDER BY sampled_at DESC LIMIT 1")
        .bind(Uuid::new_v4()).bind(second).bind(json!({&user:7})).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO proxy_probe_samples(node_id,revision,status,external_status,connection_ms,latency_ms,sampled_at) VALUES($1,1,'failed','failed',20,NULL,now()-interval '1 minute'),($1,1,'ok','ok',10,15,now()),($2,2,'ok','ok',11,16,now())")
        .bind(first).bind(second).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO monitoring_probe_state(node_id,revision,health,sampled_at) VALUES($1,1,'healthy',now()),($2,2,'healthy',now())")
        .bind(first).bind(second).execute(&state.pool).await.unwrap();
    let Json(value) = get(State(state.clone())).await.unwrap();
    assert_eq!(value["node_count"], 2);
    assert_eq!(value["risk_nodes"], 2);
    assert_eq!(value["attention_nodes"], 2);
    assert_eq!(value["eligible_nodes"], 2);
    assert_eq!(value["covered_nodes"], 1);
    assert_eq!(value["online_users"], 1);
    assert_eq!(value["connections"], 7);
    let Json(monitoring_page) =
        list_nodes(State(state.clone()), Query(MonitoringNodesQuery::default()))
            .await
            .unwrap();
    let monitoring = monitoring_page["items"].as_array().unwrap();
    let first_monitor = monitoring
        .iter()
        .find(|node| node["node_id"] == *first)
        .unwrap();
    assert_eq!(first_monitor["online_status"], "failed");
    assert_eq!(first_monitor["probe_status"], "healthy");
    assert_eq!(first_monitor["latency_ms"], 15.0);
    let second_monitor = monitoring
        .iter()
        .find(|node| node["node_id"] == *second)
        .unwrap();
    assert_eq!(
        second_monitor["probe_status"], "unknown",
        "an old deployed revision cannot use a newer probe"
    );
    let rank = value["quota_rank"].as_array().unwrap();
    assert_eq!(rank.len(), 2);
    for node in rank {
        let (package, usage) =
            crate::node_limits::snapshot(&state.pool, node["id"].as_str().unwrap())
                .await
                .unwrap();
        assert_eq!(node["package"], package);
        assert_eq!(node["package_usage"], usage);
    }
    let issues = value["issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|issue| issue["id"] == format!("node:{first}:sampling"))
    );
    assert!(issues.iter().any(
        |issue| issue["id"] == format!("node:{first}:disconnect_failed") && issue["severity"] == 0
    ));
    assert!(
        issues
            .iter()
            .any(|issue| issue["id"] == format!("node:{first}:revocation"))
    );
    let second_package = issues
        .iter()
        .find(|issue| issue["id"] == format!("node:{second}:package"))
        .unwrap();
    assert_eq!(
        second_package["reason"],
        "节点用量达到预警阈值、套餐即将到期"
    );
}

#[tokio::test]
async fn summary_of_empty_database_preserves_unknown_monitoring_and_zero_counts() {
    let state = AppState::new(
        crate::db::test_pool().await,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    let Json(value) = get(State(state)).await.unwrap();
    assert_eq!(value["node_count"], 0);
    assert_eq!(value["node_states"], json!({}));
    assert_eq!(value["risk_nodes"], 0);
    assert_eq!(value["queued_jobs"], 0);
    assert!(value["online_users"].is_null() && value["connections"].is_null());
    for key in ["issues", "quota_rank"] {
        assert_eq!(value[key], json!([]));
    }
}

#[tokio::test]
async fn monitoring_nodes_page_searches_all_nodes_and_keeps_summary_global() {
    let (state, nodes, _) = fixture_with_connections(1).await;
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_credential_id,ssh_credential_version,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,desired_revision,deployed_revision,state,last_sample_at,created_at,updated_at) SELECT 'monitor-'||lpad(i::text,3,'0'),CASE WHEN i=235 THEN '历史%_节点' ELSE 'Monitoring' END,'10.0.0.'||i,22,'root',ssh_credential_id,ssh_credential_version,'monitor.example.test',443,':443','monitor-hash-'||i,'unused','unused','INVALID-UNRELATED-CIPHER',1,1,'deployed',now(),now(),now() FROM nodes CROSS JOIN generate_series(1,235)i WHERE id=$1")
        .bind(&nodes[0]).execute(&state.pool).await.unwrap();
    let mut ids = std::collections::BTreeSet::new();
    for page in 1..=12 {
        let Json(value) = list_nodes(
            State(state.clone()),
            Query(MonitoringNodesQuery {
                page: Some(page),
                page_size: Some(20),
                q: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(value["total"], 237);
        assert!(value["items"].as_array().unwrap().len() <= 20);
        for row in value["items"].as_array().unwrap() {
            assert!(ids.insert(row["node_id"].as_str().unwrap().to_owned()));
            assert!(row.get("config").is_none());
            assert!(row.get("ssh").is_none());
        }
    }
    assert_eq!(ids.len(), 237);
    for search in ["历史%_", "monitor-235", "10.0.0.235"] {
        let Json(value) = list_nodes(
            State(state.clone()),
            Query(MonitoringNodesQuery {
                page: Some(i64::MAX),
                page_size: Some(20),
                q: Some(search.into()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(value["total"], 1);
        assert_eq!(value["page"], 1);
        assert_eq!(value["items"][0]["node_id"], "monitor-235");
    }
    let Json(empty) = list_nodes(
        State(state.clone()),
        Query(MonitoringNodesQuery {
            q: Some("' OR true --".into()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(empty["total"], 0);
    assert_eq!(empty["items"], json!([]));
    for query in [
        MonitoringNodesQuery {
            page: Some(0),
            ..Default::default()
        },
        MonitoringNodesQuery {
            page_size: Some(201),
            ..Default::default()
        },
    ] {
        assert_eq!(
            list_nodes(State(state.clone()), Query(query))
                .await
                .unwrap_err()
                .status,
            axum::http::StatusCode::BAD_REQUEST
        );
    }
    let Json(summary) = get(State(state)).await.unwrap();
    assert_eq!(summary["node_count"], 237);
    assert!(summary.get("nodes").is_none());
    assert_eq!(summary["healthy_nodes"], 0);
}
