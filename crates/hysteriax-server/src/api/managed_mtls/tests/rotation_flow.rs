use super::*;
use super::{authorization_flow::group, node_flow::managed_node};
use crate::deployment::JobInput;
use credentials::{
    activation,
    node_mtls::{self, Settings, TrustVersion},
    rotations,
};

async fn snapshot(state: &AppState, node: &str, revision: i64) -> Value {
    let cipher: String = sqlx::query_scalar(
        "SELECT config_enc FROM config_versions WHERE node_id=$1 AND revision=$2",
    )
    .bind(node)
    .bind(revision)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    serde_json::from_str(&state.secrets.decrypt(&cipher).unwrap()).unwrap()
}
async fn job_input(state: &AppState, id: &str) -> JobInput {
    let row = sqlx::query("SELECT * FROM jobs WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    JobInput {
        id: id.into(),
        kind: row.get("kind"),
        node_id: row.get("node_id"),
        target_revision: row.get("target_revision"),
        payload: row.get("payload_json"),
        attempts: 1,
    }
}
// Inject a verified remote result through the actual activation publication path.
// These are state-machine tests; fixed binary/SSH validation remains separate.
async fn initial_deployment(state: &AppState, node: &Value, ca: &str) {
    let id = node["id"].as_str().unwrap();
    let job = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,available_at,created_at,updated_at) VALUES($1,'deploy',$2,1,'running','starting',now(),now(),now())").bind(&job).bind(id).execute(&state.pool).await.unwrap();
    let snapshot = snapshot(state, id, 1).await;
    let trust = node_mtls::desired_trust(
        &state.pool,
        id,
        &Settings::from_config(&node["config"]).unwrap(),
    )
    .await
    .unwrap();
    let probe = credentials::maintenance::renew_probe_on(&state.pool, &state.secrets, id, ca, 1)
        .await
        .unwrap();
    finish(state, &job, &snapshot, &trust, &probe).await;
}
async fn finish(
    state: &AppState,
    job: &str,
    snapshot: &Value,
    trust: &[TrustVersion],
    probe: &(String, i64),
) {
    let input = job_input(state, job).await;
    let node = input.node_id.as_deref().unwrap();
    let revision = input.target_revision.unwrap();
    let digest = format!("verified-{job}");
    let _ = activation::begin(
        &state.pool,
        &state.secrets,
        activation::Begin {
            node,
            job,
            revision,
            digest: &digest,
            snapshot,
            trust,
            probe: Some((&probe.0, probe.1)),
        },
    )
    .await
    .unwrap();
    activation::verified(&state.pool, job).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    activation::commit(&mut tx, job, &digest).await.unwrap();
    sqlx::query("UPDATE nodes SET deployed_revision=$2,deployed_config_enc=$3,deployed_content_sha256=$4,state='deployed' WHERE id=$1").bind(node).bind(revision).bind(state.secrets.encrypt(&snapshot["server_config"].to_string()).unwrap()).bind(digest).execute(&mut *tx).await.unwrap();
    sqlx::query(
        "UPDATE config_versions SET deployed_success=TRUE WHERE node_id=$1 AND revision=$2",
    )
    .bind(node)
    .bind(revision)
    .execute(&mut *tx)
    .await
    .unwrap();
    rotations::operation_completed(&mut tx, &input)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE jobs SET status='succeeded',stage='health_checked',finished_at=now() WHERE id=$1",
    )
    .bind(job)
    .execute(&mut *tx)
    .await
    .unwrap();
    credentials::reference_index::flush_node_on(&mut tx, &state.secrets, node)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}
