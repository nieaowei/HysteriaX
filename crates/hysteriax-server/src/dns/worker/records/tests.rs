use super::*;
use crate::{execution, state::AppState};

struct Fixture {
    state: AppState,
    job: JobInput,
    operation: String,
    id: String,
    payload: Value,
    leases: Vec<execution::Execution>,
}
async fn fixture() -> Fixture {
    let (state, _, _, zone) = crate::dns::tests::fixture().await;
    let (_, axum::Json(receipt)) = crate::api::dns::create_record(
        axum::extract::State(state.clone()),
        axum::Json(crate::api::dns::CreateRecord {
            zone_id: zone,
            idempotency_key: "guarded-create".into(),
            record: crate::dns::RecordInput {
                name: "guard.example.test".into(),
                record_type: "A".into(),
                content: "8.8.8.8".into(),
                ttl: 1,
                proxied: false,
            },
        }),
    )
    .await
    .unwrap();
    let id = receipt["resource_id"].as_str().unwrap().to_owned();
    let operation = receipt["operation_id"].as_str().unwrap().to_owned();
    let job_id = receipt["job_id"].as_str().unwrap().to_owned();
    sqlx::query("UPDATE jobs SET status='running',attempts=1 WHERE id=$1")
        .bind(&job_id)
        .execute(&state.pool)
        .await
        .unwrap();
    let row = sqlx::query("SELECT * FROM jobs WHERE id=$1")
        .bind(&job_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let job = JobInput {
        id: job_id,
        kind: row.get("kind"),
        node_id: row.get("node_id"),
        target_revision: row.get("target_revision"),
        payload: row.get("payload_json"),
        attempts: 1,
    };
    let payload = sqlx::query_scalar("SELECT payload FROM dns_operations WHERE id=$1")
        .bind(&operation)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let mut keys = vec![
        format!("job:{}", job.id),
        row.get::<String, _>("resource_key"),
    ];
    keys.sort();
    let mut leases = vec![];
    for key in keys {
        leases.push(
            execution::acquire(&mut tx, &key, &state.instance_id, &job.id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    Fixture {
        state,
        job,
        operation,
        id,
        payload,
        leases,
    }
}
async fn prepared(f: &Fixture) -> Result<Prepared> {
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        prepare(
            &f.state.pool,
            &f.job,
            &f.operation,
            "record-create",
            &f.id,
            &f.payload,
        ),
    )
    .await
    .unwrap()
}
async fn source(f: &Fixture) -> Source {
    match prepared(f).await.unwrap() {
        Prepared::Ready(s) => s,
        _ => panic!("pending source expected"),
    }
}
fn result(f: &Fixture) -> Value {
    let mut value = f.payload["desired"].clone();
    value["id"] = json!("remote-created");
    value
}
async fn save(f: &Fixture, s: &Source, value: &Value) -> Option<Result<Value>> {
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        commit(&f.state.pool, s, value),
    )
    .await
}
async fn snapshot(f: &Fixture) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('records',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM dns_records r),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM dns_operations o),'jobs',(SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j),'audits',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM audit_records a),'events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM job_events e))")
        .fetch_one(&f.state.pool).await.unwrap()
}
async fn count(f: &Fixture, q: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(q))
        .fetch_one(&f.state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn record_receipt_check_and_audit_commit_once_without_the_old_global_lock() {
    let f = fixture().await;
    let s = source(&f).await;
    let legacy = db::hold_retired_global_write_lock(&f.state.pool)
        .await
        .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT revision FROM dns_records WHERE id=$1")
        .bind(&f.id)
        .fetch_one(&f.state.pool)
        .await
        .unwrap();
    let receipt =
        tokio::time::timeout(std::time::Duration::from_secs(2), save(&f, &s, &result(&f)))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    assert_eq!(receipt, json!({"record_id":f.id,"deleted":false}));
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM jobs WHERE kind='dns-record-check'"
        )
        .await,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM dns_records WHERE id=$1")
            .bind(&f.id)
            .fetch_one(&f.state.pool)
            .await
            .unwrap(),
        before + 1
    );
    let saved = snapshot(&f).await;
    assert_eq!(save(&f, &s, &result(&f)).await.unwrap().unwrap(), receipt);
    assert_eq!(snapshot(&f).await, saved);
    assert!(matches!(prepared(&f).await.unwrap(), Prepared::Applied(_)));
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn an_identical_new_pending_document_is_not_the_old_operation_source() {
    let f = fixture().await;
    let s = source(&f).await;
    sqlx::query("UPDATE dns_records SET revision=revision+1 WHERE id=$1")
        .bind(&f.id)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let before = snapshot(&f).await;
    assert!(prepared(&f).await.is_err());
    assert!(save(&f, &s, &result(&f)).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
}

#[tokio::test]
async fn a_provider_response_for_different_desired_content_cannot_clear_the_pending_document() {
    let f = fixture().await;
    let s = source(&f).await;
    let mut response = result(&f);
    response["content"] = json!("1.1.1.1");
    let before = snapshot(&f).await;
    assert!(save(&f, &s, &response).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
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
async fn native_nested_enqueue_retry_and_audit_failure_preserve_one_atomic_record_publication() {
    let f = fixture().await;
    let s = source(&f).await;
    let before = snapshot(&f).await;
    sqlx::raw_sql("CREATE FUNCTION reject_record_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='dns.record-create' THEN RAISE EXCEPTION 'reject audit'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_record_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_record_audit()")
        .execute(&f.state.pool).await.unwrap();
    assert!(save(&f, &s, &result(&f)).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_record_audit ON audit_records; CREATE SEQUENCE check_attempt; CREATE FUNCTION retry_record_check() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='dns-record-check' AND nextval('check_attempt')=1 THEN RAISE EXCEPTION 'retry check' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_record_check BEFORE INSERT ON jobs FOR EACH ROW EXECUTE FUNCTION retry_record_check()")
        .execute(&f.state.pool).await.unwrap();
    save(&f, &s, &result(&f)).await.unwrap().unwrap();
    assert_eq!(count(&f, "SELECT last_value FROM check_attempt").await, 2);
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM jobs WHERE kind='dns-record-check'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM audit_records WHERE action='dns.record-create'"
        )
        .await,
        1
    );
    assert_eq!(count(&f,"SELECT count(*) FROM dns_operations WHERE action='record-create' AND applied_at IS NOT NULL").await,1);
}

#[tokio::test]
async fn wrong_job_input_or_transferred_operation_cannot_publish_a_result() {
    let f = fixture().await;
    let s = source(&f).await;
    sqlx::query("INSERT INTO dns_records(id,zone_id,name,record_type,content,ttl,proxied,origin,desired,state,revision) SELECT 'other-record',zone_id,'other.'||name,record_type,content,ttl,proxied,origin,desired,state,revision FROM dns_records WHERE id=$1")
        .bind(&f.id).execute(&f.state.pool).await.unwrap();
    let mut mismatched = s.clone();
    mismatched.record = "other-record".into();
    let before = snapshot(&f).await;
    assert!(save(&f, &mismatched, &result(&f)).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
    for mode in 0..5 {
        let mut wrong = s.clone();
        match mode {
            0 => wrong.job.attempts += 1,
            1 => wrong.job.payload = json!({}),
            2 => wrong.job.target_revision = Some(99),
            3 => wrong.job.kind = "dns-record-update".into(),
            _ => wrong.job.id = "another".into(),
        }
        let before = snapshot(&f).await;
        assert!(save(&f, &wrong, &result(&f)).await.unwrap().is_err());
        assert_eq!(snapshot(&f).await, before);
    }
    sqlx::query("UPDATE dns_operations SET job_id=NULL WHERE id=$1")
        .bind(&f.operation)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let before = snapshot(&f).await;
    assert!(prepared(&f).await.is_err());
    assert!(save(&f, &s, &result(&f)).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
}

#[tokio::test]
async fn no_revision_proof_or_changed_provider_binding_cannot_be_automatically_applied() {
    for mode in 0..3 {
        let mut f = fixture().await;
        let s = source(&f).await;
        match mode {
            0 => {
                f.payload.as_object_mut().unwrap().remove("source_revision");
                sqlx::query("UPDATE dns_operations SET payload=$1 WHERE id=$2")
                    .bind(&f.payload)
                    .bind(&f.operation)
                    .execute(&f.state.pool)
                    .await
                    .unwrap();
            }
            1 => {
                sqlx::query("UPDATE dns_zones SET provider_zone_id='different' WHERE id=$1")
                    .bind(&s.zone_id)
                    .execute(&f.state.pool)
                    .await
                    .unwrap();
            }
            _ => {
                sqlx::query("UPDATE dns_records SET provider_record_id='different' WHERE id=$1")
                    .bind(&f.id)
                    .execute(&f.state.pool)
                    .await
                    .unwrap();
            }
        }
        let before = snapshot(&f).await;
        assert!(save(&f, &s, &result(&f)).await.unwrap().is_err());
        if mode == 0 {
            assert!(prepared(&f).await.is_err());
        }
        assert_eq!(snapshot(&f).await, before);
    }
}

#[tokio::test]
async fn matching_remote_value_does_not_resolve_an_unverified_previous_request() {
    let f = fixture().await;
    let s = source(&f).await;
    let lease = f
        .leases
        .iter()
        .find(|l| l.resource.starts_with("dns-zone:"))
        .unwrap();
    let mut tx = f.state.pool.begin().await.unwrap();
    lease
        .dispatch(&mut tx, "lost-response", &json!({"request_digest":"fixed"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = snapshot(&f).await;
    assert!(prepared(&f).await.is_err());
    assert!(save(&f, &s, &result(&f)).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
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
async fn superseded_generation_cannot_commit_but_the_current_owner_can_finish_the_same_pending_source()
 {
    let mut f = fixture().await;
    let old = source(&f).await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET attempts=2 WHERE id=$1")
        .bind(&f.job.id)
        .execute(&f.state.pool)
        .await
        .unwrap();
    let mut tx = f.state.pool.begin().await.unwrap();
    let mut leases = vec![];
    for lease in &f.leases {
        leases.push(
            execution::acquire(&mut tx, &lease.resource, "next", &f.job.id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &old, &result(&f)).await.is_none());
    assert_eq!(snapshot(&f).await, before);
    f.job.attempts = 2;
    f.leases = leases;
    assert!(save(&f, &old, &result(&f)).await.unwrap().is_err());
    let current = source(&f).await;
    save(&f, &current, &result(&f)).await.unwrap().unwrap();
    assert_eq!(count(&f,"SELECT count(*) FROM dns_operations WHERE action='record-create' AND applied_at IS NOT NULL").await,1);
}

#[tokio::test]
async fn the_actual_worker_does_not_send_or_accept_a_matching_value_after_source_change_or_uncertainty()
 {
    use crate::dns::worker::tests::{Remote, provider};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    for uncertain in [false, true] {
        let f = fixture().await;
        let mut remote_value = result(&f);
        remote_value["comment"] = json!(format!("HysteriaX operation:{}", f.operation));
        let remote = Remote {
            records: Arc::new(Mutex::new(vec![remote_value])),
            writes: Arc::new(AtomicUsize::new(0)),
        };
        let (provider, server) = provider(remote.clone()).await;
        if uncertain {
            let mut tx = f.state.pool.begin().await.unwrap();
            f.leases
                .iter()
                .find(|l| l.resource.starts_with("dns-zone:"))
                .unwrap()
                .dispatch(
                    &mut tx,
                    "unknown-provider-request",
                    &json!({"digest":"old"}),
                )
                .await
                .unwrap();
            tx.commit().await.unwrap();
        } else {
            sqlx::query("UPDATE dns_records SET revision=revision+1 WHERE id=$1")
                .bind(&f.id)
                .execute(&f.state.pool)
                .await
                .unwrap();
        }
        let before = snapshot(&f).await;
        let error = execution::supervise(
            f.state.pool.clone(),
            f.leases.clone(),
            super::super::write_record(
                &f.state.pool,
                &provider,
                &f.job,
                &f.operation,
                "record-create",
                &f.id,
                &f.payload,
            ),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.to_string().contains(if uncertain {
            "unverified remote step"
        } else {
            "pending source changed"
        }));
        assert_eq!(remote.writes.load(Ordering::SeqCst), 0);
        assert_eq!(snapshot(&f).await, before);
        server.abort();
    }
}
