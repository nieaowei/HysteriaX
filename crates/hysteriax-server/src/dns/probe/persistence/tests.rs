use super::*;
use crate::{execution, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
};

struct Fixture {
    state: AppState,
    job: JobInput,
    operation: String,
    expected: Value,
    leases: Vec<Execution>,
}
async fn fixture(cname: bool, expired: bool) -> Fixture {
    let (state, _, _, zone) = crate::dns::tests::fixture().await;
    sqlx::query("INSERT INTO dns_records(id,zone_id,provider_record_id,name,record_type,content,ttl,state) VALUES('record',$1,'remote','record.example.test',$2,$3,1,'synced')")
        .bind(&zone).bind(if cname {"CNAME"} else {"A"}).bind(if cname {"target.example.test"} else {"8.8.8.8"}).execute(&state.pool).await.unwrap();
    if cname {
        sqlx::query("INSERT INTO dns_records(id,zone_id,provider_record_id,name,record_type,content,ttl,state) VALUES('target',$1,'remote-target','target.example.test','A','8.8.8.8',1,'synced')")
            .bind(&zone).execute(&state.pool).await.unwrap();
    }
    let (_, Json(receipt)) = crate::api::dns::check_record(
        State(state.clone()),
        Path("record".into()),
        Json(crate::api::dns::ActionInput {
            expected_revision: 1,
            idempotency_key: "probe".into(),
        }),
    )
    .await
    .unwrap();
    let operation = receipt["operation_id"].as_str().unwrap().to_owned();
    if expired {
        sqlx::query("UPDATE dns_operations SET created_at=clock_timestamp()-interval '11 minutes' WHERE id=$1").bind(&operation).execute(&state.pool).await.unwrap();
    }
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    let (job, leases) = crate::jobs::claim_for_test(&state.pool, &state.instance_id)
        .await
        .unwrap();
    assert_eq!(job.id, receipt["job_id"].as_str().unwrap());
    let expected = sqlx::query_scalar(
        "SELECT to_jsonb(o)-'result'-'applied_at' FROM dns_operations o WHERE id=$1",
    )
    .bind(&operation)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    Fixture {
        state,
        job,
        operation,
        expected,
        leases,
    }
}
async fn prepared(f: &Fixture) -> Option<Result<Prepared>> {
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        prepare_job(&f.state.pool, &f.job, &f.operation, &f.expected),
    )
    .await
}
async fn work(f: &Fixture) -> Work {
    match prepared(f).await.unwrap().unwrap() {
        Prepared::Ready(w) => w,
        _ => panic!("fresh probe expected"),
    }
}
fn observation(record: &Record, status: &str) -> Value {
    let expected = record.content.trim_end_matches('.').to_ascii_lowercase();
    json!({"record_id":record.id,"status":status,"expected":expected,"management_answers":if status=="verified"{json!([expected])}else{json!([])},"authoritative":if status=="verified"{json!([{"server":"fixture","answers":[expected]}])}else{json!([])},"target_addresses":["8.8.8.8"],"management_addresses":["8.8.8.8"]})
}
async fn save(f: &Fixture, w: &Work, status: &str) -> Option<Result<Value>> {
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        save_job(&f.state.pool, w, &observation(&w.record, status)),
    )
    .await
}
async fn count(f: &Fixture, q: &str) -> i64 {
    sqlx::query_scalar(q)
        .fetch_one(&f.state.pool)
        .await
        .unwrap()
}
async fn snapshot(f: &Fixture) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('records',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM dns_records r),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM dns_operations o),'jobs',(SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j),'events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM job_events e),'leases',(SELECT jsonb_agg(to_jsonb(l)-'expires_at' ORDER BY resource_key) FROM resource_leases l),'audits',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM audit_records a))")
        .fetch_one(&f.state.pool).await.unwrap()
}