async fn complete_operation(state: &AppState, job: &str) {
    sqlx::query("UPDATE jobs SET status='running' WHERE id=$1")
        .bind(job)
        .execute(&state.pool)
        .await
        .unwrap();
    let input = job_input(state, job).await;
    let node = input.node_id.as_deref().unwrap();
    let mut snapshot = snapshot(state, node, input.target_revision.unwrap()).await;
    let context = rotations::configure_job(&state.pool, &state.secrets, &input, &mut snapshot)
        .await
        .unwrap()
        .unwrap();
    let ca = snapshot["server_config"]["mtls"]["ca_credential_id"]
        .as_str()
        .unwrap();
    let probe=if let Some(row)=sqlx::query_as::<_,(String,i64)>("SELECT credential_id,credential_version FROM mtls_probe_versions WHERE node_id=$1 AND ca_id=$2 AND ca_version=$3 AND expires_at>now() AND NOT discarded ORDER BY credential_version DESC LIMIT 1").bind(node).bind(ca).bind(context.probe_version).fetch_optional(&state.pool).await.unwrap(){row}else{credentials::maintenance::renew_probe_on(&state.pool,&state.secrets,node,ca,context.probe_version).await.unwrap()};
    finish(state, job, &snapshot, &context.trust, &probe).await;
}
async fn rotation_job(state: &AppState, rotation: &str, node: &str) -> String {
    sqlx::query_scalar("SELECT job_id FROM mtls_rotation_nodes WHERE rotation_id=$1 AND node_id=$2")
        .bind(rotation)
        .bind(node)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn published(state: &AppState, ca: &str) -> i64 {
    sqlx::query_scalar("SELECT published_version FROM mtls_authorities WHERE credential_id=$1")
        .bind(ca)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn phase(state: &AppState, rotation: &str) -> String {
    sqlx::query_scalar("SELECT phase FROM mtls_rotations WHERE id=$1")
        .bind(rotation)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn leased_job_completion_atomically_publishes_verified_activation_and_rotation_proof_after_native_retry()
 {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap();
    initial_deployment(&state, &node, &ca).await;
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "leased-completion")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    let job = rotation_job(&state, rotation, id).await;
    sqlx::query("UPDATE jobs SET status='running',attempts=1 WHERE id=$1")
        .bind(&job)
        .execute(&state.pool)
        .await
        .unwrap();
    let input = job_input(&state, &job).await;
    let mut snapshot = snapshot(&state, id, input.target_revision.unwrap()).await;
    let context = rotations::configure_job(&state.pool, &state.secrets, &input, &mut snapshot)
        .await
        .unwrap()
        .unwrap();
    let probe = credentials::maintenance::renew_probe_on(
        &state.pool,
        &state.secrets,
        id,
        &ca,
        context.probe_version,
    )
    .await
    .unwrap();
    let digest = "verified-leased-completion";
    let active = activation::begin(
        &state.pool,
        &state.secrets,
        activation::Begin {
            node: id,
            job: &job,
            revision: input.target_revision.unwrap(),
            digest,
            snapshot: &snapshot,
            trust: &context.trust,
            probe: Some((&probe.0, probe.1)),
        },
    )
    .await
    .unwrap();
    activation::verified(&state.pool, &job).await.unwrap();
    let cipher = state
        .secrets
        .encrypt(&snapshot["server_config"].to_string())
        .unwrap();
    let output = crate::deployment::JobOutput {
        stage: "health_checked".into(),
        result: activation::marker(&active),
        node_state: Some("deployed".into()),
        deployed_revision: input.target_revision,
        deployed_config: Some(cipher.clone()),
        deployed_sha256: Some(digest.into()),
        published_connection: None,
        delete_node: false,
    };
    let mut tx = state.pool.begin().await.unwrap();
    let mut leases = vec![];
    for resource in [format!("job:{job}"), format!("node:{id}")] {
        leases.push(
            crate::execution::acquire(&mut tx, &resource, &state.instance_id, &job)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    sqlx::raw_sql("CREATE SEQUENCE managed_completion_attempts; CREATE FUNCTION retry_managed_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.succeeded' AND nextval('managed_completion_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40P01',MESSAGE='retry verified activation'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_managed_completion BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION retry_managed_completion();").execute(&state.pool).await.unwrap();
    crate::execution::supervise(
        state.pool.clone(),
        leases.clone(),
        crate::jobs::succeed(&state, &input, output),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM managed_completion_attempts")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
    assert!(sqlx::query_scalar::<_,bool>("SELECT j.status='succeeded' AND a.phase='committed' AND n.mtls_state='ready' AND n.deployed_config_enc=$2 AND n.mtls_activation_id=a.id AND r.trust_prepared FROM jobs j JOIN mtls_activations a ON a.job_id=j.id JOIN nodes n ON n.id=j.node_id JOIN mtls_rotation_nodes r ON r.job_id=j.id WHERE j.id=$1").bind(&job).bind(&cipher).fetch_one(&state.pool).await.unwrap());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM job_events WHERE job_id=$1 AND event_type='job.succeeded'"
        )
        .bind(&job)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        1
    );
    let mut tx = state.pool.begin().await.unwrap();
    for lease in &leases {
        assert!(lease.release(&mut tx).await.unwrap());
    }
    tx.commit().await.unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 2);
}

#[tokio::test]
async fn every_node_must_prove_new_ca_before_one_shared_publication_and_probe_switch() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let n1 = managed_node(&state, &ca).await;
    let n2 = managed_node(&state, &ca).await;
    let a = n1["id"].as_str().unwrap();
    let b = n2["id"].as_str().unwrap();
    let _ = group(&state, "Both", &["alice"], &[a, b]).await;
    initial_deployment(&state, &n1, &ca).await;
    initial_deployment(&state, &n2, &ca).await;
    let old = credentials::identities::for_node(&state.pool, &state.secrets, "alice", a, &ca)
        .await
        .unwrap()
        .unwrap();
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM users WHERE id='alice'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "rotation-two-nodes")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    let first = rotation_job(&state, rotation, a).await;
    complete_operation(&state, &first).await;
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 1);
    let staged: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mtls_user_versions WHERE ca_version=2")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(staged, 0);
    let active: i64 =
        sqlx::query_scalar("SELECT ca_version FROM mtls_active_probes WHERE node_id=$1")
            .bind(a)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(active, 1);
    let second = rotation_job(&state, rotation, b).await;
    complete_operation(&state, &second).await;
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 2);
    assert_eq!(phase(&state, rotation).await, "probes");
    let versions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mtls_user_version_publication WHERE ca_version=2 AND released",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(versions, 1);
    let new_a = credentials::identities::for_node(&state.pool, &state.secrets, "alice", a, &ca)
        .await
        .unwrap()
        .unwrap();
    let new_b = credentials::identities::for_node(&state.pool, &state.secrets, "alice", b, &ca)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(new_a, new_b);
    assert_ne!(new_a, old);
    let after: i64 = sqlx::query_scalar("SELECT revision FROM users WHERE id='alice'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(revision, after);
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(phase(&state, rotation).await, "complete");
    let probes: Vec<i64> =
        sqlx::query_scalar("SELECT ca_version FROM mtls_active_probes ORDER BY node_id")
            .fetch_all(&state.pool)
            .await
            .unwrap();
    assert_eq!(probes, vec![2, 2]);
    let deadline: chrono::DateTime<Utc> = sqlx::query_scalar(
        "SELECT next_action_at FROM mtls_trust_cleanup WHERE ca_id=$1 AND ca_version=1",
    )
    .bind(&ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(deadline > Utc::now() + Duration::days(179));
    let kicks: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind='kick'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(kicks, 0);
}

#[tokio::test]
async fn offline_node_blocks_publication_and_batch_retry_preserves_already_prepared_nodes() {
    let state = state().await;
    let ca = generated(&state).await;
    let n1 = managed_node(&state, &ca).await;
    let n2 = managed_node(&state, &ca).await;
    let a = n1["id"].as_str().unwrap();
    let b = n2["id"].as_str().unwrap();
    initial_deployment(&state, &n1, &ca).await;
    initial_deployment(&state, &n2, &ca).await;
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "offline-rotation")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    let first = rotation_job(&state, rotation, a).await;
    complete_operation(&state, &first).await;
    let failed = rotation_job(&state, rotation, b).await;
    sqlx::query("UPDATE jobs SET status='failed',error_message='offline' WHERE id=$1")
        .bind(&failed)
        .execute(&state.pool)
        .await
        .unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 1);
    assert_eq!(rotation_job(&state, rotation, b).await, failed);
    let Json(batch) = api::batch(
        State(state.clone()),
        Path(receipt["batch_id"].as_str().unwrap().into()),
    )
    .await
    .unwrap();
    let blocked = batch["ca_rotation"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["node_id"] == b)
        .unwrap();
    assert_eq!(blocked["error_message"], "offline");
    assert_eq!(blocked["status"], "failed");
    let _ = api::retry(
        State(state.clone()),
        actor(),
        Path(receipt["batch_id"].as_str().unwrap().into()),
    )
    .await
    .unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    let retry = rotation_job(&state, rotation, b).await;
    assert_ne!(retry, failed);
    assert_eq!(rotation_job(&state, rotation, a).await, first);
    complete_operation(&state, &retry).await;
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 2);
}

