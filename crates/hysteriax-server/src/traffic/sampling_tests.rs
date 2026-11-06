use super::*;
use russh::{Channel, ChannelMsg, server};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, Semaphore};

#[derive(Clone)]
struct StatsServer {
    online: Arc<Notify>,
    clear: Arc<Notify>,
    gate: Arc<Semaphore>,
    requests: Arc<Mutex<Vec<String>>>,
    failed_online: bool,
    clear_gate: Arc<Semaphore>,
    instance: Arc<Notify>,
    instance_gate: Arc<Semaphore>,
}
impl server::Handler for StatsServer {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        _: &str,
        _: &str,
    ) -> std::result::Result<server::Auth, Self::Error> {
        Ok(server::Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: russh::ChannelId,
        command: &[u8],
        session: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        assert!(String::from_utf8_lossy(command).contains("systemctl show hysteriax.service"));
        self.instance.notify_one();
        self.instance_gate.acquire().await.unwrap().forget();
        session.channel_success(channel)?;
        session.data(channel, b"stable-instance\n".to_vec())?;
        session.exit_status_request(channel, 0)?;
        session.close(channel)?;
        Ok(())
    }
    async fn channel_open_direct_tcpip(
        &mut self,
        mut channel: Channel<server::Msg>,
        _: &str,
        _: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        session: &mut server::Session,
    ) -> std::result::Result<(), Self::Error> {
        reply.accept().await;
        let (online, clear, gate, requests, failed) = (
            self.online.clone(),
            self.clear.clone(),
            self.gate.clone(),
            self.requests.clone(),
            self.failed_online,
        );
        let clear_gate = self.clear_gate.clone();
        let handle = session.handle();
        let id = channel.id();
        tokio::spawn(async move {
            let Some(ChannelMsg::Data { data }) = channel.wait().await else {
                panic!("missing HTTP request")
            };
            let request = String::from_utf8_lossy(&data).into_owned();
            requests.lock().unwrap().push(request.clone());
            let (status, body) = if request.starts_with("GET /online ") {
                online.notify_one();
                gate.acquire().await.unwrap().forget();
                (if failed { 500 } else { 200 }, r#"{"user-0001":1}"#)
            } else {
                assert!(request.starts_with("GET /traffic?clear=1 "));
                clear.notify_one();
                clear_gate.acquire().await.unwrap().forget();
                (200, r#"{"user-0001":{"tx":17,"rx":0}}"#)
            };
            let response = format!(
                "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            handle.data(id, response.into_bytes()).await.unwrap();
            handle.close(id).await.unwrap();
        });
        Ok(())
    }
}

async fn stats_fixture(
    failed_online: bool,
    clear_permits: usize,
    instance_permits: usize,
) -> (AppState, StatsServer, tokio::task::JoinHandle<()>) {
    let pool = batch_tests::fixture(1).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    let key = russh::keys::PrivateKey::new(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[23; 32]).into(),
        "fixture",
    )
    .unwrap();
    let fingerprint = key
        .public_key()
        .fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
        .to_string();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let online = Arc::new(Notify::new());
    let clear = Arc::new(Notify::new());
    let gate = Arc::new(Semaphore::new(0));
    let requests = Arc::new(Mutex::new(vec![]));
    let handler = StatsServer {
        online: online.clone(),
        clear: clear.clone(),
        gate: gate.clone(),
        requests: requests.clone(),
        failed_online,
        clear_gate: Arc::new(Semaphore::new(clear_permits)),
        instance: Arc::new(Notify::new()),
        instance_gate: Arc::new(Semaphore::new(instance_permits)),
    };
    let handler_for_peer = handler.clone();
    let peer = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let handler = handler_for_peer.clone();
            let key = key.clone();
            tokio::spawn(async move {
                server::run_stream(
                    Arc::new(server::Config {
                        keys: vec![key],
                        ..Default::default()
                    }),
                    stream,
                    handler,
                )
                .await
                .unwrap()
                .await
            });
        }
    });
    sqlx::query(
    "INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('fixture-ssh',2,$1,clock_timestamp())",
)
.bind(secrets.encrypt(r#"{"secret":"fixture"}"#).unwrap())
.execute(&pool)
.await
.unwrap();
    sqlx::query("UPDATE nodes SET ssh_port=$1,ssh_host_fingerprint=$2,traffic_stats_secret_enc=$3,deployed_revision=1,ssh_credential_version=2 WHERE id='a'")
    .bind(i32::from(address.port())).bind(fingerprint).bind(secrets.encrypt("stats").unwrap()).execute(&pool).await.unwrap();
    (AppState::new(pool, secrets), handler, peer)
}

#[tokio::test]
#[cfg(unix)]
#[ignore = "requires HYSTERIAX_TEST_SERVER_BIN built from the same source; runs a real SIGTERM child"]
async fn real_server_sigterm_preserves_the_inflight_clear_response() {
    let binary = std::env::var("HYSTERIAX_TEST_SERVER_BIN").expect("build the server first");
    let log =
        std::env::var("HYSTERIAX_TEST_SERVER_LOG").expect("provide a retained child log path");
    let (state, handler, peer) = stats_fixture(false, 0, 16).await;
    handler.gate.add_permits(8);
    sqlx::query("UPDATE nodes SET desired_config_enc=$1")
        .bind(state.secrets.encrypt("{}").unwrap())
        .execute(&state.pool)
        .await
        .unwrap();
    crate::credentials::reference_index::initialize(&state.pool, &state.secrets)
        .await
        .unwrap();
    crate::traffic::access::initialize(&state.pool)
        .await
        .unwrap();
    crate::bootstrap::finalize(&state.pool, &state.secrets)
        .await
        .unwrap();
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut database = url::Url::parse(&std::env::var("TEST_DATABASE_URL").unwrap()).unwrap();
    database
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let output = std::fs::File::create(&log).unwrap();
    let mut child = tokio::process::Command::new(binary)
        .env_clear()
        .env("DATABASE_URL", database.as_str())
        .env(
            "HYSTERIAX_MASTER_KEY",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        )
        .env("HYSTERIAX_LISTEN_ADDR", address.to_string())
        .env("HYSTERIAX_SAMPLING_INTERVAL_SECONDS", "60")
        .env("HYSTERIAX_BACKGROUND_WORKER_PARALLELISM", "1")
        .env("RUST_LOG", "info")
        .stdout(output.try_clone().unwrap())
        .stderr(output)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let client = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(response) = client.get(format!("http://{address}/readyz")).send().await
                && response.status().is_success()
            {
                break;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before readiness; inspect {log}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), handler.clear.notified())
        .await
        .unwrap();
    let id: String = sqlx::query_scalar(
        "SELECT sample_id FROM traffic_sample_batches WHERE clear_requested_at IS NOT NULL",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-TERM", &child.id().unwrap().to_string()])
            .status()
            .unwrap()
            .success()
    );
    // Prove that HTTP has stopped accepting connections while the child remains
    // alive solely to finish the gated destructive response.
    tokio::time::timeout(Duration::from_secs(2), async {
        while tokio::net::TcpStream::connect(address).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "SIGTERM killed capture before its response; inspect {log}"
    );
    handler.clear_gate.add_permits(1);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "child drain failed; inspect {log}");
    let saved:(String,bool,String,i64)=sqlx::query_as("SELECT b.state,b.payload_digest IS NOT NULL,w.state,(SELECT usage_bytes FROM users WHERE id='user-0001') FROM traffic_sample_batches b JOIN background_work w ON w.id=b.sample_id WHERE b.sample_id=$1")
        .bind(&id).fetch_one(&state.pool).await.unwrap();
    assert_eq!(saved, ("complete".into(), true, "complete".into(), 17));
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM data_gaps WHERE reason='traffic_clear_uncertain'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM resource_leases WHERE work_id=$1 AND owner IS NOT NULL"
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        handler
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("GET /traffic?clear=1 "))
            .count(),
        1
    );
    assert!(
        std::fs::read_to_string(log)
            .unwrap()
            .contains("sampling workers drained")
    );
    peer.abort();
}

