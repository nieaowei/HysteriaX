use super::*;
use crate::{credentials::tests as fixture_api, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
};

struct Fixture {
    state: AppState,
    job: JobInput,
    leases: Vec<Execution>,
}
async fn fixture(config: bool) -> Fixture {
    let state = fixture_api::state().await;
    let ssh = fixture_api::create(&state, "ssh_password", json!({"secret":"old"})).await;
    let (credential, payload, config) = if config {
        let payload = json!({"certificate":include_str!("../../../../../../tests/fixtures/credentials-test.crt"),"private_key":include_str!("../../../../../../tests/fixtures/credentials-test.key")});
        let id = fixture_api::create(&state, "tls_identity", payload.clone()).await;
        let config = json!({"tls":{"cert":crate::credentials::Reference{id:id.clone(),version:1,field:"certificate".into()}.uri(),"key":crate::credentials::Reference{id:id.clone(),version:1,field:"private_key".into()}.uri()}});
        (id, payload, config)
    } else {
        (ssh.clone(), json!({"secret":"new"}), json!({}))
    };
    fixture_api::node(&state, "target", &ssh, config).await;
    let _receipt = crate::api::credentials::publish(
        State(state.clone()),
        fixture_api::actor(),
        Path(credential),
        Json(crate::api::credentials::PublishVersion {
            expected_revision: 1,
            payload,
        }),
    )
    .await
    .unwrap();
    let (job, leases) = crate::jobs::claim_for_test(&state.pool, &state.instance_id)
        .await
        .unwrap();
    assert_eq!(job.kind, "credential-apply");
    Fixture { state, job, leases }
}
async fn prepared(f: &Fixture) -> Result<Prepared> {
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        prepare(&f.state.pool, &f.job),
    )
    .await
    .unwrap()
}
async fn candidate(f: &Fixture) -> (Source, Value, Option<Value>) {
    let Prepared::Ready(s, row) = prepared(f).await.unwrap() else {
        panic!("already applied")
    };
    let config = if f.job.payload["mode"] == "config" {
        let mut c: Value = serde_json::from_str(
            &f.state
                .secrets
                .decrypt(&row.get::<String, _>("desired_config_enc"))
                .unwrap(),
        )
        .unwrap();
        crate::credentials::replace_version(
            &mut c,
            f.job.payload["credential_id"].as_str().unwrap(),
            2,
        )
        .unwrap();
        Some(c)
    } else {
        None
    };
    let snapshot = super::super::make_snapshot(&f.state.secrets, &row, config.as_ref()).unwrap();
    (s, snapshot, config)
}
async fn save(
    f: &Fixture,
    s: &Source,
    snapshot: &Value,
    config: Option<&Value>,
) -> Result<JobOutput> {
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        commit(&f.state.pool, &f.state.secrets, s, snapshot, config),
    )
    .await
    .unwrap()
}
async fn snapshot(f: &Fixture) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'items',(SELECT jsonb_agg(to_jsonb(i) ORDER BY batch_id,node_id,user_id) FROM credential_batch_items i),'snapshots',(SELECT jsonb_agg(to_jsonb(v) ORDER BY id) FROM config_versions v),'receipts',(SELECT jsonb_agg(to_jsonb(r) ORDER BY job_id) FROM credential_application_receipts r),'jobs',(SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j),'events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM job_events e),'refs',(SELECT jsonb_agg(to_jsonb(e) ORDER BY to_jsonb(e)::text) FROM credential_reference_edges e))").fetch_one(&f.state.pool).await.unwrap()
}
async fn count(f: &Fixture, q: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(q))
        .fetch_one(&f.state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn config_worker_handoff_snapshot_refs_and_receipt_commit_once_without_global_lock() {
    let f = fixture(true).await;
    let legacy = db::hold_retired_global_write_lock(&f.state.pool)
        .await
        .unwrap();
    let result = execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        super::super::apply(&f.state.pool, &f.state.secrets, &f.job),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.stage, "credential_deployment_queued");
    assert_eq!(
        count(&f, "SELECT count(*) FROM jobs WHERE kind='sync'").await,
        1
    );
    assert_eq!(count(&f, "SELECT count(*) FROM config_versions").await, 2);
    assert_eq!(
        count(&f, "SELECT count(*) FROM credential_application_receipts").await,
        1
    );
    let before = snapshot(&f).await;
    let result = execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        super::super::apply(&f.state.pool, &f.state.secrets, &f.job),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.result["already_applied"], true);
    assert_eq!(snapshot(&f).await, before);
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn receipt_failure_rolls_back_node_history_refs_and_sync_then_native_retry_commits_once() {
    let f = fixture(true).await;
    let (s, snapshot_value, config) = candidate(&f).await;
    sqlx::raw_sql("CREATE FUNCTION reject_application() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject'; END $$; CREATE TRIGGER reject_application BEFORE INSERT ON credential_application_receipts FOR EACH ROW EXECUTE FUNCTION reject_application()")
        .execute(&f.state.pool).await.unwrap();
    let before = snapshot(&f).await;
    assert!(
        save(&f, &s, &snapshot_value, config.as_ref())
            .await
            .is_err()
    );
    assert_eq!(snapshot(&f).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_application ON credential_application_receipts; CREATE SEQUENCE application_attempt; CREATE FUNCTION retry_application() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('application_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40P01'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_application BEFORE INSERT ON credential_application_receipts FOR EACH ROW EXECUTE FUNCTION retry_application()")
        .execute(&f.state.pool).await.unwrap();
    save(&f, &s, &snapshot_value, config.as_ref())
        .await
        .unwrap();
    assert_eq!(
        count(&f, "SELECT last_value FROM application_attempt").await,
        2
    );
    assert_eq!(
        count(&f, "SELECT count(*) FROM jobs WHERE kind='sync'").await,
        1
    );
    assert_eq!(
        count(&f, "SELECT count(*) FROM credential_application_receipts").await,
        1
    );
}

#[tokio::test]
async fn changed_node_credential_batch_or_item_rejects_old_verified_result() {
    for query in [
        "UPDATE nodes SET desired_revision=desired_revision+1",
        "UPDATE nodes SET ssh_host='192.0.2.1'",
        "UPDATE nodes SET state='deleting'",
        "UPDATE credentials SET archived=true WHERE kind='ssh_password'",
        "UPDATE credentials SET latest_version=1 WHERE kind='ssh_password'",
        "UPDATE credential_batches SET version=1",
        "UPDATE credential_batch_items SET expected_revision=expected_revision+1",
        "UPDATE credential_batch_items SET applied_at=clock_timestamp(),apply_stage='credential_applied'",
    ] {
        let f = fixture(false).await;
        let (s, value, config) = candidate(&f).await;
        sqlx::query(query).execute(&f.state.pool).await.unwrap();
        let before = snapshot(&f).await;
        assert!(
            save(&f, &s, &value, config.as_ref()).await.is_err(),
            "{query}"
        );
        assert_eq!(snapshot(&f).await, before);
    }
}

#[tokio::test]
async fn ssh_receipt_survives_new_publication_and_replays_without_reconnecting() {
    let f = fixture(false).await;
    let (s, value, _) = candidate(&f).await;
    save(&f, &s, &value, None).await.unwrap();
    sqlx::query("UPDATE credentials SET archived=true WHERE kind='ssh_password'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    let before = snapshot(&f).await;
    let result = execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        super::super::apply(&f.state.pool, &f.state.secrets, &f.job),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.stage, "credential_applied");
    assert_eq!(result.result["already_applied"], true);
    assert_eq!(snapshot(&f).await, before);
}

#[tokio::test]
async fn takeover_and_unknown_steps_do_not_publish_old_verification() {
    let mut f = fixture(false).await;
    let (old, value, _) = candidate(&f).await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    let mut tx = f.state.pool.begin().await.unwrap();
    let mut leases = vec![];
    for old in &f.leases {
        leases.push(
            execution::acquire(&mut tx, &old.resource, "next", &f.job.id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    f.leases = leases;
    let before = snapshot(&f).await;
    assert!(save(&f, &old, &value, None).await.is_err());
    assert_eq!(snapshot(&f).await, before);
    let (current, value, _) = candidate(&f).await;
    let mut tx = f.state.pool.begin().await.unwrap();
    f.leases[0]
        .dispatch(&mut tx, "unknown", &json!({}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &current, &value, None).await.is_err());
    assert_eq!(snapshot(&f).await, before);
}

#[tokio::test]
async fn historical_applied_item_without_receipt_and_wrong_job_source_are_rejected() {
    let f = fixture(false).await;
    let mut wrong = f.job.clone();
    wrong.payload["version"] = json!(1);
    assert!(
        execution::supervise(
            f.state.pool.clone(),
            f.leases.clone(),
            prepare(&f.state.pool, &wrong)
        )
        .await
        .unwrap()
        .is_err()
    );
    sqlx::query("UPDATE credential_batch_items SET applied_at=clock_timestamp(),apply_stage='credential_applied'").execute(&f.state.pool).await.unwrap();
    let before = snapshot(&f).await;
    assert!(prepared(&f).await.is_err());
    assert_eq!(snapshot(&f).await, before);
}

#[tokio::test]
async fn partial_scope_changed_attempt_and_another_resource_binding_cannot_confirm() {
    let f = fixture(false).await;
    let (s, value, _) = candidate(&f).await;
    let lease = f
        .leases
        .iter()
        .find(|l| l.resource.starts_with("job:"))
        .unwrap()
        .clone();
    let before = snapshot(&f).await;
    assert!(
        execution::supervise(
            f.state.pool.clone(),
            vec![lease],
            commit(&f.state.pool, &f.state.secrets, &s, &value, None)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(snapshot(&f).await, before);
    sqlx::query("UPDATE jobs SET resource_key='another' WHERE id=$1")
        .bind(&f.job.id)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &s, &value, None).await.is_err());
    assert_eq!(snapshot(&f).await, before);
    sqlx::query("UPDATE jobs SET resource_key=NULL,attempts=attempts+1 WHERE id=$1")
        .bind(&f.job.id)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &s, &value, None).await.is_err());
    assert_eq!(snapshot(&f).await, before);
}

#[tokio::test]
async fn repeat_migration_preserves_applied_or_pending_business_history() {
    let f = fixture(false).await;
    sqlx::query("UPDATE credential_batch_items SET applied_at=clock_timestamp(),apply_stage='credential_applied'").execute(&f.state.pool).await.unwrap();
    let before = snapshot(&f).await;
    let credentials: Value =
        sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM credentials c")
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
    let versions: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v",
    )
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    crate::db::run_migrations(&f.state.pool).await.unwrap();
    crate::db::run_migrations(&f.state.pool).await.unwrap();
    assert_eq!(snapshot(&f).await, before);
    assert_eq!(
        sqlx::query_scalar::<_, Value>(
            "SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM credentials c"
        )
        .fetch_one(&f.state.pool)
        .await
        .unwrap(),
        credentials
    );
    assert_eq!(sqlx::query_scalar::<_,Value>("SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v").fetch_one(&f.state.pool).await.unwrap(),versions);
    assert!(prepared(&f).await.is_err());
}
