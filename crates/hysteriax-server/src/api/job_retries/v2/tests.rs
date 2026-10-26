use super::*;
use reqwest::Client;

struct Case {
    kind: usize,
    node: String,
    job: String,
    body: Value,
}
impl Case {
    fn path(&self) -> String {
        format!(
            "/jobs/{}/{}",
            self.job,
            if self.kind < 3 {
                "retry"
            } else {
                "confirm-fingerprint"
            }
        )
    }
}
async fn fixture(kind: usize) -> (AppState, Case) {
    let pool = db::test_pool_with_max_connections(8).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    crate::credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    crate::credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    let state = AppState::new(pool, secrets);
    let input = crate::credentials::test_node_request(&state, json!({"name":"Node","ssh_host":"8.8.8.8","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"test","public_host":"node.example.test","public_port":443,"listen_addr":":443"})).await;
    let (_, Json(result)) = crate::api::nodes::create(State(state.clone()), Json(input))
        .await
        .unwrap();
    let node = result["node"]["id"].as_str().unwrap().to_owned();
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('job-operator',$1,'Job operator',clock_timestamp(),clock_timestamp())")
        .bind(crate::security::token_digest("job-admin-v2")).execute(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let (job, body) = if kind == 1 {
        let credential = crate::credentials::insert(
            &mut tx,
            &state.secrets,
            "DNS",
            "dns",
            None,
            &json!({"provider":"cloudflare","config":{"cloudflare_api_token":"test"}}),
            &json!({}),
            &json!({}),
        )
        .await
        .unwrap();
        sqlx::query("INSERT INTO dns_connections(id,name,provider,credential_id,credential_version,status) VALUES('connection','DNS','cloudflare',$1,1,'verified')")
            .bind(credential).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO dns_zones(id,connection_id,provider_zone_id,name,enabled) VALUES('zone','connection','remote-zone','example.test',true)").execute(&mut *tx).await.unwrap();
        let job = crate::api::enqueue_job_with_payload_in_tx(
            &mut tx,
            "dns-zone-refresh",
            None,
            None,
            json!({"dns_operation_id":"operation"}),
        )
        .await
        .unwrap();
        sqlx::query("INSERT INTO dns_operations(id,idempotency_key,request_sha256,connection_id,credential_version,resource_type,resource_id,action,payload,job_id) VALUES('operation','operation','digest','connection',1,'dns_zone','zone','zone-refresh','{}',$1)")
            .bind(&job).execute(&mut *tx).await.unwrap();
        sqlx::query("UPDATE jobs SET resource_key='dns-zone:zone',resource_type='dns_zone',resource_id='zone' WHERE id=$1").bind(&job).execute(&mut *tx).await.unwrap();
        (job, json!({"expected_revision":1}))
    } else if kind == 2 {
        sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES('kick-user','Kick',false,clock_timestamp(),clock_timestamp())").execute(&mut *tx).await.unwrap();
        let job = crate::api::enqueue_job_with_payload_in_tx(
            &mut tx,
            "kick",
            Some(&node),
            Some(1),
            json!({"user_id":"kick-user","kick_reason":"credentials_revoked"}),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE kick_requests SET state='needs_attention' WHERE node_id=$1")
            .bind(&node)
            .execute(&mut *tx)
            .await
            .unwrap();
        (job, json!({"expected_revision":1}))
    } else {
        let job_kind = match kind {
            0 => "deploy",
            3 | 7 => "ssh-test",
            4 => "deploy",
            5 => "sync",
            6 => "rollback",
            _ => unreachable!(),
        };
        let job = crate::api::enqueue_job_in_tx(&mut tx, job_kind, Some(&node), Some(1))
            .await
            .unwrap();
        if kind >= 3 {
            let other = crate::api::enqueue_job_in_tx(&mut tx, "ssh-test", Some(&node), Some(1))
                .await
                .unwrap();
            sqlx::query("UPDATE jobs SET status='waiting_confirmation',stage='waiting_confirmation' WHERE id=$1").bind(other).execute(&mut *tx).await.unwrap();
            let previous = if kind == 7 { Some("SHA256:old") } else { None };
            sqlx::query("UPDATE nodes SET state=$2,ssh_host_fingerprint=$3 WHERE id=$1")
                .bind(&node)
                .bind(if kind == 7 {
                    "fingerprint_changed"
                } else {
                    "needs_fingerprint"
                })
                .bind(previous)
                .execute(&mut *tx)
                .await
                .unwrap();
            sqlx::query("UPDATE jobs SET status='waiting_confirmation',stage='waiting_confirmation',result_json=$2 WHERE id=$1")
                .bind(&job).bind(json!({"result":{"node_revision":1,"fingerprint":"SHA256:new","expected_fingerprint":previous}})).execute(&mut *tx).await.unwrap();
        }
        (
            job,
            if kind >= 3 {
                json!({"expected_revision":1,"fingerprint":"SHA256:new"})
            } else {
                json!({"expected_revision":1})
            },
        )
    };
    if kind < 3 {
        sqlx::query("UPDATE jobs SET status='failed',stage='failed',finished_at=clock_timestamp() WHERE id=$1").bind(&job).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    (
        state,
        Case {
            kind,
            node,
            job,
            body,
        },
    )
}
async fn serve(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v2", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, crate::api::router(state))
            .await
            .unwrap()
    });
    (base, task)
}
fn client() -> Client {
    Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap()
}
async fn send(client: &Client, base: &str, case: &Case, key: Option<&str>) -> reqwest::Response {
    let mut request = client
        .post(format!("{base}{}", case.path()))
        .bearer_auth("job-admin-v2")
        .json(&case.body);
    if let Some(key) = key {
        request = request.header("idempotency-key", key);
    }
    request.send().await.unwrap()
}
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn snapshot(state: &AppState) -> Value {
    let tables = [
        "nodes",
        "config_versions",
        "jobs",
        "job_events",
        "dns_operations",
        "kick_requests",
        "audit_records",
        "resource_leases",
        "credential_reference_dirty",
        "credential_reference_edges",
        "config_resource_references",
        "management_receipts",
        "cache_bucket_versions",
        "cache_entity_versions",
    ];
    let parts: Vec<String> = tables
        .iter()
        .map(|t| {
            format!("'{t}',(SELECT jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text) FROM {t} t)")
        })
        .collect();
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT jsonb_build_object({})",
        parts.join(",")
    )))
    .fetch_one(&state.pool)
    .await
    .unwrap()
}
async fn assert_committed(state: &AppState, case: &Case, value: &Value) {
    assert_eq!(
        count(state, "SELECT count(*) FROM management_receipts").await,
        1
    );
    let action = if case.kind < 3 {
        "job.retry_requested"
    } else {
        "node.ssh_fingerprint_confirmed"
    };
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE actor='job-operator' AND action=$1"
        )
        .bind(action)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        1
    );
    let child = value["job_id"].as_str().unwrap();
    if case.kind < 3 {
        assert_ne!(child, case.job);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT retry_of_job_id FROM jobs WHERE id=$1")
                .bind(child)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            case.job
        );
        if case.kind == 1 {
            assert_eq!(
                sqlx::query_scalar::<_, String>(
                    "SELECT job_id FROM dns_operations WHERE id='operation'"
                )
                .fetch_one(&state.pool)
                .await
                .unwrap(),
                child
            );
        }
        if case.kind == 2 {
            assert_eq!(
                sqlx::query_scalar::<_, String>(
                    "SELECT latest_job_id FROM kick_requests WHERE node_id=$1"
                )
                .bind(&case.node)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
                child
            );
        }
    } else {
        assert_eq!(child, case.job);
        let row =
            sqlx::query("SELECT desired_revision,ssh_host_fingerprint FROM nodes WHERE id=$1")
                .bind(&case.node)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(row.get::<i64, _>("desired_revision"), 2);
        assert_eq!(row.get::<String, _>("ssh_host_fingerprint"), "SHA256:new");
        assert_eq!(
            count(
                state,
                "SELECT count(*) FROM config_versions WHERE revision=2"
            )
            .await,
            1
        );
        assert_eq!(
            count(state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
            1
        );
        assert_eq!(
            count(
                state,
                "SELECT count(*) FROM job_events WHERE event_type='job.fingerprint_confirmed'"
            )
            .await,
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT target_revision FROM jobs WHERE id=$1")
                .bind(&case.job)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            if case.kind == 6 { 1 } else { 2 }
        );
    }
}