#[tokio::test]
async fn shutdown_drains_an_inflight_clear_after_http_stops_and_bills_it_once() {
    let (state, handler, peer) = stats_fixture(false, 0, 8).await;
    handler.gate.add_permits(1);
    let worker = tokio::spawn(super::run(state.clone()));
    tokio::time::timeout(Duration::from_secs(5), handler.clear.notified())
        .await
        .unwrap();
    let id: String = sqlx::query_scalar(
        "SELECT sample_id FROM traffic_sample_batches WHERE clear_requested_at IS NOT NULL",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    // Dispatch is already admitted. Its response must remain acceptable after
    // the slot closes and HTTP serving has finished.
    sqlx::query(
        "UPDATE background_work SET deadline_at=clock_timestamp()-interval '1 second' WHERE id=$1",
    )
    .bind(&id)
    .execute(&state.pool)
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    crate::serve_with_shutdown(
        listener,
        axum::Router::new(),
        state.shutdown.clone(),
        async {},
        Duration::from_millis(50),
    )
    .await
    .unwrap();
    let mut draining = tokio::spawn(async move {
        crate::drain_sampling_workers(&mut [("traffic", worker)], Duration::from_secs(5)).await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut draining)
            .await
            .is_err(),
        "shutdown abandoned the in-flight response"
    );
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM resource_leases WHERE work_id=$1 AND owner IS NOT NULL AND expires_at>clock_timestamp()")
        .bind(&id).fetch_one(&state.pool).await.unwrap(),2);
    handler.clear_gate.add_permits(1);
    draining.await.unwrap().unwrap();
    let saved:(String,bool,String,i64)=sqlx::query_as("SELECT b.state,b.payload_digest IS NOT NULL,w.state,(SELECT usage_bytes FROM users WHERE id='user-0001') FROM traffic_sample_batches b JOIN background_work w ON w.id=b.sample_id WHERE b.sample_id=$1")
        .bind(&id).fetch_one(&state.pool).await.unwrap();
    assert_eq!(saved, ("complete".into(), true, "complete".into(), 17));
    // Recovery/reprocessing cannot charge this interval again.
    batches::process(&state.pool, &id).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id='user-0001'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        17
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM resource_leases WHERE work_id=$1 AND owner IS NOT NULL"
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM data_gaps WHERE reason='traffic_clear_uncertain'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        handler
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("GET /traffic?clear=1 "))
            .count(),
        1
    );
    peer.abort();
}