#[tokio::test]
async fn continuous_publications_do_not_wait_for_old_certificates_and_cleanup_preserves_newer_trust()
 {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let _ = group(&state, "One", &["alice"], &[n]).await;
    initial_deployment(&state, &node, &ca).await;
    for current in 1..=2 {
        let (_, Json(receipt)) = publish(
            State(state.clone()),
            actor(),
            Path(ca.clone()),
            Json(publish_request(current, &format!("continuous-{current}"))),
        )
        .await
        .unwrap();
        let rotation = receipt["rotation_id"].as_str().unwrap();
        rotations::advance(&state, rotation).await.unwrap();
        let job = rotation_job(&state, rotation, n).await;
        complete_operation(&state, &job).await;
        // Metadata and probe switches are separately claimed pages. Their
        // durable retry time can require another pass even for one node.
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                rotations::advance(&state, rotation).await.unwrap();
                if phase(&state, rotation).await == "complete" {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("published rotation must finish metadata and probe pages");
    }
    assert_eq!(published(&state, &ca).await, 3);
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mtls_trust_cleanup WHERE ca_id=$1 AND completed_at IS NULL",
    )
    .bind(&ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(pending, 2);
    // Inject passage beyond only generation 1's leaf deadline. Newer generations remain valid.
    sqlx::query("UPDATE mtls_user_versions SET expires_at=now()-interval '6 minutes',not_before=now()-interval '2 days' WHERE ca_id=$1 AND ca_version=1").bind(&ca).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE mtls_probe_versions SET expires_at=now()-interval '6 minutes' WHERE ca_id=$1 AND ca_version=1").bind(&ca).execute(&state.pool).await.unwrap();
    sqlx::query(
        "UPDATE mtls_trust_cleanup SET next_action_at=now() WHERE ca_id=$1 AND ca_version=1",
    )
    .bind(&ca)
    .execute(&state.pool)
    .await
    .unwrap();
    rotations::run_pass(&state).await.unwrap();
    let cleanup: String = sqlx::query_scalar(
        "SELECT job_id FROM mtls_cleanup_nodes WHERE ca_id=$1 AND ca_version=1 AND node_id=$2",
    )
    .bind(&ca)
    .bind(n)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let (_, Json(next)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(3, "newer-than-cleanup")),
    )
    .await
    .unwrap();
    complete_operation(&state, &cleanup).await;
    let trust: Vec<i64> = sqlx::query_scalar(
        "SELECT ca_version FROM mtls_node_trust WHERE node_id=$1 AND deployed ORDER BY ca_version",
    )
    .bind(n)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(trust, vec![2, 3, 4]);
    // Advance the durable cleanup schedule as well as the retained-leaf time.
    // The recurring worker deliberately waits between pages and acknowledgments.
    sqlx::query("UPDATE mtls_trust_cleanup SET next_action_at=clock_timestamp() WHERE ca_id=$1 AND ca_version=1").bind(&ca).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp() WHERE kind='mtls-trust-cleanup-page' AND payload->>'ca_id'=$1 AND payload->>'ca_version'='1' AND state='queued'").bind(&ca).execute(&state.pool).await.unwrap();
    rotations::run_pass(&state).await.unwrap();
    let completed: bool = sqlx::query_scalar(
        "SELECT completed_at IS NOT NULL FROM mtls_trust_cleanup WHERE ca_id=$1 AND ca_version=1",
    )
    .bind(&ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(completed);
    assert_eq!(published(&state, &ca).await, 3);
    let rotation = next["rotation_id"].as_str().unwrap();
    assert_eq!(phase(&state, rotation).await, "trust");
    assert!(
        credentials::identities::prepare_user(&state.pool, &state.secrets, "alice", &ca, 1, false)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("retired")
    );
    assert!(
        credentials::maintenance::renew_probe_on(&state.pool, &state.secrets, n, &ca, 1)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("retired")
    );
}

#[tokio::test]
async fn newly_bound_nodes_join_the_barrier_and_target_removal_does_not_hide_deployed_nodes() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    initial_deployment(&state, &node, &ca).await;
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "dynamic-members")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    complete_operation(&state, &rotation_job(&state, rotation, n).await).await;
    let added = managed_node(&state, &ca).await;
    let extra = added["id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 1);
    assert_eq!(
        rotations::members_on(&mut state.pool.acquire().await.unwrap(), &ca)
            .await
            .unwrap()
            .len(),
        2
    );
    let mut options = node["config"].clone();
    options["mtls"] = json!({"mode":"off"});
    let _ = crate::api::nodes::patch(
        State(state.clone()),
        Path(n.into()),
        Json(serde_json::from_value(json!({"expected_revision":1,"config":options})).unwrap()),
    )
    .await
    .unwrap();
    assert!(
        rotations::members_on(&mut state.pool.acquire().await.unwrap(), &ca)
            .await
            .unwrap()
            .contains(&n.to_owned())
    );
    // An undeployed new node may leave after an explicit off configuration; no remote CA ever applied.
    let mut options = added["config"].clone();
    options["mtls"] = json!({"mode":"off"});
    let _ = crate::api::nodes::patch(
        State(state.clone()),
        Path(extra.into()),
        Json(serde_json::from_value(json!({"expected_revision":1,"config":options})).unwrap()),
    )
    .await
    .unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 2);
}

