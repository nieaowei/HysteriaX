use super::*;
use base64::Engine;

async fn fixture() -> (AppState, Vec<String>, String) {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let key = base64::engine::general_purpose::STANDARD_NO_PAD.encode([7_u8; 32]);
    let state = AppState::new(pool, crate::security::SecretBox::from_base64(&key).unwrap());
    let mut nodes = Vec::new();
    for name in ["One", "Two"] {
        let (_,Json(created))=crate::api::nodes::create(State(state.clone()),Json(serde_json::from_value(json!({"name":name,"ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"unused","public_host":"example.test","public_port":443,"listen_addr":":443"})).unwrap())).await.unwrap();
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
    let at = Utc::now();
    let body = serde_json::to_vec(&json!({&user:2})).unwrap();
    for id in &nodes {
        record_online(&state.pool, id, cycle, at, 2, Some(&body))
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) VALUES('t',$1,$2,'instance',100,100,10,20,now())").bind(&nodes[0]).bind(&user).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO proxy_probe_samples(node_id,revision,status,latency_ms,sampled_at) VALUES($1,1,'ok',10,now()),($1,1,'failed',NULL,now()-interval '1 second')").bind(&nodes[0]).execute(&state.pool).await.unwrap();
    let Json(history) = history(
        State(state.clone()),
        Query(HistoryQuery {
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
    let at = Utc::now();
    record_online(&state.pool, &nodes[0], cycle, at, 2, Some(&good))
        .await
        .unwrap();
    record_online(&state.pool, &nodes[1], cycle, at, 2, None)
        .await
        .unwrap();
    let Json(value) = history(
        State(state),
        Query(HistoryQuery {
            range: "today".into(),
            timezone: "UTC".into(),
            node_id: None,
            source: None,
        }),
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
    sqlx::query("INSERT INTO monitoring_probe_tokens(node_id,token_hash,expires_at) VALUES($1,$2,now()+interval '30 seconds')").bind(node).bind(crate::security::token_digest("monitor-secret")).execute(&state.pool).await.unwrap();
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
    assert_eq!(result["id"], format!("monitor-{node}"));
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
    let at = Utc::now();
    for node in &nodes {
        record_online(&state.pool, node, cycle, at, 2, Some(b"{}"))
            .await
            .unwrap();
        finish_traffic_sample(&state.pool, node, cycle, "ok").await;
    }
    let Json(value) = history(
        State(state.clone()),
        Query(HistoryQuery {
            range: "today".into(),
            timezone: "UTC".into(),
            node_id: None,
            source: None,
        }),
    )
    .await
    .unwrap();
    let bucket = value["buckets"].as_array().unwrap().last().unwrap();
    assert_eq!(bucket["tx_bytes"], 0);
    assert_eq!(bucket["incomplete"], false);
    assert_eq!(bucket["traffic_covered_nodes"], 2);
    finish_traffic_sample(&state.pool, &nodes[0], cycle, "failed").await;
    let Json(value) = history(
        State(state),
        Query(HistoryQuery {
            range: "today".into(),
            timezone: "UTC".into(),
            node_id: None,
            source: None,
        }),
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
                sqlx::query(&format!("DELETE FROM {table}"))
                    .execute(&state.pool)
                    .await
                    .unwrap();
            }
        }
    }
}

async fn failed_job(state: &AppState, node: &str, kind: &str, message: &str) -> String {
    let mut tx = crate::db::begin_write(&state.pool).await.unwrap();
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

    let unsupported = failed_job(&state, &nodes[0], "kick", "kick failure").await;
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