#[tokio::test]
async fn shutdown_before_clear_admission_yields_ownership_and_next_instance_captures_once() {
    let (state, handler, peer) = stats_fixture(false, 1, 0).await;
    handler.gate.add_permits(2);
    let worker = tokio::spawn(super::run(state.clone()));
    tokio::time::timeout(Duration::from_secs(5), handler.instance.notified())
        .await
        .unwrap();
    state.shutdown.send_replace(true);
    let mut draining = tokio::spawn(async move {
        crate::drain_sampling_workers(&mut [("traffic", worker)], Duration::from_secs(5)).await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut draining)
            .await
            .is_err()
    );
    handler.instance_gate.add_permits(8);
    draining.await.unwrap().unwrap();
    let row:(String,String,String,bool,bool)=sqlx::query_as("SELECT w.id,w.state,w.error_code,b.clear_requested_at IS NOT NULL,b.payload_digest IS NOT NULL FROM background_work w JOIN traffic_sample_batches b ON b.sample_id=w.id")
        .fetch_one(&state.pool).await.unwrap();
    assert_eq!(row.1, "queued");
    assert_eq!(row.2, "shutdown_deferred");
    assert!(!row.3 && !row.4);
    assert!(
        !handler
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.starts_with("GET /traffic?clear=1 "))
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM resource_leases WHERE work_id=$1 AND owner IS NOT NULL"
        )
        .bind(&row.0)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second',deadline_at=clock_timestamp()+interval '1 minute' WHERE id=$1")
        .bind(&row.0).execute(&state.pool).await.unwrap();
    let next = AppState::new(
        state.pool.clone(),
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    let work = crate::background::claim(&next.pool, "traffic", &next.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(work.id, row.0);
    crate::execution::supervise(next.pool.clone(), work.leases.clone(), async {
        sample_work(&next, &work).await.unwrap();
        crate::background::finish(&next.pool, &work, true)
            .await
            .unwrap();
    })
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT usage_bytes FROM users WHERE id='user-0001'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        17
    );
    assert_eq!(
        handler
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.starts_with("GET /traffic?clear=1 "))
            .count(),
        1
    );
    peer.abort();
}

#[tokio::test]
async fn slow_or_failed_online_observation_cannot_delay_capture_or_discard_received_bytes() {
    for failed_online in [false, true] {
        let (state, handler, peer) = stats_fixture(failed_online, 1, 8).await;
        let pool = state.pool.clone();
        let (online, clear, gate, requests) = (
            handler.online,
            handler.clear,
            handler.gate,
            handler.requests,
        );
        let cycle = crate::background::ensure_cycle(&pool, "traffic")
            .await
            .unwrap();
        sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '10 seconds' WHERE cycle_id=$1")
            .bind(cycle).execute(&pool).await.unwrap();
        let work = crate::background::claim(&pool, "traffic", "fixture")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(work.node, "a");
        let (app, owned) = (state.clone(), work.clone());
        let task = tokio::spawn(async move {
            crate::execution::supervise(app.pool.clone(), owned.leases.clone(), async {
                let result = sample_work(&app, &owned).await;
                crate::background::finish(&app.pool, &owned, result.is_ok())
                    .await
                    .unwrap();
                result
            })
            .await
            .into_result()
            .unwrap()
            .unwrap()
            .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(5), online.notified())
            .await
            .unwrap();
        // The online response remains blocked. This must not delay the one-shot
        // clear request or its durable billing, even if online eventually fails.
        tokio::time::timeout(Duration::from_secs(5), clear.notified())
            .await
            .expect("slow online observation blocked destructive capture");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let bytes: i64 =
                sqlx::query_scalar("SELECT usage_bytes FROM users WHERE id='user-0001'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            if bytes == 17 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "received interval was not billed while observation waited"
            );
            tokio::task::yield_now().await;
        }
        assert!(!task.is_finished());
        gate.add_permits(1);
        task.await.unwrap();
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.starts_with("GET /traffic?clear=1 "))
                .count(),
            1
        );
        let result:(String,i64)=sqlx::query_as("SELECT b.state,(SELECT usage_bytes FROM users WHERE id='user-0001') FROM traffic_sample_batches b WHERE sample_id=$1")
            .bind(&work.id).fetch_one(&pool).await.unwrap();
        assert_eq!(result, ("complete".into(), 17));
        let quality:(String,String,bool)=sqlx::query_as("SELECT status,traffic_status,pending FROM online_samples WHERE cycle_id=$1 AND node_id='a'")
            .bind(cycle).fetch_one(&pool).await.unwrap();
        assert_eq!(
            quality,
            (
                if failed_online { "failed" } else { "ok" }.into(),
                "ok".into(),
                false
            )
        );
        peer.abort();
    }
}