#[tokio::test]
async fn verified_observation_receipt_and_audit_commit_once_without_the_old_global_lock() {
    let f = fixture(false, false).await;
    let w = work(&f).await;
    assert_eq!(w.expected["payload"]["source_revision"], 1);
    let legacy = db::begin_write(&f.state.pool).await.unwrap();
    save(&f, &w, "verified").await.unwrap().unwrap();
    assert_eq!(count(&f,"SELECT count(*) FROM dns_records WHERE id='record' AND revision=1 AND resolution_status='verified' AND checked_at IS NOT NULL").await,1);
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM dns_operations WHERE applied_at IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM audit_records WHERE action='dns.record-check'"
        )
        .await,
        1
    );
    let before = snapshot(&f).await;
    save(&f, &w, "verified").await.unwrap().unwrap();
    assert_eq!(snapshot(&f).await, before);
    assert!(matches!(
        prepared(&f).await.unwrap().unwrap(),
        Prepared::Applied(_)
    ));
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn pending_observation_requeues_and_releases_every_lease_in_the_same_commit() {
    let mut f = fixture(false, false).await;
    let old = work(&f).await;
    let result = save(&f, &old, "pending").await;
    assert!(result.is_none_or(|r| r.is_ok()));
    assert_eq!(count(&f,"SELECT count(*) FROM jobs WHERE status='queued' AND stage='dns_propagation_wait' AND attempts=0 AND available_at BETWEEN clock_timestamp()+interval '25 seconds' AND clock_timestamp()+interval '31 seconds'").await,1);
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL OR work_id IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(count(&f,"SELECT count(*) FROM dns_records WHERE resolution_status='pending' AND checked_at IS NOT NULL").await,1);
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM job_events WHERE event_type='job.retrying'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM dns_operations WHERE applied_at IS NOT NULL"
        )
        .await,
        0
    );
    let before = snapshot(&f).await;
    assert!(save(&f, &old, "verified").await.is_none());
    assert_eq!(snapshot(&f).await, before);
    // Move only the fixture's eligibility clock; claim the next attempt through
    // the real production claimant, including its new lease generations.
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp()-interval '1 second'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    let (job, leases) = crate::jobs::claim_for_test(&f.state.pool, "next")
        .await
        .unwrap();
    assert_eq!(job.attempts, 1);
    assert!(leases.iter().all(|l| l.generation > 1));
    f.job = job;
    f.leases = leases;
    assert!(save(&f, &old, "verified").await.unwrap().is_err());
    let current = work(&f).await;
    save(&f, &current, "verified").await.unwrap().unwrap();
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM dns_operations WHERE applied_at IS NOT NULL"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn expired_pending_observation_fails_with_its_last_result_and_releases_resources() {
    let f = fixture(false, true).await;
    let w = work(&f).await;
    let result = save(&f, &w, "pending").await;
    assert!(result.is_none_or(|r| r.is_ok()));
    assert_eq!(count(&f,"SELECT count(*) FROM jobs WHERE status='failed' AND stage='failed' AND error_message LIKE 'DNS propagation check exceeded%'").await,1);
    assert_eq!(count(&f,"SELECT count(*) FROM dns_records WHERE state='synced' AND resolution_status='pending' AND checked_at IS NOT NULL").await,1);
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL OR work_id IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM job_events WHERE event_type='job.failed'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM dns_operations WHERE applied_at IS NOT NULL"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn a_current_proxied_cname_target_is_reported_as_proxied_instead_of_verified() {
    let f = fixture(true, false).await;
    sqlx::query("UPDATE dns_records SET proxied=true,revision=revision+1 WHERE id='target'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    let w = work(&f).await;
    assert!(w.record.target_proxied);
    save(&f, &w, "proxied").await.unwrap().unwrap();
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM dns_records WHERE id='record' AND resolution_status='proxied'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM dns_operations WHERE applied_at IS NOT NULL"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn retry_event_failure_rolls_back_observation_queue_and_release_then_native_conflict_commits_once()
 {
    let f = fixture(false, false).await;
    let w = work(&f).await;
    sqlx::raw_sql("CREATE FUNCTION reject_probe_retry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.retrying' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_probe_retry BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_probe_retry()")
        .execute(&f.state.pool).await.unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &w, "pending").await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_probe_retry ON job_events; CREATE SEQUENCE probe_retry_attempt; CREATE FUNCTION retry_probe_retry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.retrying' AND nextval('probe_retry_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_probe_retry BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION retry_probe_retry()")
        .execute(&f.state.pool).await.unwrap();
    let result = save(&f, &w, "pending").await;
    assert!(result.is_none_or(|r| r.is_ok()));
    assert_eq!(
        count(&f, "SELECT last_value FROM probe_retry_attempt").await,
        2
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM job_events WHERE event_type='job.retrying'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn audit_failure_rolls_back_verified_result_and_native_retry_commits_one_receipt() {
    let f = fixture(false, false).await;
    let w = work(&f).await;
    sqlx::raw_sql("CREATE FUNCTION reject_probe_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='dns.record-check' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_probe_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_probe_audit()")
        .execute(&f.state.pool).await.unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &w, "verified").await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_probe_audit ON audit_records; CREATE SEQUENCE probe_audit_attempt; CREATE FUNCTION retry_probe_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='dns.record-check' AND nextval('probe_audit_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40P01'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_probe_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_probe_audit()")
        .execute(&f.state.pool).await.unwrap();
    save(&f, &w, "verified").await.unwrap().unwrap();
    assert_eq!(
        count(&f, "SELECT last_value FROM probe_audit_attempt").await,
        2
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM audit_records WHERE action='dns.record-check'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn changed_record_zone_or_cname_proxy_dependency_cannot_save_or_authorize_an_old_observation()
{
    for change in [
        "UPDATE dns_records SET revision=revision+1 WHERE id='record'",
        "UPDATE dns_zones SET revision=revision+1 WHERE id='zone'",
        "UPDATE dns_records SET proxied=true,revision=revision+1 WHERE id='target'",
    ] {
        let f = fixture(true, false).await;
        let w = work(&f).await;
        sqlx::query(change).execute(&f.state.pool).await.unwrap();
        let before = snapshot(&f).await;
        assert!(validate(&f.state.pool, &w.record).await.is_err());
        assert!(save(&f, &w, "verified").await.unwrap().is_err());
        assert_eq!(snapshot(&f).await, before);
    }
}

#[tokio::test]
async fn readonly_readiness_validation_does_not_publish_observations_and_rejects_a_changed_source()
{
    let f = fixture(false, false).await;
    let before = snapshot(&f).await;
    let pool = f.state.pool.clone();
    super::super::inspect_with(&f.state.pool, "record", |record| async move {
        Ok(observation(&record, "verified"))
    })
    .await
    .unwrap();
    assert_eq!(snapshot(&f).await, before);
    let error = super::super::inspect_with(&f.state.pool, "record", |record| async move {
        sqlx::query(
            "UPDATE dns_records SET content='1.1.1.1',revision=revision+1 WHERE id='record'",
        )
        .execute(&pool)
        .await
        .unwrap();
        Ok(observation(&record, "verified"))
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("DNS probe source"));
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM dns_records WHERE checked_at IS NOT NULL"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn wrong_observation_scope_missing_pin_and_uncertain_execution_cannot_modify_probe_state() {
    let mut f = fixture(false, false).await;
    let w = work(&f).await;
    let mut wrong = observation(&w.record, "verified");
    wrong["record_id"] = json!("another");
    let before = snapshot(&f).await;
    assert!(
        execution::supervise(
            f.state.pool.clone(),
            f.leases.clone(),
            save_job(&f.state.pool, &w, &wrong)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(snapshot(&f).await, before);
    let partial = vec![
        f.leases
            .iter()
            .find(|l| l.resource.starts_with("job:"))
            .unwrap()
            .clone(),
    ];
    assert!(
        execution::supervise(
            f.state.pool.clone(),
            partial,
            prepare_job(&f.state.pool, &f.job, &f.operation, &f.expected)
        )
        .await
        .unwrap()
        .is_err()
    );
    sqlx::query("UPDATE dns_operations SET payload=payload-'source_revision' WHERE id=$1")
        .bind(&f.operation)
        .execute(&f.state.pool)
        .await
        .unwrap();
    f.expected = sqlx::query_scalar(
        "SELECT to_jsonb(o)-'result'-'applied_at' FROM dns_operations o WHERE id=$1",
    )
    .bind(&f.operation)
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    assert!(prepared(&f).await.unwrap().is_err());
    // Restore only the fixture's pin, then record an actual unknown dispatch.
    sqlx::query(
        "UPDATE dns_operations SET payload=jsonb_set(payload,'{source_revision}','1') WHERE id=$1",
    )
    .bind(&f.operation)
    .execute(&f.state.pool)
    .await
    .unwrap();
    f.expected = sqlx::query_scalar(
        "SELECT to_jsonb(o)-'result'-'applied_at' FROM dns_operations o WHERE id=$1",
    )
    .bind(&f.operation)
    .fetch_one(&f.state.pool)
    .await
    .unwrap();
    let mut tx = f.state.pool.begin().await.unwrap();
    f.leases
        .iter()
        .find(|l| l.resource.starts_with("dns-zone:"))
        .unwrap()
        .dispatch(&mut tx, "unknown", &json!({"digest":"old"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &w, "verified").await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
}

#[tokio::test]
async fn actual_executor_finishes_or_requeues_without_loading_provider_secrets_or_republishing_ended_work()
 {
    for (status, expired, expected, applied) in [
        ("verified", false, "succeeded", true),
        ("pending", false, "queued", false),
        ("pending", true, "failed", false),
    ] {
        let f = fixture(false, expired).await;
        // Model an unreadable stored credential only in this isolated fixture;
        // production credential versions remain immutable. A propagation check
        // requires DNS data, not a Cloudflare credential.
        sqlx::raw_sql("BEGIN; ALTER TABLE credential_versions DISABLE TRIGGER immutable_credential_version; UPDATE credential_versions v SET payload_enc='unavailable' FROM dns_connections c WHERE v.credential_id=c.credential_id AND v.version=c.credential_version; ALTER TABLE credential_versions ENABLE TRIGGER immutable_credential_version; COMMIT;")
            .execute(&f.state.pool).await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::super::OBSERVATION_STATUS_FOR_TEST.scope(
                status,
                crate::jobs::execute_for_test(f.state.clone(), f.job.clone(), f.leases.clone()),
            ),
        )
        .await
        .unwrap();
        let actual: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=$1")
            .bind(&f.job.id)
            .fetch_one(&f.state.pool)
            .await
            .unwrap();
        assert_eq!(actual, expected);
        let actual: bool =
            sqlx::query_scalar("SELECT applied_at IS NOT NULL FROM dns_operations WHERE id=$1")
                .bind(&f.operation)
                .fetch_one(&f.state.pool)
                .await
                .unwrap();
        assert_eq!(actual, applied);
        if expected != "succeeded" {
            assert_eq!(count(&f,"SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL OR work_id IS NOT NULL").await,0);
            assert_eq!(
                count(
                    &f,
                    "SELECT count(*) FROM job_events WHERE event_type='job.succeeded'"
                )
                .await,
                0
            );
        } else {
            assert_eq!(
                count(
                    &f,
                    "SELECT count(*) FROM job_events WHERE event_type='job.succeeded'"
                )
                .await,
                1
            );
        }
    }
}
