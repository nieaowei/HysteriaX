use super::*;
use crate::security::SecretBox;

async fn fixture() -> (AppState, Execution, String, Input) {
    let state = AppState::new(
        db::test_pool_with_max_connections(4).await,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('admin',$1,'fixture',now())").bind(crate::security::token_digest("admin")).execute(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let lease = execution::acquire(&mut tx, "node:fixture", "worker", "work")
        .await
        .unwrap()
        .unwrap();
    let (id, _) = lease
        .dispatch(&mut tx, "unknown", &json!({"kind":"unsupported"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let input = Input {
        expected_epoch: lease.epoch.clone(),
        expected_generation: lease.generation,
    };
    (state, lease, id, input)
}
async fn expire(state: &AppState) {
    sqlx::query("UPDATE resource_leases SET expires_at=CASE WHEN owner IS NULL THEN NULL ELSE now()-interval '1 second' END,blocked=CASE WHEN resource_key LIKE 'verify:%' THEN blocked ELSE true END")
        .execute(&state.pool)
        .await
        .unwrap();
}
async fn prepared(
    state: &AppState,
    id: &str,
    input: &Input,
    key: &str,
    digest: &str,
) -> Result<Preparation, ApiError> {
    prepare(
        state,
        id,
        input,
        key.into(),
        digest.into(),
        Uuid::new_v4().to_string(),
        "admin".into(),
    )
    .await
}
fn identity(state: &AppState, key: &str) -> RequestIdentity {
    let mut headers = HeaderMap::new();
    headers.insert("idempotency-key", key.parse().unwrap());
    RequestIdentity::from_headers(
        &state.secrets,
        "admin",
        "execution.reverify",
        &headers,
        json!({"fixture":key}),
    )
    .unwrap()
}
#[tokio::test]
async fn unsupported_recheck_reserves_body_and_never_unblocks_or_replays_effects() {
    let (state, lease, id, input) = fixture().await;
    assert_eq!(
        prepared(&state, &id, &input, "live", "a")
            .await
            .err()
            .unwrap()
            .status,
        StatusCode::CONFLICT
    );
    expire(&state).await;
    assert!(matches!(
        prepared(&state, &id, &input, "key", "a").await.unwrap(),
        Preparation::Unsupported
    ));
    assert_eq!(
        prepared(&state, &id, &input, "key", "different")
            .await
            .err()
            .unwrap()
            .status,
        StatusCode::CONFLICT
    );
    let response = final_result(
        &state,
        identity(&state, "key"),
        id.clone(),
        input,
        None,
        None,
        "unsupported",
    )
    .await
    .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["state"], "needs_attention");
    assert_eq!(value["resource_blocked"], true);
    assert_eq!(value["retry_allowed"], false);
    let row =
        sqlx::query("SELECT owner,generation,blocked FROM resource_leases WHERE resource_key=$1")
            .bind(&lease.resource)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(row.get::<String, _>("owner"), lease.owner);
    assert_eq!(row.get::<i64, _>("generation"), lease.generation);
    assert!(row.get::<bool, _>("blocked"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM execution_rechecks")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn unsupported_finalization_rejects_new_owner_and_wrong_incarnation() {
    let (state, _, id, mut input) = fixture().await;
    expire(&state).await;
    input.expected_generation += 1;
    assert_eq!(
        prepared(&state, &id, &input, "wrong", "a")
            .await
            .err()
            .unwrap()
            .status,
        StatusCode::CONFLICT
    );
    input.expected_generation -= 1;
    assert!(matches!(
        prepared(&state, &id, &input, "key", "a").await.unwrap(),
        Preparation::Unsupported
    ));
    sqlx::query("UPDATE resource_leases SET owner='new-owner',work_id='new-work',generation=generation+1,blocked=false WHERE resource_key='node:fixture'").execute(&state.pool).await.unwrap();
    let error = final_result(
        &state,
        identity(&state, "key"),
        id,
        input,
        None,
        None,
        "unsupported",
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM management_receipts")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT blocked FROM resource_leases LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn already_verified_recheck_cannot_quarantine_current_worker_and_reserves_key() {
    let (state, _, id, input) = fixture().await;
    sqlx::query("UPDATE execution_operations SET state='verified',evidence='{}'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE resource_leases SET owner='new-owner',work_id='new-work',generation=generation+1,blocked=false WHERE resource_key='node:fixture'").execute(&state.pool).await.unwrap();
    assert!(matches!(
        prepared(&state, &id, &input, "key", "a").await.unwrap(),
        Preparation::Verified
    ));
    assert_eq!(
        prepared(&state, &id, &input, "key", "different")
            .await
            .err()
            .unwrap()
            .status,
        StatusCode::CONFLICT
    );
    final_result(
        &state,
        identity(&state, "key"),
        id,
        input,
        None,
        None,
        "verified",
    )
    .await
    .unwrap();
    let row = sqlx::query("SELECT owner,work_id,blocked FROM resource_leases LIMIT 1")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("owner"), "new-owner");
    assert_eq!(row.get::<String, _>("work_id"), "new-work");
    assert!(!row.get::<bool, _>("blocked"));
}
#[tokio::test]
async fn listing_is_bounded_filters_work_and_rejects_oversized_cursor() {
    let (state, _, _, _) = fixture().await;
    let Json(value) = list(
        State(state.clone()),
        Query(ListInput {
            work_id: Some("work".into()),
            after: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(value["items"].as_array().unwrap().len(), 1);
    assert!(
        list(
            State(state.clone()),
            Query(ListInput {
                work_id: None,
                after: Some("x".repeat(201))
            })
        )
        .await
        .is_err()
    );
    let Json(value) = list(
        State(state),
        Query(ListInput {
            work_id: Some("other".into()),
            after: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(value["items"].as_array().unwrap().len(), 0);
}

async fn supported_fixture() -> (AppState, String, Input) {
    let (state, ssh, _, _) = crate::dns::tests::fixture().await;
    let node = crate::dns::tests::node(&state, &ssh).await;
    sqlx::query("UPDATE nodes SET ssh_host_fingerprint='SHA256:fixture' WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('admin',$1,'fixture',now())").bind(crate::security::token_digest("admin")).execute(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let work = crate::api::enqueue_job_with_payload_in_tx(
        &mut tx,
        "sync",
        Some(&node),
        Some(1),
        json!({}),
    )
    .await
    .unwrap();
    let resource = format!("node:{node}");
    let lease = execution::acquire(&mut tx, &resource, "old-worker", &work)
        .await
        .unwrap()
        .unwrap();
    let job_key = format!("job:{work}");
    execution::acquire(&mut tx, &job_key, "old-worker", &work)
        .await
        .unwrap()
        .unwrap();
    let endpoint = endpoint_on(&mut tx, &node).await.unwrap();
    let intent = json!({"kind":"ssh-command","request_digest":crate::security::token_digest("fixture"),"verification":{"protocol":"ssh-journal-v2","endpoint":endpoint,"expected_head":"empty"}});
    let (id, _) = lease
        .dispatch(&mut tx, "ssh-command:0", &intent)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    expire(&state).await;
    (
        state,
        id,
        Input {
            expected_epoch: lease.epoch,
            expected_generation: lease.generation,
        },
    )
}
async fn ready(state: &AppState, id: &str, input: &Input, key: &str) -> Box<Prepared> {
    match prepared(state, id, input, key, "a").await.unwrap() {
        Preparation::Ready(p) => p,
        _ => panic!("expected preparation"),
    }
}
#[tokio::test]
async fn preparation_reserves_generation_and_source_changes_fence_finalization() {
    let (state, id, input) = supported_fixture().await;
    let p = ready(&state, &id, &input, "key").await;
    assert!(p.data.target_generation > input.expected_generation);
    let mut tx = state.pool.begin().await.unwrap();
    validate_on(&mut tx, &p).await.unwrap();
    tx.rollback().await.unwrap();
    sqlx::query("UPDATE jobs SET payload_json=jsonb_build_object('changed',true) WHERE id=$1")
        .bind(&p.data.original.work_id)
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    assert!(validate_on(&mut tx, &p).await.is_err());
    tx.rollback().await.unwrap();
    sqlx::query("UPDATE jobs SET payload_json='{}' WHERE id=$1")
        .bind(&p.data.original.work_id)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET ssh_host='changed.example.test' WHERE id=$1")
        .bind(&p.data.node)
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    assert!(validate_on(&mut tx, &p).await.is_err());
    tx.rollback().await.unwrap();
    assert_eq!(
        final_result(
            &state,
            identity(&state, "key"),
            id,
            input,
            Some(*p),
            None,
            "unavailable",
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM management_receipts")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn pending_fence_resumes_same_nonce_after_lease_expiry_without_reserving_again() {
    let (state, id, input) = supported_fixture().await;
    let p = ready(&state, &id, &input, "first").await;
    let generation = p.data.target_generation;
    expire(&state).await;
    let resumed = ready(&state, &id, &input, "second").await;
    assert_eq!(resumed.id, p.id);
    assert_eq!(resumed.data.target_generation, generation);
    assert_eq!(resumed.data.coordinates, p.data.coordinates);
    let mut tx = state.pool.begin().await.unwrap();
    assert!(validate_on(&mut tx, &p).await.is_err());
    tx.rollback().await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    validate_on(&mut tx, &resumed).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM execution_rechecks")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
#[ignore = "requires an owned Linux Docker fixture via TEST_SSH_JOURNAL_CONTAINER"]
async fn actual_http_recheck_recovers_lost_fence_response_with_same_nonce_and_one_payload() {
    use crate::ssh::journal::tests::{Linux, fixture_handler};
    use std::sync::Arc;
    let linux = Linux {
        container: std::env::var("TEST_SSH_JOURNAL_CONTAINER").unwrap(),
        home: format!("/tmp/hx-0034-api-recheck-{}", Uuid::new_v4()),
    };
    assert!(linux.run("mkdir -m 700 \"$HOME\"").status.success());
    let (state, id, input) = supported_fixture().await;
    let key = russh::keys::PrivateKey::new(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[27; 32]).into(),
        "fixture",
    )
    .unwrap();
    let fingerprint = key
        .public_key()
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string();
    let config = Arc::new(russh::server::Config {
        keys: vec![key],
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (container, home) = (linux.container.clone(), linux.home.clone());
    let ssh_server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (config, handler) = (
                config.clone(),
                fixture_handler(container.clone(), home.clone()),
            );
            tokio::spawn(async move {
                if let Ok(s) = russh::server::run_stream(config, stream, handler).await {
                    let _ = s.await;
                }
            });
        }
    });
    let mut tx = state.pool.begin().await.unwrap();
    let (mut op, _) = load_on(&mut tx, &id).await.unwrap();
    let node = op.resource_key.strip_prefix("node:").unwrap();
    sqlx::query("UPDATE nodes SET ssh_port=$1,ssh_host_fingerprint=$2 WHERE id=$3")
        .bind(i32::from(port))
        .bind(&fingerprint)
        .bind(node)
        .execute(&mut *tx)
        .await
        .unwrap();
    let endpoint = endpoint_on(&mut tx, node).await.unwrap();
    let command = "printf once >> \"$HOME/payload\"";
    op.intent["request_digest"] = json!(crate::security::token_digest(command));
    op.intent["verification"]["endpoint"] = serde_json::to_value(&endpoint).unwrap();
    sqlx::query("UPDATE execution_operations SET intent=$2 WHERE operation_id=$1")
        .bind(&id)
        .bind(&op.intent)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET status='failed' WHERE id=$1")
        .bind(&op.work_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // Model a completed original command whose response never reached its caller.
    assert!(
        linux
            .run(&crate::ssh::journal::wrap(&op.step(), command, 128).unwrap())
            .status
            .success()
    );
    assert!(
        linux
            .run("touch \"$HOME/drop-recheck-ack\"")
            .status
            .success()
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/api/v2/execution-operations/{id}/reverify",
        listener.local_addr().unwrap()
    );
    let app = crate::api::router(state.clone());
    let http_server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();
    let body = json!({"expected_epoch":input.expected_epoch,"expected_generation":input.expected_generation});
    assert_eq!(
        client.post(&url).json(&body).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth("admin")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let first = client
        .post(&url)
        .bearer_auth("admin")
        .header("idempotency-key", "lost-fence")
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = first.status();
    let first: Value = first.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["state"], "needs_attention");
    assert_eq!(first["resource_blocked"], true);
    let check_id = first["recheck_id"].as_str().unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM execution_rechecks WHERE id=$1")
            .bind(check_id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "dispatching"
    );
    let second = client
        .post(&url)
        .bearer_auth("admin")
        .header("idempotency-key", "resume-fence")
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = second.status();
    let bytes = second.bytes().await.unwrap();
    let result: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["state"], "verified");
    assert_eq!(result["recheck_id"], check_id);
    assert_eq!(result["resource_blocked"], false);
    assert_eq!(result["retry_allowed"], true);
    assert_eq!(linux.run("cat \"$HOME/payload\"").stdout, b"once");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id=$1")
            .bind(&op.work_id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "failed"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM execution_rechecks")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM resource_leases WHERE work_id=$1 OR (resource_key=$2 AND blocked)"
        )
        .bind(&op.work_id)
        .bind(&op.resource_key)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
    // The committed HTTP result replays before consulting changed live sources.
    sqlx::query("UPDATE nodes SET ssh_port=1 WHERE id=$1")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    let replay = client
        .post(&url)
        .bearer_auth("admin")
        .header("idempotency-key", "resume-fence")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.bytes().await.unwrap(), bytes);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE action='execution.reverified'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM management_receipts WHERE operation='execution.reverify'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        2
    );
    http_server.abort();
    ssh_server.abort();
}

async fn completed_preparation(
    state: &AppState,
    id: &str,
    input: &Input,
    key: &str,
) -> (Prepared, Proof) {
    let mut p = *ready(state, id, input, key).await;
    let completion = crate::ssh::journal::Receipt {
        protocol: 2,
        context: p.data.original.step(),
        state: "completed".into(),
        exit_code: 0,
        stdout_bytes: 0,
        stderr_bytes: 0,
        stdout_sha256: hex::encode(Sha256::digest(b"")),
        stderr_sha256: hex::encode(Sha256::digest(b"")),
    };
    let request = FenceRequest {
        id: p.id.clone(),
        original: p.data.original.step(),
        receipt_sha256: recheck::digest(&completion).unwrap(),
        target_epoch: p.data.epoch.clone(),
        target_generation: p.data.target_generation,
    };
    p.data.fence = Some(request.clone());
    sqlx::query("UPDATE execution_rechecks SET prepared=$2,state='dispatching' WHERE id=$1")
        .bind(&p.id)
        .bind(serde_json::to_value(&p.data).unwrap())
        .execute(&state.pool)
        .await
        .unwrap();
    (
        p,
        Proof {
            protocol: 2,
            request,
            completion,
            state: "fenced".into(),
        },
    )
}
#[tokio::test]
async fn proof_ack_receipt_failure_rolls_back_then_native_conflicts_reuse_one_proof() {
    let (state, id, input) = supported_fixture().await;
    let (p, proof) = completed_preparation(&state, &id, &input, "key").await;
    sqlx::raw_sql("CREATE FUNCTION reject_recheck_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'rejected'; END $$; CREATE TRIGGER reject_recheck_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION reject_recheck_receipt()")
        .execute(&state.pool).await.unwrap();
    let attempt = || {
        final_result(
            &state,
            identity(&state, "key"),
            id.clone(),
            input.clone(),
            Some(p.clone_for_write()),
            Some(proof.clone()),
            "verified",
        )
    };
    assert!(attempt().await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM execution_operations WHERE operation_id=$1"
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        "dispatching"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM execution_rechecks WHERE id=$1")
            .bind(&p.id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "dispatching"
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT blocked FROM resource_leases WHERE resource_key=$1")
            .bind(&p.data.original.resource_key)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE action='execution.reverified'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_recheck_receipt ON management_receipts; DROP FUNCTION reject_recheck_receipt(); CREATE SEQUENCE recheck_attempt; CREATE FUNCTION retry_recheck_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE n bigint; BEGIN n=nextval('recheck_attempt'); IF n=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; ELSIF n=2 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40P01'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_recheck_receipt BEFORE UPDATE ON management_receipts FOR EACH ROW EXECUTE FUNCTION retry_recheck_receipt()")
        .execute(&state.pool).await.unwrap();
    let first = attempt().await.unwrap();
    let first = axum::body::to_bytes(first.into_body(), 65536)
        .await
        .unwrap();
    let replay = attempt().await.unwrap();
    assert_eq!(
        axum::body::to_bytes(replay.into_body(), 65536)
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM recheck_attempt")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE action='execution.reverified'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM job_events WHERE event_type='job.reverified'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        1
    );
}
#[tokio::test]
async fn one_verified_operation_cannot_release_another_unknown_step_of_the_same_work() {
    let (state, id, input) = supported_fixture().await;
    let (p, proof) = completed_preparation(&state, &id, &input, "key").await;
    sqlx::query("INSERT INTO execution_operations(operation_id,resource_key,owner,generation,epoch,work_id,step,state,intent) SELECT $2,resource_key,owner,generation,epoch,work_id,'another-step','needs_attention',intent FROM execution_operations WHERE operation_id=$1")
        .bind(&id).bind(Uuid::new_v4().to_string()).execute(&state.pool).await.unwrap();
    let result = final_result(
        &state,
        identity(&state, "key"),
        id,
        input,
        Some(p),
        Some(proof),
        "verified",
    )
    .await
    .unwrap();
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(result.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["state"], "verified");
    assert_eq!(body["resource_blocked"], true);
    assert_eq!(body["retry_allowed"], false);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM resource_leases WHERE resource_key NOT LIKE 'verify:%' AND NOT blocked").fetch_one(&state.pool).await.unwrap(),0);
}
