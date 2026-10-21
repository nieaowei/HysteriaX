use super::*;
use axum::{
    Json,
    extract::{Path, State},
};
use russh::{Channel, ChannelId, server};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone)]
struct SSHFixture {
    authentications: Arc<AtomicUsize>,
    reject: bool,
    stop_before_remote_changes: bool,
}
impl server::Handler for SSHFixture {
    type Error = russh::Error;
    async fn auth_password(&mut self, _: &str, _: &str) -> Result<server::Auth, Self::Error> {
        self.authentications.fetch_add(1, Ordering::SeqCst);
        Ok(if self.reject {
            server::Auth::reject()
        } else {
            server::Auth::Accept
        })
    }
    async fn channel_open_session(
        &mut self,
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        if self.stop_before_remote_changes
            && String::from_utf8_lossy(data).contains("getent passwd hysteriax")
        {
            session.extended_data(
                channel,
                1,
                b"fixture stopped before remote changes".to_vec(),
            )?;
            session.exit_status_request(channel, 57)?;
            session.close(channel)?;
            return Ok(());
        }
        let output = if String::from_utf8_lossy(data).contains("/etc/os-release") {
            "debian\n12\nx86_64\nsystemd 252\nroot\n"
        } else {
            ""
        };
        session.data(channel, output.as_bytes().to_vec())?;
        session.exit_status_request(channel, 0)?;
        session.close(channel)?;
        Ok(())
    }
}

async fn fixture(
    reject: bool,
) -> (
    AppState,
    String,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    fixture_with_preflight_stop(reject, false).await
}

async fn fixture_with_preflight_stop(
    reject: bool,
    stop_before_remote_changes: bool,
) -> (
    AppState,
    String,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let (state, ssh, _, _) = crate::dns::tests::fixture().await;
    let node = crate::dns::tests::node(&state, &ssh).await;
    let key = russh::keys::PrivateKey::new(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[9; 32]).into(),
        "test",
    )
    .unwrap();
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    sqlx::query("UPDATE nodes SET ssh_host='127.0.0.1',ssh_port=$1 WHERE id=$2")
        .bind(i32::from(port))
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let authentications = Arc::new(AtomicUsize::new(0));
    let handler = SSHFixture {
        authentications: authentications.clone(),
        reject,
        stop_before_remote_changes,
    };
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let config = config.clone();
            let handler = handler.clone();
            tokio::spawn(async move {
                if let Ok(session) = server::run_stream(config, stream, handler).await {
                    let _ = session.await;
                }
            });
        }
    });
    (state, node, authentications, task)
}