#[tokio::test]
async fn task_retry_keeps_rotation_phase_and_uses_a_new_persisted_node_operation() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    initial_deployment(&state, &node, &ca).await;
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "task-retry")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    let original = rotation_job(&state, rotation, n).await;
    sqlx::query("UPDATE jobs SET status='failed' WHERE id=$1")
        .bind(&original)
        .execute(&state.pool)
        .await
        .unwrap();
    let (_, Json(retry)) = crate::api::job_retries::retry(
        State(state.clone()),
        Path(original.clone()),
        Json(crate::api::job_retries::RetryRequest {
            expected_revision: 1,
        }),
    )
    .await
    .unwrap();
    let child = retry["job_id"].as_str().unwrap();
    assert_ne!(child, original);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mtls_node_operations WHERE job_id=$1")
            .bind(child)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    complete_operation(&state, child).await;
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 2);
}

#[tokio::test]
async fn cleanup_deadline_includes_archived_longer_history_not_only_latest_certificate() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let _ = update_policy(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(UpdatePolicy {
            expected_revision: 1,
            policy: Policy {
                client_validity_days: 365,
                renew_before_hours: 720,
            },
            idempotency_key: "history-long".into(),
        }),
    )
    .await
    .unwrap();
    let grant = group(&state, "History", &["alice"], &[n]).await;
    initial_deployment(&state, &node, &ca).await;
    let _ = update_policy(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(UpdatePolicy {
            expected_revision: 2,
            policy: Policy {
                client_validity_days: 180,
                renew_before_hours: 720,
            },
            idempotency_key: "history-short".into(),
        }),
    )
    .await
    .unwrap();
    issue(&state, "alice", &ca, true).await;
    let id = grant["group"]["id"].as_str().unwrap();
    let Json(p) = crate::api::authorization_groups::preview_group(
        State(state.clone()),
        Path(id.into()),
        Json(serde_json::from_value(json!({"action":"delete","expected_revision":1})).unwrap()),
    )
    .await
    .unwrap();
    let _ = crate::api::authorization_groups::delete(
        State(state.clone()),
        Path(id.into()),
        Json(
            serde_json::from_value(
                json!({"expected_revision":1,"preview_token":p["preview_token"]}),
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "history-rotation")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    complete_operation(&state, &rotation_job(&state, rotation, n).await).await;
    rotations::advance(&state, rotation).await.unwrap();
    let deadline: chrono::DateTime<Utc> = sqlx::query_scalar(
        "SELECT next_action_at FROM mtls_trust_cleanup WHERE ca_id=$1 AND ca_version=1",
    )
    .bind(&ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(deadline > Utc::now() + Duration::days(364));
    let latest:chrono::DateTime<Utc>=sqlx::query_scalar("SELECT expires_at FROM mtls_user_versions WHERE ca_id=$1 ORDER BY credential_version DESC LIMIT 1").bind(&ca).fetch_one(&state.pool).await.unwrap();
    assert!(deadline > latest + Duration::days(180));
}

#[tokio::test]
async fn resumed_and_new_users_are_included_before_final_generation_publication() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let mut users = Vec::new();
    for index in 0..101 {
        let name = format!("user-{index:03}");
        user(&state, &name, true).await;
        users.push(name);
    }
    user(&state, "resumed", false).await;
    users.push("resumed".into());
    let refs: Vec<&str> = users.iter().map(String::as_str).collect();
    let _ = group(&state, "Many", &refs, &[n]).await;
    initial_deployment(&state, &node, &ca).await;
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "paged-users")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    complete_operation(&state, &rotation_job(&state, rotation, n).await).await;
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 1);
    let prepared: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mtls_user_versions WHERE ca_version=2")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(prepared, 100);
    sqlx::query("UPDATE users SET enabled=TRUE,revision=revision+1 WHERE id='resumed'")
        .execute(&state.pool)
        .await
        .unwrap();
    user(&state, "late", true).await;
    let _ = group(&state, "Late", &["late"], &[n]).await;
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 2);
    let prepared: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mtls_user_version_publication WHERE ca_version=2 AND released",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(prepared, 103);
    let status = rotations::status_on(&mut state.pool.acquire().await.unwrap(), rotation)
        .await
        .unwrap();
    assert_eq!(status["prepared_user_count"], 103);
    assert_eq!(status["eligible_user_count"], 103);
}

#[tokio::test]
async fn explicit_replacement_retires_unpublished_target_and_old_tasks_cannot_run() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    initial_deployment(&state, &node, &ca).await;
    let (_, Json(first)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "replace-old")),
    )
    .await
    .unwrap();
    let old = first["rotation_id"].as_str().unwrap();
    rotations::advance(&state, old).await.unwrap();
    let old_job = rotation_job(&state, old, n).await;
    let mut request = publish_request(2, "replace-new");
    request.replace_rotation_id = Some(old.into());
    let (_, Json(next)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(request),
    )
    .await
    .unwrap();
    assert_eq!(next["version"], 3);
    assert_eq!(published(&state, &ca).await, 1);
    let old_input = job_input(&state, &old_job).await;
    assert!(
        rotations::validate_job_on(&mut state.pool.acquire().await.unwrap(), &old_input)
            .await
            .is_err()
    );
    assert_eq!(phase(&state, old).await, "complete");
    let retired: String =
        sqlx::query_scalar("SELECT state FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=2")
            .bind(&ca)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(retired, "retired");
    let next_rotation = next["rotation_id"].as_str().unwrap();
    rotations::advance(&state, next_rotation).await.unwrap();
    complete_operation(&state, &rotation_job(&state, next_rotation, n).await).await;
    rotations::advance(&state, next_rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 3);
}

