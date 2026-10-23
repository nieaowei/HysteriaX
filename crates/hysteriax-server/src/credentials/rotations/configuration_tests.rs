use super::*;
use crate::execution;

async fn fixture(n: i32) -> (AppState, String, String) {
    cancelled::tests::fixture(n).await
}
async fn input(state: &AppState, id: &str) -> JobInput {
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
        attempts: row.get("attempts"),
    }
}
fn snapshot(ca: &str) -> Value {
    json!({"server_config":{"tls":{},"mtls":{"mode":"managed","ca_credential_id":ca,"ca_credential_version":1}},"marker":"prepared outside transaction"})
}
async fn source(state: &AppState, job: &JobInput) -> sqlx::postgres::PgRow {
    sqlx::query(CONFIGURATION_SOURCE)
        .bind(&job.id)
        .bind(&job.node_id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn effective(state: &AppState, id: &str) -> Option<i64> {
    sqlx::query_scalar("SELECT effective_version FROM mtls_node_operations WHERE job_id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn leases(state: &AppState, job: &JobInput, owner: &str) -> Vec<execution::Execution> {
    sqlx::query("UPDATE jobs SET status='running' WHERE id=$1")
        .bind(&job.id)
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let mut result = vec![];
    for key in [
        format!("job:{}", job.id),
        format!("node:{}", job.node_id.as_deref().unwrap()),
    ] {
        result.push(
            execution::acquire(&mut tx, &key, owner, &job.id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    result
}

#[tokio::test]
async fn configuration_binds_only_its_operation_without_waiting_for_the_old_global_lock_or_another_node()
 {
    let (state, ca, _) = fixture(2).await;
    let job = input(&state, "old-member-0001").await;
    let legacy = db::begin_write(&state.pool).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='member-0002' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let mut value = snapshot(&ca);
    let before = value.clone();
    let context = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        configure_job(&state.pool, &state.secrets, &job, &mut value),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(context.probe_version, 2);
    assert!(context.skip_users);
    assert!(!context.recovery);
    assert_eq!(
        context.trust.iter().map(|v| v.version).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(value, before);
    assert_eq!(effective(&state, &job.id).await, Some(1));
    assert_eq!(effective(&state, "old-member-0002").await, None);
    held.rollback().await.unwrap();
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn old_execution_cannot_bind_after_expiry_and_takeover_binds_once_with_the_new_generation() {
    let (state, ca, _) = fixture(1).await;
    let job = input(&state, "old-member-0001").await;
    let old = leases(&state, &job, "old").await;
    let app = state.clone();
    let request = job.clone();
    let authority = ca.clone();
    let result=execution::supervise(state.pool.clone(),old,async move {
        sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&request.id).execute(&app.pool).await.unwrap();
        let mut value=snapshot(&authority);let before=value.clone();
        let result=configure_job(&app.pool,&app.secrets,&request,&mut value).await;
        assert!(result.is_err());assert_eq!(value,before);
    }).await;
    let _ = result;
    assert_eq!(effective(&state, &job.id).await, None);
    let next = leases(&state, &job, "next").await;
    assert!(next.iter().all(|l| l.generation == 2));
    let app = state.clone();
    let request = job.clone();
    execution::supervise(state.pool.clone(), next, async move {
        let mut value = snapshot(&ca);
        configure_job(&app.pool, &app.secrets, &request, &mut value).await
    })
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(effective(&state, &job.id).await, Some(1));
}

#[tokio::test]
async fn a_valid_execution_for_another_job_and_node_cannot_bind_this_operation() {
    let (state, ca, _) = fixture(2).await;
    let one = input(&state, "old-member-0001").await;
    let other = input(&state, "old-member-0002").await;
    let owner = leases(&state, &one, "owner").await;
    let app = state.clone();
    let request = other.clone();
    execution::supervise(state.pool.clone(), owner, async move {
        let mut value = snapshot(&ca);
        let before = value.clone();
        assert!(
            configure_job(&app.pool, &app.secrets, &request, &mut value)
                .await
                .is_err()
        );
        assert_eq!(value, before);
    })
    .await
    .unwrap();
    assert_eq!(effective(&state, &one.id).await, None);
    assert_eq!(effective(&state, &other.id).await, None);
}

#[tokio::test]
async fn native_binding_retry_is_idempotent_and_preserves_all_encrypted_history() {
    let (state, ca, _) = fixture(1).await;
    let job = input(&state, "old-member-0001").await;
    let snapshot_query =
        "SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v";
    let before: Value = sqlx::query_scalar(snapshot_query)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE SEQUENCE binding_attempts; CREATE FUNCTION retry_configuration_binding() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.effective_version IS NOT NULL AND nextval('binding_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_configuration_binding BEFORE UPDATE OF effective_version ON mtls_node_operations FOR EACH ROW EXECUTE FUNCTION retry_configuration_binding();").execute(&state.pool).await.unwrap();
    let mut value = snapshot(&ca);
    configure_job(&state.pool, &state.secrets, &job, &mut value)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(effective(&state, &job.id).await, Some(1));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM binding_attempts")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(snapshot_query)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    sqlx::raw_sql("DROP TRIGGER retry_configuration_binding ON mtls_node_operations")
        .execute(&state.pool)
        .await
        .unwrap();
    configure_job(&state.pool, &state.secrets, &job, &mut value)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(snapshot_query)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn rejected_binding_does_not_publish_the_tentatively_rewritten_snapshot() {
    let (state, ca, _) = fixture(1).await;
    let job = input(&state, "old-member-0001").await;
    sqlx::raw_sql("CREATE FUNCTION reject_configuration_binding() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject binding'; END $$; CREATE TRIGGER reject_configuration_binding BEFORE UPDATE OF effective_version ON mtls_node_operations FOR EACH ROW EXECUTE FUNCTION reject_configuration_binding();").execute(&state.pool).await.unwrap();
    let mut value = snapshot(&ca);
    value["server_config"]["mtls"]["ca_credential_version"] = json!(2);
    let before = value.clone();
    assert!(
        configure_job(&state.pool, &state.secrets, &job, &mut value)
            .await
            .is_err()
    );
    assert_eq!(value, before);
    assert_eq!(effective(&state, &job.id).await, None);
}

#[tokio::test]
async fn an_existing_binding_to_a_different_publication_is_never_overwritten() {
    let (state, ca, _) = fixture(1).await;
    let job = input(&state, "old-member-0001").await;
    sqlx::query("UPDATE mtls_node_operations SET effective_version=2 WHERE job_id=$1")
        .bind(&job.id)
        .execute(&state.pool)
        .await
        .unwrap();
    let mut value = snapshot(&ca);
    let before = value.clone();
    assert!(
        configure_job(&state.pool, &state.secrets, &job, &mut value)
            .await
            .is_err()
    );
    assert_eq!(value, before);
    assert_eq!(effective(&state, &job.id).await, Some(2));
}

#[tokio::test]
async fn parent_or_operation_changes_between_preparation_and_binding_reject_stale_results() {
    for change in [
        "UPDATE credentials SET archived=true WHERE kind='ca_identity'",
        "UPDATE credentials SET latest_version=latest_version+1 WHERE kind='ca_identity'",
        "UPDATE nodes SET state='deleting'",
        "UPDATE nodes SET desired_revision=desired_revision+1",
        "UPDATE nodes SET deployed_content_sha256='new-digest'",
        "UPDATE mtls_node_operations SET recovery=true",
        "UPDATE mtls_rotations SET phase='complete',outcome='superseded_unpublished_target'",
        "UPDATE mtls_rotations SET outcome='superseded_unpublished_target'",
    ] {
        let (state, _, _) = fixture(1).await;
        let job = input(&state, "old-member-0001").await;
        if change == "UPDATE nodes SET desired_revision=desired_revision+1" {
            sqlx::query("UPDATE mtls_node_operations SET use_desired=true")
                .execute(&state.pool)
                .await
                .unwrap();
        }
        let prepared = source(&state, &job).await;
        sqlx::query(change).execute(&state.pool).await.unwrap();
        assert!(
            bind_configuration(&state.pool, &job, &prepared)
                .await
                .is_err(),
            "{change}"
        );
        assert_eq!(effective(&state, &job.id).await, None);
    }
}

#[tokio::test]
async fn a_new_unpublished_target_invalidates_configuration_even_before_the_published_pointer_changes()
 {
    use axum::{
        Json,
        extract::{Extension, Path, State},
    };
    let (state, ca, rotation) = fixture(1).await;
    let job = input(&state, "old-member-0001").await;
    let prepared = source(&state, &job).await;
    let _ = crate::api::managed_mtls::publish(
        State(state.clone()),
        Extension(crate::api::AdminActor { id: "test".into() }),
        Path(ca),
        Json(crate::api::managed_mtls::PublishAuthority {
            expected_revision: 2,
            certificate: None,
            private_key: None,
            replace_rotation_id: Some(rotation),
            idempotency_key: "supersede-configuration".into(),
        }),
    )
    .await
    .unwrap();
    assert!(
        bind_configuration(&state.pool, &job, &prepared)
            .await
            .is_err()
    );
    assert_eq!(effective(&state, &job.id).await, None);
}

#[tokio::test]
async fn database_clock_controls_the_expiring_issuer_recovery_boundary() {
    let (state, ca, _) = fixture(1).await;
    let job = input(&state, "old-member-0001").await;
    sqlx::query("UPDATE mtls_node_trust SET deployed=false")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_node_operations SET recovery=true")
        .execute(&state.pool)
        .await
        .unwrap();
    for (seconds, skip) in [(600_i64, false), (240, true)] {
        sqlx::query("UPDATE mtls_ca_generations SET expires_at=clock_timestamp()+make_interval(secs=>$2::double precision) WHERE ca_id=$1 AND ca_version=1").bind(&ca).bind(seconds as f64).execute(&state.pool).await.unwrap();
        let mut value = snapshot(&ca);
        let context = configure_job(&state.pool, &state.secrets, &job, &mut value)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(context.skip_users, skip);
    }
}

async fn pending_fixture() -> (AppState, String, JobInput, Value) {
    let (state, ca, rotation, node) =
        crate::api::managed_mtls::tests::rotation_flow::release_fixture().await;
    let mut tx = state.pool.begin().await.unwrap();
    let id = enqueue_node(&mut tx, &node, &ca, 2, Some(&rotation), "trust")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let job = input(&state, &id).await;
    let mut value = json!({"caller":"independent desired snapshot"});
    let context = configure_job(&state.pool, &state.secrets, &job, &mut value)
        .await
        .unwrap()
        .unwrap();
    let probe:(String,i64)=sqlx::query_as("SELECT credential_id,credential_version FROM mtls_probe_versions WHERE node_id=$1 AND ca_id=$2 AND ca_version=2 ORDER BY credential_version DESC LIMIT 1").bind(&node).bind(&ca).fetch_one(&state.pool).await.unwrap();
    crate::credentials::activation::begin(
        &state.pool,
        &state.secrets,
        crate::credentials::activation::Begin {
            node: &node,
            job: &id,
            revision: job.target_revision.unwrap(),
            digest: "configuration-pending-marker",
            snapshot: &value,
            trust: &context.trust,
            probe: Some((&probe.0, probe.1)),
        },
    )
    .await
    .unwrap();
    let cipher: String =
        sqlx::query_scalar("SELECT snapshot_enc FROM mtls_activations WHERE job_id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let expected = serde_json::from_str(&state.secrets.decrypt(&cipher).unwrap()).unwrap();
    (state, ca, job, expected)
}

#[tokio::test]
async fn known_pending_recovery_preserves_the_exact_snapshot_and_cipher_after_the_old_issuer_expires()
 {
    let (state, ca, job, expected) = pending_fixture().await;
    sqlx::query("UPDATE mtls_node_operations SET recovery=true WHERE job_id=$1")
        .bind(&job.id)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_ca_generations SET expires_at=clock_timestamp()-interval '1 minute' WHERE ca_id=$1 AND ca_version=1").bind(&ca).execute(&state.pool).await.unwrap();
    let q = "SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM mtls_activations a";
    let before: Value = sqlx::query_scalar(q).fetch_one(&state.pool).await.unwrap();
    let mut value = json!({"caller":"must not replace known pending material"});
    let context = configure_job(&state.pool, &state.secrets, &job, &mut value)
        .await
        .unwrap()
        .unwrap();
    assert!(context.recovery);
    assert!(context.skip_users);
    assert_eq!(context.probe_version, 2);
    assert_eq!(value, expected);
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(q)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    assert_eq!(effective(&state, &job.id).await, Some(1));
}

#[tokio::test]
async fn a_changed_pending_activation_rejects_old_preparation_without_replacing_its_evidence() {
    let (state, _, job, expected) = pending_fixture().await;
    sqlx::query("UPDATE mtls_node_operations SET effective_version=NULL WHERE job_id=$1")
        .bind(&job.id)
        .execute(&state.pool)
        .await
        .unwrap();
    let prepared = source(&state, &job).await;
    crate::credentials::activation::verified(&state.pool, &job.id)
        .await
        .unwrap();
    let q = "SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM mtls_activations a";
    let before: Value = sqlx::query_scalar(q).fetch_one(&state.pool).await.unwrap();
    assert!(
        bind_configuration(&state.pool, &job, &prepared)
            .await
            .is_err()
    );
    assert_eq!(effective(&state, &job.id).await, None);
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(q)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    let mut value = json!({"caller":"fresh preparation"});
    configure_job(&state.pool, &state.secrets, &job, &mut value)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(value, expected);
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(q)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn unrelated_ca_labels_policy_and_pending_desired_edits_do_not_invalidate_a_deployed_configuration()
 {
    let (state, _, _) = fixture(1).await;
    let job = input(&state, "old-member-0001").await;
    let prepared = source(&state, &job).await;
    sqlx::query(
        "UPDATE credentials SET name='Renamed CA',revision=revision+1 WHERE kind='ca_identity'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE mtls_authorities SET policy_revision=policy_revision+1")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET desired_revision=desired_revision+1")
        .execute(&state.pool)
        .await
        .unwrap();
    let q = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'authorities',(SELECT jsonb_agg(to_jsonb(a) ORDER BY credential_id) FROM mtls_authorities a),'credentials',(SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM credentials c))";
    let before: Value = sqlx::query_scalar(q).fetch_one(&state.pool).await.unwrap();
    assert!(
        bind_configuration(&state.pool, &job, &prepared)
            .await
            .unwrap()
    );
    assert_eq!(effective(&state, &job.id).await, Some(1));
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(q)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
}