async fn start(state: &AppState, node: &str) -> String {
    let mut tx = db::begin_write(&state.pool).await.unwrap();
    let id = crate::api::enqueue_job_in_tx(&mut tx, "ssh-test", Some(node), Some(1))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    assert_eq!(job.id, id);
    execute_one(state.clone(), job).await;
    id
}
async fn row(state: &AppState, id: &str) -> sqlx::postgres::PgRow {
    sqlx::query("SELECT * FROM jobs WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn confirm(
    state: &AppState,
    id: &str,
    revision: i64,
    fingerprint: &str,
) -> Result<(), crate::error::ApiError> {
    crate::api::ssh_fingerprint::confirm(
        State(state.clone()),
        Path(id.into()),
        Json(
            serde_json::from_value(json!({"expected_revision":revision,"fingerprint":fingerprint}))
                .unwrap(),
        ),
    )
    .await
    .map(|_| ())
}
fn fingerprint(row: &sqlx::postgres::PgRow) -> String {
    row.get::<serde_json::Value, _>("result_json")["result"]["fingerprint"]
        .as_str()
        .unwrap()
        .into()
}

#[tokio::test]
async fn ssh_confirmation_resumes_same_task_after_restart_without_authenticating_before_trust() {
    for changed in [false, true] {
        let (state, node, auth, server) = fixture(false).await;
        if changed {
            sqlx::query("UPDATE nodes SET ssh_host_fingerprint='SHA256:old' WHERE id=$1")
                .bind(&node)
                .execute(&state.pool)
                .await
                .unwrap();
        }
        let id = start(&state, &node).await;
        let waiting = row(&state, &id).await;
        assert_eq!(waiting.get::<String, _>("status"), "waiting_confirmation");
        assert!(
            waiting
                .get::<Option<chrono::DateTime<chrono::Utc>>, _>("finished_at")
                .is_none()
        );
        assert_eq!(auth.load(Ordering::SeqCst), 0);
        assert!(claim_next(&state.pool).await.unwrap().is_none());
        recover_interrupted(&state.pool).await.unwrap();
        assert_eq!(
            row(&state, &id).await.get::<String, _>("status"),
            "waiting_confirmation"
        );
        let fp = fingerprint(&waiting);
        confirm(&state, &id, 1, &fp).await.unwrap();
        assert!(confirm(&state, &id, 1, &fp).await.is_err());
        let continued = claim_next(&state.pool).await.unwrap().unwrap();
        assert_eq!(continued.id, id);
        assert_eq!(continued.attempts, 1, "confirmation is not a retry");
        execute_one(state.clone(), continued).await;
        let finished = row(&state, &id).await;
        assert_eq!(finished.get::<String, _>("status"), "succeeded");
        assert_eq!(finished.get::<String, _>("stage"), "environment_checked");
        assert_eq!(auth.load(Ordering::SeqCst), 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE node_id=$1")
                .bind(&node)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            1
        );
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM job_events WHERE job_id=$1 AND event_type='job.fingerprint_confirmed'").bind(&id).fetch_one(&state.pool).await.unwrap(),1);
        server.abort();
    }
}

#[tokio::test]
async fn ssh_confirmation_rejects_stale_or_wrong_trust_and_saves_atomically() {
    let (state, node, _, server) = fixture(false).await;
    let id = start(&state, &node).await;
    let fp = fingerprint(&row(&state, &id).await);
    assert!(confirm(&state, &id, 1, "SHA256:wrong").await.is_err());
    assert!(confirm(&state, &id, 2, &fp).await.is_err());
    sqlx::query("UPDATE nodes SET desired_revision=2,ssh_host='other.invalid' WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(confirm(&state, &id, 2, &fp).await.is_err());
    assert!(
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT ssh_host_fingerprint FROM nodes WHERE id=$1"
        )
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(
        row(&state, &id).await.get::<String, _>("status"),
        "waiting_confirmation"
    );
    server.abort();
}

#[tokio::test]
async fn ssh_confirmation_authentication_failure_stays_on_original_task() {
    let (state, node, auth, server) = fixture(true).await;
    let id = start(&state, &node).await;
    let fp = fingerprint(&row(&state, &id).await);
    confirm(&state, &id, 1, &fp).await.unwrap();
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    execute_one(state.clone(), job).await;
    assert_eq!(row(&state, &id).await.get::<String, _>("status"), "failed");
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "unreachable"
    );
    assert!(auth.load(Ordering::SeqCst) > 0);
    assert!(claim_next(&state.pool).await.unwrap().is_none());
    server.abort();
}

#[tokio::test]
async fn ssh_confirmation_new_test_and_node_removal_cancel_waiting_task() {
    let (state, node, _, server) = fixture(false).await;
    let first = start(&state, &node).await;
    let second = start(&state, &node).await;
    assert_eq!(
        row(&state, &first).await.get::<String, _>("status"),
        "cancelled"
    );
    crate::api::nodes::remove_record(
        State(state.clone()),
        Path(node),
        axum::extract::Query(serde_json::from_value(json!({"expected_revision":1})).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(
        row(&state, &second).await.get::<String, _>("status"),
        "cancelled"
    );
    server.abort();
}

#[tokio::test]
async fn ssh_confirmation_failed_event_rolls_back_trust_and_resumption() {
    let (state, node, _, server) = fixture(false).await;
    let id = start(&state, &node).await;
    let fp = fingerprint(&row(&state, &id).await);
    sqlx::raw_sql("CREATE FUNCTION reject_confirmation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.fingerprint_confirmed' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_confirmation BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_confirmation();").execute(&state.pool).await.unwrap();
    assert!(confirm(&state, &id, 1, &fp).await.is_err());
    let node_row =
        sqlx::query("SELECT desired_revision,ssh_host_fingerprint FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(node_row.get::<i64, _>("desired_revision"), 1);
    assert!(
        node_row
            .get::<Option<String>, _>("ssh_host_fingerprint")
            .is_none()
    );
    assert_eq!(
        row(&state, &id).await.get::<String, _>("status"),
        "waiting_confirmation"
    );
    assert!(claim_next(&state.pool).await.unwrap().is_none());
    sqlx::query("DROP TRIGGER reject_confirmation ON job_events")
        .execute(&state.pool)
        .await
        .unwrap();
    confirm(&state, &id, 1, &fp).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn ssh_confirmation_node_edit_cancels_waiting_and_post_confirmation_edit_cannot_authenticate()
{
    let (state, node, auth, server) = fixture(false).await;
    let id = start(&state, &node).await;
    let _ = crate::api::nodes::patch(
        State(state.clone()),
        Path(node.clone()),
        Json(serde_json::from_value(json!({"expected_revision":1,"name":"Edited node"})).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(
        row(&state, &id).await.get::<String, _>("status"),
        "cancelled"
    );
    let next = start(&state, &node).await;
    let fp = fingerprint(&row(&state, &next).await);
    confirm(&state, &next, 2, &fp).await.unwrap();
    sqlx::query("UPDATE nodes SET desired_revision=4 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    execute_one(state.clone(), job).await;
    assert_eq!(
        row(&state, &next).await.get::<String, _>("status"),
        "failed"
    );
    assert_eq!(auth.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn ssh_confirmation_late_worker_cannot_overwrite_an_edited_node() {
    let (state, node, _, server) = fixture(false).await;
    let id = start(&state, &node).await;
    let fp = fingerprint(&row(&state, &id).await);
    confirm(&state, &id, 1, &fp).await.unwrap();
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    let output = deployment::run_job(&state.pool, &state.secrets, &job)
        .await
        .unwrap();
    assert_eq!(output.stage, "environment_checked");
    let _ = crate::api::nodes::patch(
        State(state.clone()),
        Path(node.clone()),
        Json(
            serde_json::from_value(
                json!({"expected_revision":2,"name":"Edited during inspection"}),
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    succeed(&state, &job, output).await.unwrap();
    assert_eq!(
        row(&state, &id).await.get::<String, _>("status"),
        "cancelled"
    );
    server.abort();
}

#[tokio::test]
async fn first_trust_deploy_sync_and_rollback_resume_original_snapshot_and_task() {
    for kind in ["deploy", "sync", "rollback"] {
        let (state, node, auth, server) = fixture_with_preflight_stop(false, true).await;
        let revision = if kind == "rollback" {
            sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at,deployed_success) SELECT id || '-r' || r,node_id,r,config_enc,content_sha256,now(),r=2 FROM config_versions CROSS JOIN generate_series(2,3) r WHERE node_id=$1 AND revision=1")
                .bind(&node).execute(&state.pool).await.unwrap();
            sqlx::query("UPDATE nodes SET desired_revision=3,deployed_revision=2 WHERE id=$1")
                .bind(&node)
                .execute(&state.pool)
                .await
                .unwrap();
            3
        } else {
            1
        };
        let mut tx = db::begin_write(&state.pool).await.unwrap();
        let id = crate::api::enqueue_job_with_payload_in_tx(
            &mut tx,
            kind,
            Some(&node),
            Some(1),
            json!({"force":true}),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let job = claim_next(&state.pool).await.unwrap().unwrap();
        execute_one(state.clone(), job).await;
        let waiting = row(&state, &id).await;
        assert_eq!(
            waiting.get::<String, _>("status"),
            "waiting_confirmation",
            "{kind}"
        );
        assert_eq!(
            auth.load(Ordering::SeqCst),
            0,
            "no authentication before explicit trust"
        );
        recover_interrupted(&state.pool).await.unwrap();
        assert_eq!(
            row(&state, &id).await.get::<String, _>("status"),
            "waiting_confirmation"
        );
        confirm(&state, &id, revision, &fingerprint(&waiting))
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM nodes WHERE id=$1")
                .bind(&node)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            "syncing"
        );
        let resumed = claim_next(&state.pool).await.unwrap().unwrap();
        assert_eq!(resumed.id, id);
        assert_eq!(resumed.kind, kind);
        assert_eq!(resumed.attempts, 1);
        assert_eq!(resumed.payload["original_target_revision"], 1);
        assert_eq!(resumed.payload["force"], true);
        assert_eq!(
            resumed.target_revision,
            Some(if kind == "rollback" { 1 } else { 2 })
        );
        let original: String = sqlx::query_scalar(
            "SELECT config_enc FROM config_versions WHERE node_id=$1 AND revision=1",
        )
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        let continued: String = sqlx::query_scalar(
            "SELECT config_enc FROM config_versions WHERE node_id=$1 AND revision=$2",
        )
        .bind(&node)
        .bind(resumed.target_revision)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            state.secrets.decrypt(&original).unwrap(),
            state.secrets.decrypt(&continued).unwrap()
        );
        // Run the actual resumed connection and environment checks; the fixture
        // stops at the final read-only preflight before downloads or remote writes.
        execute_one(state.clone(), resumed).await;
        let stopped = row(&state, &id).await;
        assert_eq!(stopped.get::<String, _>("status"), "failed");
        assert!(
            stopped
                .get::<String, _>("error_message")
                .contains("fixture stopped before remote changes")
        );
        assert_eq!(auth.load(Ordering::SeqCst), 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE node_id=$1")
                .bind(&node)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            1
        );
        server.abort();
    }
}

#[tokio::test]
async fn deletion_failure_can_test_confirm_and_then_explicitly_retry_uninstall() {
    let (state, node, auth, server) = fixture(false).await;
    sqlx::query("UPDATE nodes SET state='delete_failed',deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let (_, receipt) = crate::api::nodes::ssh_test(
        State(state.clone()),
        Path(node.clone()),
        Json(serde_json::from_value(json!({"expected_revision":1})).unwrap()),
    )
    .await
    .unwrap();
    let id = receipt.0["job_id"].as_str().unwrap().to_owned();
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    execute_one(state.clone(), job).await;
    let waiting = row(&state, &id).await;
    assert_eq!(waiting.get::<String, _>("status"), "waiting_confirmation");
    assert_eq!(auth.load(Ordering::SeqCst), 0);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "delete_failed"
    );
    confirm(&state, &id, 1, &fingerprint(&waiting))
        .await
        .unwrap();
    assert!(
        crate::api::nodes::sync(
            State(state.clone()),
            Path(node.clone()),
            Json(serde_json::from_value(json!({"expected_revision":2})).unwrap())
        )
        .await
        .is_err()
    );
    let continued = claim_next(&state.pool).await.unwrap().unwrap();
    assert_eq!(continued.id, id);
    execute_one(state.clone(), continued).await;
    assert_eq!(
        row(&state, &id).await.get::<String, _>("status"),
        "succeeded"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "delete_failed"
    );
    assert!(
        claim_next(&state.pool).await.unwrap().is_none(),
        "successful diagnosis must not resume destructive work"
    );
    let (status, _) = crate::api::nodes::delete(
        State(state.clone()),
        Path(node.clone()),
        axum::extract::Query(serde_json::from_value(json!({"expected_revision":2})).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(status, axum::http::StatusCode::ACCEPTED);
    assert!(
        crate::api::nodes::ssh_test(
            State(state.clone()),
            Path(node.clone()),
            Json(serde_json::from_value(json!({"expected_revision":2})).unwrap())
        )
        .await
        .is_err()
    );
    assert_eq!(
        claim_next(&state.pool).await.unwrap().unwrap().kind,
        "uninstall"
    );
    server.abort();
}

#[tokio::test]
async fn deletion_recovery_failed_ssh_test_can_use_linked_retry() {
    let (state, node, _, server) = fixture(true).await;
    sqlx::query("UPDATE nodes SET state='delete_failed',deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let (_, receipt) = crate::api::nodes::ssh_test(
        State(state.clone()),
        Path(node.clone()),
        Json(serde_json::from_value(json!({"expected_revision":1})).unwrap()),
    )
    .await
    .unwrap();
    let id = receipt.0["job_id"].as_str().unwrap();
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    execute_one(state.clone(), job).await;
    confirm(&state, id, 1, &fingerprint(&row(&state, id).await))
        .await
        .unwrap();
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    execute_one(state.clone(), job).await;
    assert_eq!(row(&state, id).await.get::<String, _>("status"), "failed");
    let (_, retry) = crate::api::job_retries::retry(
        State(state.clone()),
        Path(id.into()),
        Json(crate::api::job_retries::RetryRequest {
            expected_revision: 2,
        }),
    )
    .await
    .unwrap();
    let retried = claim_next(&state.pool).await.unwrap().unwrap();
    assert_eq!(retried.id, retry.0["job_id"].as_str().unwrap());
    assert_eq!(retried.payload["delete_recovery"], true);
    server.abort();
}

#[tokio::test]
async fn deletion_start_during_ssh_diagnosis_cannot_be_overwritten_by_completion() {
    let (state, node, _, server) = fixture(false).await;
    sqlx::query("UPDATE nodes SET state='delete_failed',deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let (_, receipt) = crate::api::nodes::ssh_test(
        State(state.clone()),
        Path(node.clone()),
        Json(serde_json::from_value(json!({"expected_revision":1})).unwrap()),
    )
    .await
    .unwrap();
    let id = receipt.0["job_id"].as_str().unwrap();
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    execute_one(state.clone(), job).await;
    confirm(&state, id, 1, &fingerprint(&row(&state, id).await))
        .await
        .unwrap();
    let job = claim_next(&state.pool).await.unwrap().unwrap();
    let output = deployment::run_job(&state.pool, &state.secrets, &job)
        .await
        .unwrap();
    let _ = crate::api::nodes::delete(
        State(state.clone()),
        Path(node.clone()),
        axum::extract::Query(serde_json::from_value(json!({"expected_revision":2})).unwrap()),
    )
    .await
    .unwrap();
    succeed(&state, &job, output).await.unwrap();
    assert_eq!(
        row(&state, id).await.get::<String, _>("status"),
        "cancelled"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "deleting"
    );
    server.abort();
}

#[tokio::test]
async fn ssh_test_preserves_deployment_state_directly_and_through_fingerprint_confirmation() {
    for expected in [
        "deployed",
        "rolled_back",
        "sync_failed",
        "rollback_failed",
        "drift",
        "syncing",
    ] {
        let (state, node, _, server) = fixture(false).await;
        sqlx::query("UPDATE nodes SET state=$1,deployed_revision=1 WHERE id=$2")
            .bind(expected)
            .bind(&node)
            .execute(&state.pool)
            .await
            .unwrap();
        let id = start(&state, &node).await;
        let waiting = row(&state, &id).await;
        assert_eq!(waiting.get::<String, _>("status"), "waiting_confirmation");
        assert_eq!(
            waiting.get::<Value, _>("result_json")["result"]["ssh_restore_state"],
            expected
        );
        recover_interrupted(&state.pool).await.unwrap();
        confirm(&state, &id, 1, &fingerprint(&waiting))
            .await
            .unwrap();
        let resumed = claim_next(&state.pool).await.unwrap().unwrap();
        execute_one(state.clone(), resumed).await;
        let check = sqlx::query("SELECT state,deployed_revision FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(check.get::<String, _>("state"), expected);
        assert_eq!(check.get::<Option<i64>, _>("deployed_revision"), Some(1));
        assert_eq!(
            row(&state, &id).await.get::<String, _>("status"),
            "succeeded"
        );
        let direct = start(&state, &node).await;
        assert_eq!(
            row(&state, &direct).await.get::<String, _>("status"),
            "succeeded"
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM nodes WHERE id=$1")
                .bind(&node)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            expected
        );
        server.abort();
    }
}

#[tokio::test]
async fn ssh_test_recovers_deployed_nodes_from_connectivity_warnings_or_previous_ready_bug() {
    for original in ["ready", "unreachable", "fingerprint_changed"] {
        let (state, node, _, server) = fixture(false).await;
        sqlx::query("UPDATE nodes SET state=$1,deployed_revision=1 WHERE id=$2")
            .bind(original)
            .bind(&node)
            .execute(&state.pool)
            .await
            .unwrap();
        let id = start(&state, &node).await;
        confirm(&state, &id, 1, &fingerprint(&row(&state, &id).await))
            .await
            .unwrap();
        let resumed = claim_next(&state.pool).await.unwrap().unwrap();
        execute_one(state.clone(), resumed).await;
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM nodes WHERE id=$1")
                .bind(&node)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            "deployed"
        );
        server.abort();
    }
}

#[tokio::test]
async fn failed_ssh_auth_after_confirmation_preserves_existing_deployment_warnings() {
    for expected in ["deployed", "sync_failed"] {
        let (state, node, _, server) = fixture(true).await;
        sqlx::query("UPDATE nodes SET state=$1,deployed_revision=1 WHERE id=$2")
            .bind(expected)
            .bind(&node)
            .execute(&state.pool)
            .await
            .unwrap();
        let id = start(&state, &node).await;
        confirm(&state, &id, 1, &fingerprint(&row(&state, &id).await))
            .await
            .unwrap();
        let resumed = claim_next(&state.pool).await.unwrap().unwrap();
        execute_one(state.clone(), resumed).await;
        assert_eq!(row(&state, &id).await.get::<String, _>("status"), "failed");
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM nodes WHERE id=$1")
                .bind(&node)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            expected
        );
        server.abort();
    }
}