#[tokio::test]
async fn explicit_recovery_uses_the_known_pending_marker_even_if_old_issuer_expired() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let _ = group(&state, "Recover", &["alice"], &[n]).await;
    initial_deployment(&state, &node, &ca).await;
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "recovery-ca")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    let original = rotation_job(&state, rotation, n).await;
    let input = job_input(&state, &original).await;
    let mut snapshot = snapshot(&state, n, 1).await;
    let context = rotations::configure_job(&state.pool, &state.secrets, &input, &mut snapshot)
        .await
        .unwrap()
        .unwrap();
    let probe = credentials::maintenance::renew_probe_on(&state.pool, &state.secrets, n, &ca, 2)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET status='running' WHERE id=$1")
        .bind(&original)
        .execute(&state.pool)
        .await
        .unwrap();
    let record = activation::begin(
        &state.pool,
        &state.secrets,
        activation::Begin {
            node: n,
            job: &original,
            revision: 1,
            digest: "pending-remote-config",
            snapshot: &snapshot,
            trust: &context.trust,
            probe: Some((&probe.0, probe.1)),
        },
    )
    .await
    .unwrap();
    activation::uncertain(&state.pool, &original).await.unwrap();
    sqlx::query("UPDATE jobs SET status='failed' WHERE id=$1")
        .bind(&original)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_ca_generations SET expires_at=now()-interval '1 second' WHERE ca_id=$1 AND ca_version=1").bind(&ca).execute(&state.pool).await.unwrap();
    let (_, Json(recovery)) = crate::api::nodes::sync(
        State(state.clone()),
        Path(n.into()),
        Json(serde_json::from_value(json!({"expected_revision":1})).unwrap()),
    )
    .await
    .unwrap();
    let job = recovery["job_id"].as_str().unwrap();
    let expected = rotations::recovery_marker(&state.pool, job)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(expected["activation_id"], record.id);
    assert_eq!(expected["config_sha256"], "pending-remote-config");
    let recovery_input = job_input(&state, job).await;
    let mut target = snapshot.clone();
    let recovery =
        rotations::configure_job(&state.pool, &state.secrets, &recovery_input, &mut target)
            .await
            .unwrap()
            .unwrap();
    assert!(recovery.recovery && recovery.skip_users);
    complete_operation(&state, job).await;
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 2);
}

#[tokio::test]
async fn staged_short_lived_certificates_publish_even_after_entering_the_renewal_window() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let _ = update_policy(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(UpdatePolicy {
            expected_revision: 1,
            policy: Policy {
                client_validity_days: 1,
                renew_before_hours: 23,
            },
            idempotency_key: "short-policy".into(),
        }),
    )
    .await
    .unwrap();
    let mut users = Vec::new();
    for index in 0..101 {
        let id = format!("short-{index:03}");
        user(&state, &id, true).await;
        users.push(id);
    }
    let refs: Vec<&str> = users.iter().map(String::as_str).collect();
    let _ = group(&state, "Short certificates", &refs, &[n]).await;
    initial_deployment(&state, &node, &ca).await;
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "short-rotation")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    complete_operation(&state, &rotation_job(&state, rotation, n).await).await;
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 1);
    // Simulate a slow signing batch: the first page is still valid but is due for renewal.
    sqlx::query("UPDATE mtls_user_versions SET next_action_at=now()-interval '1 minute' WHERE ca_id=$1 AND ca_version=2").bind(&ca).execute(&state.pool).await.unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 2);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mtls_user_versions WHERE ca_id=$1 AND ca_version=2",
    )
    .bind(&ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(count, 101);
    let mut connection = state.pool.acquire().await.unwrap();
    assert!(
        rotations::maintenance_notifications_on(&mut connection)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        rotations::notification_credentials_on(&mut connection)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn published_target_can_only_be_superseded_explicitly_after_issuer_expiry() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    initial_deployment(&state, &node, &ca).await;
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "published-old")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    complete_operation(&state, &rotation_job(&state, rotation, n).await).await;
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(phase(&state, rotation).await, "probes");
    let replace = || {
        let mut request = publish_request(2, "published-replace");
        request.replace_rotation_id = Some(rotation.into());
        request
    };
    let result = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(replace()),
    )
    .await;
    assert_eq!(result.unwrap_err().status, StatusCode::CONFLICT);
    sqlx::query("UPDATE mtls_ca_generations SET expires_at=now()-interval '1 second' WHERE ca_id=$1 AND ca_version=2").bind(&ca).execute(&state.pool).await.unwrap();
    let ordinary = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(2, "ordinary-expired")),
    )
    .await;
    assert_eq!(ordinary.unwrap_err().status, StatusCode::CONFLICT);
    let (_, Json(next)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(replace()),
    )
    .await
    .unwrap();
    assert_eq!(next["version"], 3);
    assert_eq!(published(&state, &ca).await, 2);
    let outcome: String = sqlx::query_scalar("SELECT outcome FROM mtls_rotations WHERE id=$1")
        .bind(rotation)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(outcome, "superseded_expired_publication");
    let generation: String =
        sqlx::query_scalar("SELECT state FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=2")
            .bind(&ca)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(generation, "published");
}

