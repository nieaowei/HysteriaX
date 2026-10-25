use super::*;
use crate::{
    execution::{self, Execution},
    state::AppState,
};
struct Fixture {
    state: AppState,
    job: JobInput,
    leases: Vec<Execution>,
}
async fn fixture() -> Fixture {
    let state = AppState::new(
        db::test_pool_with_max_connections(8).await,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    let request=crate::credentials::test_node_request(&state,json!({"name":"target","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"fixture","public_host":"example.test","public_port":443,"listen_addr":":443","config":{}})).await;
    let (_, axum::Json(receipt)) =
        crate::api::nodes::create(axum::extract::State(state.clone()), axum::Json(request))
            .await
            .unwrap();
    let node = receipt["node"]["id"].as_str().unwrap().to_owned();
    sqlx::query("UPDATE nodes SET state='deployed',deployed_revision=1,deployed_config_enc=desired_config_enc,deployed_content_sha256='saved' WHERE id=$1").bind(&node).execute(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    crate::api::enqueue_job_in_tx(&mut tx, "sync", Some(&node), Some(1))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    let (job, leases) = crate::jobs::claim_for_test(&state.pool, &state.instance_id)
        .await
        .unwrap();
    Fixture { state, job, leases }
}
async fn save(f: &Fixture, job: &JobInput) -> Option<Result<()>> {
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        record(&f.state.pool, job, 1, Some(1), "saved"),
    )
    .await
}
async fn snapshot(f: &Fixture) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'jobs',(SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j),'events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM job_events e))").fetch_one(&f.state.pool).await.unwrap()
}
#[tokio::test]
async fn drift_state_progress_and_event_commit_once_without_the_old_lock() {
    let f = fixture().await;
    let legacy = db::hold_retired_global_write_lock(&f.state.pool)
        .await
        .unwrap();
    save(&f, &f.job).await.unwrap().unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes WHERE state='drift'")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE stage='drift_detected'")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        1
    );
    let before = snapshot(&f).await;
    save(&f, &f.job).await.unwrap().unwrap();
    assert_eq!(snapshot(&f).await, before);
    legacy.rollback().await.unwrap();
}
#[tokio::test]
async fn event_failure_rolls_back_node_and_progress_then_native_retry_commits_one_observation() {
    let f = fixture().await;
    sqlx::raw_sql("CREATE FUNCTION reject_drift() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.progress' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_drift BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_drift()")
        .execute(&f.state.pool).await.unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &f.job).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_drift ON job_events; CREATE SEQUENCE drift_attempt; CREATE FUNCTION retry_drift() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.progress' AND nextval('drift_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40P01'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_drift BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION retry_drift()")
        .execute(&f.state.pool).await.unwrap();
    save(&f, &f.job).await.unwrap().unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM drift_attempt")
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM job_events WHERE event_type='job.progress'"
        )
        .fetch_one(&f.state.pool)
        .await
        .unwrap(),
        1
    );
}
#[tokio::test]
async fn changed_node_deployment_and_job_source_do_not_accept_an_old_remote_observation() {
    for query in [
        "UPDATE nodes SET desired_revision=2",
        "UPDATE nodes SET deployed_revision=NULL",
        "UPDATE nodes SET deployed_content_sha256='new'",
        "UPDATE nodes SET state='deleting'",
        "UPDATE jobs SET attempts=attempts+1 WHERE kind='sync'",
        "UPDATE jobs SET payload_json='{\"force\":true}' WHERE kind='sync'",
    ] {
        let f = fixture().await;
        sqlx::query(query).execute(&f.state.pool).await.unwrap();
        let before = snapshot(&f).await;
        assert!(save(&f, &f.job).await.is_none_or(|r| r.is_err()), "{query}");
        assert_eq!(snapshot(&f).await, before);
    }
}
#[tokio::test]
async fn old_owner_missing_node_scope_and_unknown_remote_work_cannot_mark_drift() {
    let mut f = fixture().await;
    let old = f.leases.clone();
    let job_only = old
        .iter()
        .find(|l| l.resource.starts_with("job:"))
        .unwrap()
        .clone();
    assert!(
        execution::supervise(
            f.state.pool.clone(),
            vec![job_only],
            record(&f.state.pool, &f.job, 1, Some(1), "saved")
        )
        .await
        .unwrap()
        .is_err()
    );
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    let mut tx = f.state.pool.begin().await.unwrap();
    let mut next = vec![];
    for l in &old {
        next.push(
            execution::acquire(&mut tx, &l.resource, "next", &f.job.id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    let before = snapshot(&f).await;
    assert!(
        execution::supervise(
            f.state.pool.clone(),
            old,
            record(&f.state.pool, &f.job, 1, Some(1), "saved")
        )
        .await
        .is_none()
    );
    assert_eq!(snapshot(&f).await, before);
    f.leases = next;
    let mut tx = f.state.pool.begin().await.unwrap();
    f.leases[0]
        .dispatch(&mut tx, "unknown", &json!({}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &f.job).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
}