#[tokio::test]
async fn captured_multipage_billing_yields_the_node_between_pages_and_resumes_after_slot_expiry() {
    let pool = batch_tests::fixture(501).await;
    sqlx::query("UPDATE nodes SET deployed_revision=1,mtls_state='ready',ssh_port=1 WHERE id='a'")
        .execute(&pool)
        .await
        .unwrap();
    let cycle = crate::background::ensure_cycle(&pool, "traffic")
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour' WHERE cycle_id=$1")
        .bind(cycle).execute(&pool).await.unwrap();
    let work = crate::background::claim(&pool, "traffic", "capture")
        .await
        .unwrap()
        .unwrap();
    batches::reserve_work(&pool, &work).await.unwrap();
    assert_eq!(
        clear::prepare(&pool, &work).await.unwrap(),
        clear::Admission::Send
    );
    let counters: Map<String, Value> = (1..=501)
        .map(|n| (format!("user-{n:04}"), serde_json::json!({"tx":17,"rx":0})))
        .collect();
    let raw = serde_json::to_vec(&counters).unwrap();
    batches::seal_clear(&pool, &work, "instance", &counters, &raw, true)
        .await
        .unwrap();
    let state = AppState::new(
        pool.clone(),
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    crate::execution::supervise(pool.clone(), work.leases.clone(), async {
        sample_work(&state, &work).await.unwrap();
        crate::background::finish(&pool, &work, true).await.unwrap();
    })
    .await
    .into_result()
    .unwrap()
    .unwrap();
    let first:(String,i64,String,Option<String>)=sqlx::query_as("SELECT b.state,b.processed_count,w.state,w.error_code FROM traffic_sample_batches b JOIN background_work w ON w.id=b.sample_id WHERE b.sample_id=$1")
        .bind(&work.id).fetch_one(&pool).await.unwrap();
    assert_eq!(
        first,
        (
            "ready".into(),
            500,
            "queued".into(),
            Some("billing_deferred".into())
        )
    );
    let proxy_cycle = crate::background::ensure_cycle(&pool, "proxy")
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour' WHERE cycle_id=$1")
        .bind(proxy_cycle).execute(&pool).await.unwrap();
    let proxy = crate::background::claim(&pool, "proxy", "observer")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(proxy.node, work.node);
    crate::execution::supervise(pool.clone(), proxy.leases.clone(), async {
        crate::background::finish(&pool, &proxy, true)
            .await
            .unwrap();
    })
    .await
    .into_result()
    .unwrap()
    .unwrap();
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second',deadline_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(&work.id).execute(&pool).await.unwrap();
    let resumed = crate::background::claim(&pool, "traffic", "billing")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumed.id, work.id);
    crate::execution::supervise(pool.clone(), resumed.leases.clone(), async {
        sample_work(&state, &resumed).await.unwrap();
        crate::background::finish(&pool, &resumed, true)
            .await
            .unwrap();
    })
    .await
    .into_result()
    .unwrap()
    .unwrap();
    let result:(String,i64,i64,i64)=sqlx::query_as("SELECT b.state,b.processed_count,(SELECT count(*) FROM traffic_records WHERE sample_id=$1),(SELECT sum(usage_bytes)::bigint FROM users) FROM traffic_sample_batches b WHERE sample_id=$1")
        .bind(&work.id).fetch_one(&pool).await.unwrap();
    assert_eq!(result, ("complete".into(), 501, 501, 501 * 17));
}