#[tokio::test]
async fn unconfirmed_remote_trust_survives_new_attempt_until_a_later_commit() {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    initial_deployment(&state, &node, &ca).await;
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "uncertain-trust")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    rotations::advance(&state, rotation).await.unwrap();
    let original = rotation_job(&state, rotation, n).await;
    sqlx::query("UPDATE jobs SET status='running',attempts=1 WHERE id=$1")
        .bind(&original)
        .execute(&state.pool)
        .await
        .unwrap();
    let input = job_input(&state, &original).await;
    let mut pending = snapshot(&state, n, 1).await;
    let context = rotations::configure_job(&state.pool, &state.secrets, &input, &mut pending)
        .await
        .unwrap()
        .unwrap();
    let probe = credentials::maintenance::renew_probe_on(&state.pool, &state.secrets, n, &ca, 2)
        .await
        .unwrap();
    let first = activation::begin(
        &state.pool,
        &state.secrets,
        activation::Begin {
            node: n,
            job: &original,
            revision: 1,
            digest: "unknown-remote",
            snapshot: &pending,
            trust: &context.trust,
            probe: Some((&probe.0, probe.1)),
        },
    )
    .await
    .unwrap();
    activation::uncertain(&state.pool, &original).await.unwrap();
    sqlx::query("UPDATE jobs SET status='failed' WHERE id=$1")
        .bind(&original)
        .execute(&state.pool)
        .await
        .unwrap();
    let newer = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,available_at,created_at,updated_at) VALUES($1,'deploy',$2,1,'running','starting',now(),now(),now())").bind(&newer).bind(n).execute(&state.pool).await.unwrap();
    let confirmed = snapshot(&state, n, 1).await;
    let old_trust = vec![TrustVersion {
        ca_id: ca.clone(),
        version: 1,
    }];
    let old_probe: (String,i64) = sqlx::query_as("SELECT credential_id,credential_version FROM mtls_probe_versions WHERE node_id=$1 AND ca_version=1 ORDER BY credential_version DESC LIMIT 1").bind(n).fetch_one(&state.pool).await.unwrap();
    let second = activation::begin(
        &state.pool,
        &state.secrets,
        activation::Begin {
            node: n,
            job: &newer,
            revision: 1,
            digest: &format!("verified-{newer}"),
            snapshot: &confirmed,
            trust: &old_trust,
            probe: Some((&old_probe.0, old_probe.1)),
        },
    )
    .await
    .unwrap();
    assert!(second.generation > first.generation);
    let possible: Vec<i64> = sqlx::query_scalar("SELECT DISTINCT ca_version FROM mtls_live_activation_trust WHERE node_id=$1 ORDER BY ca_version").bind(n).fetch_all(&state.pool).await.unwrap();
    assert_eq!(possible, vec![1, 2]);
    // The later attempt being recorded is not proof that the earlier remote switch failed.
    activation::rolled_back(&state.pool, &newer, second.previous_digest.as_deref())
        .await
        .unwrap();
    let pointer: String = sqlx::query_scalar("SELECT mtls_activation_id FROM nodes WHERE id=$1")
        .bind(n)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(pointer, first.id);
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM mtls_live_activation_trust WHERE node_id=$1 AND ca_version=2)",
    )
    .bind(n)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(live);
    // A separately verified later deployment finally supersedes the unresolved outcome.
    let recovery = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,available_at,created_at,updated_at) VALUES($1,'deploy',$2,1,'running','starting',now(),now(),now())").bind(&recovery).bind(n).execute(&state.pool).await.unwrap();
    finish(&state, &recovery, &confirmed, &old_trust, &old_probe).await;
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM mtls_live_activation_trust WHERE node_id=$1)",
    )
    .bind(n)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(!live);
    let counter: i64 = sqlx::query_scalar("SELECT mtls_generation FROM nodes WHERE id=$1")
        .bind(n)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(counter > second.generation);
    assert_eq!(published(&state, &ca).await, 1);
}

#[tokio::test]
async fn retention_removes_only_expired_superseded_unpinned_private_material() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let _ = group(&state, "Retention", &["alice"], &[n]).await;
    initial_deployment(&state, &node, &ca).await;
    issue(&state, "alice", &ca, true).await;
    issue(&state, "alice", &ca, true).await;
    let credential: String =
        sqlx::query_scalar("SELECT credential_id FROM mtls_user_identities WHERE user_id='alice'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    // A historical batch pins version 1. Version 3 is latest; only version 2 may disappear.
    let batch = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO credential_batches(id,credential_id,version,created_at) VALUES($1,$2,1,now())",
    )
    .bind(&batch)
    .bind(&credential)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE mtls_user_versions SET not_before=now()-interval '2 days',expires_at=now()-interval '10 minutes' WHERE credential_id=$1").bind(&credential).execute(&state.pool).await.unwrap();
    // Renew the node probe, then expire its predecessor. The committed deployment pins it.
    let old_probe: (String, i64) = sqlx::query_as(
        "SELECT credential_id,credential_version FROM mtls_active_probes WHERE node_id=$1",
    )
    .bind(n)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE mtls_probe_versions SET expires_at=now()-interval '10 minutes' WHERE node_id=$1",
    )
    .bind(n)
    .execute(&state.pool)
    .await
    .unwrap();
    let _ = credentials::maintenance::renew_probe_on(&state.pool, &state.secrets, n, &ca, 1)
        .await
        .unwrap();
    assert_eq!(credentials::retention::prune(&state).await.unwrap(), 1);
    let retained: Vec<i64> = sqlx::query_scalar(
        "SELECT version FROM credential_versions WHERE credential_id=$1 ORDER BY version",
    )
    .bind(&credential)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(retained, vec![1, 3]);
    let protected: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM credential_versions WHERE credential_id=$1 AND version=$2)",
    )
    .bind(&old_probe.0)
    .bind(old_probe.1)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(protected);
    // A removed pin makes an already expired superseded leaf eligible on the next pass.
    sqlx::query("DELETE FROM credential_batches WHERE id=$1")
        .bind(batch)
        .execute(&state.pool)
        .await
        .unwrap();
    // Advance only this persisted maintenance deadline. Both calls execute
    // actual owned pages, preserving the old material/pin assertions.
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE kind='credential-retention'")
        .execute(&state.pool).await.unwrap();
    assert_eq!(credentials::retention::prune(&state).await.unwrap(), 1);
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second' WHERE kind='credential-retention'")
        .execute(&state.pool).await.unwrap();
    assert_eq!(credentials::retention::prune(&state).await.unwrap(), 0);
    let retained: Vec<i64> = sqlx::query_scalar("SELECT credential_version FROM mtls_user_versions WHERE credential_id=$1 ORDER BY credential_version").bind(&credential).fetch_all(&state.pool).await.unwrap();
    assert_eq!(retained, vec![3]);
}