#[tokio::test]
async fn retries_and_confirmations_have_one_concurrent_result_and_replay_after_sources_are_deleted()
{
    let client = client();
    for kind in 0..8 {
        let (state, case) = fixture(kind).await;
        let (base, server) = serve(state.clone()).await;
        let (a, b) = tokio::join!(
            send(&client, &base, &case, Some("race")),
            send(&client, &base, &case, Some("race"))
        );
        assert_eq!(a.status(), StatusCode::ACCEPTED, "kind {kind}");
        assert_eq!(b.status(), StatusCode::ACCEPTED, "kind {kind}");
        assert_eq!(a.headers()["cache-control"], "no-store");
        let bytes = a.bytes().await.unwrap();
        assert_eq!(b.bytes().await.unwrap(), bytes);
        let value = serde_json::from_slice(&bytes).unwrap();
        assert_committed(&state, &case, &value).await;
        sqlx::raw_sql("DELETE FROM dns_operations; DELETE FROM kick_requests; DELETE FROM jobs; DELETE FROM nodes").execute(&state.pool).await.unwrap();
        let before = snapshot(&state).await;
        let response = send(&client, &base, &case, Some("race")).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(response.bytes().await.unwrap(), bytes);
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn receipt_failure_rolls_back_retry_links_trust_history_events_and_cache_and_native_conflicts_commit_once()
 {
    let client = client();
    for code in ["40001", "40P01"] {
        for kind in 0..8 {
            let (state, case) = fixture(kind).await;
            let (base, server) = serve(state.clone()).await;
            sqlx::raw_sql("CREATE FUNCTION reject_job_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'receipt failed'; END $$; CREATE TRIGGER reject_job_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_job_receipt()")
                .execute(&state.pool).await.unwrap();
            let before = snapshot(&state).await;
            assert_eq!(
                send(&client, &base, &case, Some("retry")).await.status(),
                StatusCode::INTERNAL_SERVER_ERROR
            );
            assert_eq!(snapshot(&state).await, before, "kind {kind}, code {code}");
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SEQUENCE job_attempt; CREATE OR REPLACE FUNCTION reject_job_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('job_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$")))
                .execute(&state.pool).await.unwrap();
            let response = send(&client, &base, &case, Some("retry")).await;
            assert_eq!(
                response.status(),
                StatusCode::ACCEPTED,
                "kind {kind}, code {code}"
            );
            let bytes = response.bytes().await.unwrap();
            assert_committed(&state, &case, &serde_json::from_slice(&bytes).unwrap()).await;
            assert_eq!(count(&state, "SELECT last_value FROM job_attempt").await, 2);
            let before = snapshot(&state).await;
            assert_eq!(
                send(&client, &base, &case, Some("retry"))
                    .await
                    .bytes()
                    .await
                    .unwrap(),
                bytes
            );
            assert_eq!(snapshot(&state).await, before);
            server.abort();
        }
    }
}

#[tokio::test]
async fn missing_keys_and_changed_bodies_have_no_effect_and_historical_retries_do_not_retry_failed_successors()
 {
    let client = client();
    for kind in 0..8 {
        let (state, mut case) = fixture(kind).await;
        let (base, server) = serve(state.clone()).await;
        let before = snapshot(&state).await;
        assert_eq!(
            send(&client, &base, &case, None).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(snapshot(&state).await, before);
        let response = send(&client, &base, &case, Some("same")).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = response.bytes().await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        sqlx::query("UPDATE jobs SET status='failed',stage='failed',finished_at=clock_timestamp() WHERE id=$1")
            .bind(value["job_id"].as_str().unwrap()).execute(&state.pool).await.unwrap();
        let before = snapshot(&state).await;
        assert_eq!(
            send(&client, &base, &case, Some("same"))
                .await
                .bytes()
                .await
                .unwrap(),
            bytes
        );
        assert_eq!(snapshot(&state).await, before);
        case.body["expected_revision"] = json!(99);
        assert_eq!(
            send(&client, &base, &case, Some("same")).await.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}

#[tokio::test]
async fn a_new_retry_stays_blocked_until_remote_uncertainty_is_resolved() {
    let client = client();
    for kind in 0..3 {
        let (state, case) = fixture(kind).await;
        let resource = if kind == 1 {
            "dns-zone:zone".to_owned()
        } else {
            format!("node:{}", case.node)
        };
        sqlx::query("INSERT INTO resource_leases(resource_key,work_id,blocked) VALUES($1,$2,true)")
            .bind(resource)
            .bind(&case.job)
            .execute(&state.pool)
            .await
            .unwrap();
        let (base, server) = serve(state.clone()).await;
        let before = snapshot(&state).await;
        assert_eq!(
            send(&client, &base, &case, Some("blocked")).await.status(),
            StatusCode::CONFLICT
        );
        assert_eq!(snapshot(&state).await, before);
        server.abort();
    }
}