#[tokio::test]
async fn credential_pages_hide_internal_probes_before_paging_and_count_shared_node_references() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let first = managed_node(&state, &ca).await;
    let n = first["id"].as_str().unwrap();
    let second = managed_node(&state, &ca).await;
    let b = second["id"].as_str().unwrap();
    let _ = group(&state, "Shared", &["alice"], &[n, b]).await;
    initial_deployment(&state, &first, &ca).await;
    for (include, total) in [(false, 0_i64), (true, 1_i64)] {
        let Json(page) = api::list(
            State(state.clone()),
            axum::extract::Query(
                serde_json::from_value(
                    json!({"q":"Internal probe","page_size":1,"include_internal":include}),
                )
                .unwrap(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(page["total"], total);
        assert_eq!(page["items"].as_array().unwrap().len(), total as usize);
    }
    let Json(page) = api::list(
        State(state.clone()),
        axum::extract::Query(
            serde_json::from_value(json!({"kind":"ca_identity","page_size":1})).unwrap(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["reference_count"], 2);
    let credential: String =
        sqlx::query_scalar("SELECT credential_id FROM mtls_user_identities WHERE user_id='alice'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let Json(page) = api::list(
        State(state.clone()),
        axum::extract::Query(
            serde_json::from_value(json!({"kind":"tls_identity","category":"user"})).unwrap(),
        ),
    )
    .await
    .unwrap();
    let identity = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == credential)
        .unwrap();
    assert_eq!(identity["reference_count"], 2);
    // Cleanup state is queryable independently of publication batch state.
    sqlx::query("INSERT INTO mtls_trust_cleanup(ca_id,ca_version,next_action_at) VALUES($1,1,now()+interval '30 days')").bind(&ca).execute(&state.pool).await.unwrap();
    let job = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,status,stage,error_message,available_at,created_at,updated_at) VALUES($1,'deploy',$2,'failed','ssh','offline',now(),now(),now())").bind(&job).bind(n).execute(&state.pool).await.unwrap();
    sqlx::query(
        "INSERT INTO mtls_cleanup_nodes(ca_id,ca_version,node_id,job_id) VALUES($1,1,$2,$3)",
    )
    .bind(&ca)
    .bind(n)
    .bind(&job)
    .execute(&state.pool)
    .await
    .unwrap();
    let Json(detail) = api::get(State(state.clone()), Path(ca)).await.unwrap();
    assert_eq!(detail["ca"]["cleanup"][0]["version"], 1);
    assert_eq!(detail["ca"]["cleanup"][0]["nodes"][0]["job_id"], job);
    assert_eq!(
        detail["ca"]["cleanup"][0]["nodes"][0]["error_message"],
        "offline"
    );
    assert!(!detail.to_string().contains("PRIVATE KEY"));
}

#[tokio::test]
async fn http_batch_cache_tracks_ca_phases_and_post_publication_user_eligibility() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let _ = group(&state, "Cached CA", &["alice"], &[n]).await;
    initial_deployment(&state, &node, &ca).await;
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca),
        Json(publish_request(1, "cached-rotation")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap();
    let token = "mtls-cache-test-012345678901234567890123456789";
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('ca-test-admin',$1,'CA cache test',now())").bind(crate::security::token_digest(token)).execute(&state.pool).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/api/v2/credential-batches/{}",
        listener.local_addr().unwrap(),
        receipt["batch_id"].as_str().unwrap()
    );
    let app = crate::api::router(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    async fn read(client: &reqwest::Client, url: &str, token: &str) -> Value {
        client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    assert_eq!(
        read(&client, &url, token).await["ca_rotation"]["phase"],
        "trust"
    );
    assert_eq!(
        read(&client, &url, token).await["ca_rotation"]["phase"],
        "trust"
    );
    assert!(
        state.caches.stats()["routes"]["/api/v2/credential-batches/:id"]["reused"]
            .as_u64()
            .unwrap()
            > 0,
        "warm the actual HTTP response cache before state changes"
    );
    rotations::advance(&state, rotation).await.unwrap();
    complete_operation(&state, &rotation_job(&state, rotation, n).await).await;
    rotations::advance(&state, rotation).await.unwrap();
    assert_eq!(
        read(&client, &url, token).await["ca_rotation"]["phase"],
        "probes"
    );
    rotations::advance(&state, rotation).await.unwrap();
    let completed = read(&client, &url, token).await;
    assert_eq!(completed["ca_rotation"]["phase"], "complete");
    assert_eq!(completed["ca_rotation"]["status"]["eligible_user_count"], 1);
    let _ = read(&client, &url, token).await;
    sqlx::query("UPDATE users SET enabled=FALSE WHERE id='alice'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        read(&client, &url, token).await["ca_rotation"]["status"]["eligible_user_count"],
        0
    );
    if std::env::var_os("HYSTERIAX_TEST_EXPECT_REDIS").is_some() {
        let redis = state
            .caches
            .redis
            .as_ref()
            .expect("Redis must be configured for this run");
        assert!(redis.available());
        assert_eq!(redis.stats()["errors"], 0);
    }
    server.abort();
}

#[tokio::test]
async fn record_removal_and_never_installed_delete_cannot_forget_uncertain_managed_trust() {
    let state = state().await;
    let ca = generated(&state).await;
    let pending = managed_node(&state, &ca).await;
    let pending_id = pending["id"].as_str().unwrap();
    let removed = crate::api::nodes::remove_record(
        State(state.clone()),
        Path(pending_id.into()),
        axum::extract::Query(crate::api::nodes::RevisionQuery {
            expected_revision: Some(1),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        removed,
        StatusCode::NO_CONTENT,
        "a desired-only node with no remote attempt can be cancelled"
    );
    let node = managed_node(&state, &ca).await;
    let n = node["id"].as_str().unwrap();
    let job = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,available_at,created_at,updated_at) VALUES($1,'deploy',$2,1,'running','installing',now(),now(),now())").bind(&job).bind(n).execute(&state.pool).await.unwrap();
    let snapshot = snapshot(&state, n, 1).await;
    let trust = node_mtls::desired_trust(
        &state.pool,
        n,
        &Settings::from_config(&node["config"]).unwrap(),
    )
    .await
    .unwrap();
    let probe = credentials::maintenance::renew_probe_on(&state.pool, &state.secrets, n, &ca, 1)
        .await
        .unwrap();
    activation::begin(
        &state.pool,
        &state.secrets,
        activation::Begin {
            node: n,
            job: &job,
            revision: 1,
            digest: "possible-first-remote-switch",
            snapshot: &snapshot,
            trust: &trust,
            probe: Some((&probe.0, probe.1)),
        },
    )
    .await
    .unwrap();
    activation::uncertain(&state.pool, &job).await.unwrap();
    sqlx::query("UPDATE jobs SET status='failed' WHERE id=$1")
        .bind(job)
        .execute(&state.pool)
        .await
        .unwrap();
    // SSH diagnosis can return the ordinary connection state to ready; it is
    // not evidence that a failed first activation never reached the remote.
    sqlx::query("UPDATE nodes SET state='ready' WHERE id=$1")
        .bind(n)
        .execute(&state.pool)
        .await
        .unwrap();
    let error = crate::api::nodes::remove_record(
        State(state.clone()),
        Path(n.into()),
        axum::extract::Query(crate::api::nodes::RevisionQuery {
            expected_revision: Some(1),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("managed mTLS remote trust"));
    let (status, _) = crate::api::nodes::delete(
        State(state.clone()),
        Path(n.into()),
        axum::extract::Query(crate::api::nodes::RevisionQuery {
            expected_revision: Some(1),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "an uncertain first activation requires a real uninstall"
    );
    assert!(
        rotations::members_on(&mut state.pool.acquire().await.unwrap(), &ca)
            .await
            .unwrap()
            .contains(&n.to_owned())
    );
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM nodes WHERE id=$1)")
        .bind(n)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(exists);
}

// Shared real API / activation fixture for publication metadata transaction
// tests. The node lease keeps asynchronous metadata pending until the fixture
// returns; it never bypasses the publication trust proof.
pub(crate) async fn metadata_fixture(pending_edit: bool) -> (AppState, String, String, String) {
    metadata_fixture_with_pool(
        crate::db::test_pool_with_max_connections(4).await,
        pending_edit,
    )
    .await
}
pub(crate) async fn metadata_fixture_with_pool(
    pool: sqlx::PgPool,
    pending_edit: bool,
) -> (AppState, String, String, String) {
    let (state, ca, rotation, id) = publication_fixture_with_pool(pool, pending_edit).await;
    rotations::advance(&state, &rotation).await.unwrap();
    complete_operation(&state, &rotation_job(&state, &rotation, &id).await).await;
    let resource = format!("node:{id}");
    let lease = db::write::<_, anyhow::Error, _>(&state.pool, move |tx| {
        let resource = resource.clone();
        Box::pin(async move {
            Ok(
                crate::execution::acquire(tx, &resource, "fixture", "metadata-fixture-hold")
                    .await?
                    .unwrap(),
            )
        })
    })
    .await
    .unwrap();
    rotations::advance(&state, &rotation).await.unwrap();
    assert_eq!(published(&state, &ca).await, 2);
    // Direct advancement is best effort: a transiently unavailable CA/work
    // claim can leave admission queued. Keep the node fenced until its child
    // actually exists, as required by this fixture's returned contract.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let admitted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_work WHERE kind='mtls-node-publication-metadata' AND payload->>'rotation_id'=$1 AND payload->>'node_id'=$2)")
                .bind(&rotation).bind(&id).fetch_one(&state.pool).await.unwrap();
            if admitted { break; }
            rotations::advance(&state, &rotation).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("metadata fixture must admit its fenced node before returning");

    db::write::<_, anyhow::Error, _>(&state.pool, move |tx| {
        let lease = lease.clone();
        Box::pin(async move {
            lease.release(tx).await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    (state, ca, rotation, id)
}

// Return a real verified trust activation before the final CA publication.
pub(crate) async fn release_fixture() -> (AppState, String, String, String) {
    let state = state().await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap().to_owned();
    initial_deployment(&state, &node, &ca).await;
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "release-fixture")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap().to_owned();
    rotations::advance(&state, &rotation).await.unwrap();
    complete_operation(&state, &rotation_job(&state, &rotation, &id).await).await;
    assert_eq!(published(&state, &ca).await, 1);
    (state, ca, rotation, id)
}

pub(crate) async fn probe_switch_fixture() -> (AppState, String, String, Vec<String>) {
    let state = state().await;
    let ca = generated(&state).await;
    let mut nodes = Vec::new();
    for _ in 0..2 {
        let node = managed_node(&state, &ca).await;
        initial_deployment(&state, &node, &ca).await;
        nodes.push(node["id"].as_str().unwrap().to_owned());
    }
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "probe-switch-fixture")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap().to_owned();
    rotations::advance(&state, &rotation).await.unwrap();
    for node in &nodes {
        complete_operation(&state, &rotation_job(&state, &rotation, node).await).await;
    }
    rotations::advance(&state, &rotation).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while published(&state, &ca).await != 2 {
            rotations::advance(&state, &rotation).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("probe fixture must publish after both actual activations");
    assert_eq!(published(&state, &ca).await, 2);
    (state, ca, rotation, nodes)
}

pub(crate) async fn complete_cleanup_fixture(state: &AppState, job: &str) {
    complete_operation(state, job).await;
}

// Historical migration fixtures prepare source data without running a worker
// compiled for a newer resource-lease schema.
pub(crate) async fn publication_fixture_with_pool(
    pool: sqlx::PgPool,
    pending_edit: bool,
) -> (AppState, String, String, String) {
    let state = super::state_with_pool(pool).await;
    let ca = generated(&state).await;
    let node = managed_node(&state, &ca).await;
    let id = node["id"].as_str().unwrap().to_owned();
    initial_deployment(&state, &node, &ca).await;
    if pending_edit {
        let _ = crate::api::nodes::patch(
            State(state.clone()),
            Path(id.clone()),
            Json(
                serde_json::from_value(json!({"expected_revision":1,"name":"Pending name"}))
                    .unwrap(),
            ),
        )
        .await
        .unwrap();
    }
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "metadata-fixture")),
    )
    .await
    .unwrap();
    let rotation = receipt["rotation_id"].as_str().unwrap().to_owned();
    (state, ca, rotation, id)
}
