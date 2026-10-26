use super::*;
use crate::{api, execution, state::AppState};
use axum::{
    Json,
    extract::{Extension, Path, State},
};

pub(super) struct Fixture {
    pub(super) state: AppState,
    pub(super) job: crate::deployment::JobInput,
    pub(super) operation: String,
    pub(super) expected: Value,
    pub(super) leases: Vec<execution::Execution>,
}
pub(super) async fn fixture() -> Fixture {
    let (state, _, connection, _) = crate::dns::tests::fixture().await;
    let credential: String =
        sqlx::query_scalar("SELECT credential_id FROM dns_connections WHERE id=$1")
            .bind(connection)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let _receipt=api::credentials::publish(State(state.clone()),Extension(api::AdminActor{id:"fixture".into()}),Path(credential),Json(api::credentials::PublishVersion{expected_revision:1,payload:json!({"provider":"cloudflare","config":{"cloudflare_api_token":"new-fixture-token"}})})).await.unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    let (job, leases) = crate::jobs::claim_for_test(&state.pool, &state.instance_id)
        .await
        .unwrap();
    assert_eq!(job.kind, "dns-credential-apply");
    let operation = job.payload["dns_operation_id"].as_str().unwrap().to_owned();
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
        prepare(&f.state.pool, &f.job, &f.operation, &f.expected),
    )
    .await
}
async fn source(f: &Fixture) -> Source {
    match prepared(f).await.unwrap().unwrap() {
        Prepared::Ready(s) => s,
        Prepared::Applied(_) => panic!("already applied"),
    }
}
async fn save(f: &Fixture, s: &Source) -> Option<Result<Value>> {
    execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        commit(&f.state.pool, s),
    )
    .await
}
async fn snapshot(f: &Fixture) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('connections',(SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM dns_connections c),'items',(SELECT jsonb_agg(to_jsonb(i) ORDER BY batch_id,connection_id) FROM dns_credential_batch_items i),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM dns_operations o),'audits',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM audit_records a))").fetch_one(&f.state.pool).await.unwrap()
}
pub(super) async fn count(f: &Fixture, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&f.state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn publication_receipt_batch_and_audit_are_atomic_and_replay_is_readonly_without_the_old_lock()
 {
    let f = fixture().await;
    let s = source(&f).await;
    let legacy = crate::db::hold_retired_global_write_lock(&f.state.pool)
        .await
        .unwrap();
    let receipt = save(&f, &s).await.unwrap().unwrap();
    assert_eq!(receipt["credential_version"], 2);
    assert_eq!(count(&f,"SELECT count(*) FROM dns_connections WHERE credential_version=2 AND revision=2 AND status='verified'").await,1);
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM dns_credential_batch_items WHERE applied_at IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(count(&f,"SELECT count(*) FROM dns_operations WHERE action='credential-apply' AND applied_at IS NOT NULL").await,1);
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM audit_records WHERE action='dns.credential-apply'"
        )
        .await,
        1
    );
    let before = snapshot(&f).await;
    assert_eq!(save(&f, &s).await.unwrap().unwrap(), receipt);
    assert!(matches!(
        prepared(&f).await.unwrap().unwrap(),
        Prepared::Applied(_)
    ));
    assert_eq!(snapshot(&f).await, before);
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn audit_failure_rolls_back_publication_and_native_retry_commits_a_single_receipt() {
    let f = fixture().await;
    let s = source(&f).await;
    sqlx::raw_sql("CREATE FUNCTION reject_dns_rotation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='dns.credential-apply' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_dns_rotation BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_dns_rotation()")
        .execute(&f.state.pool).await.unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &s).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_dns_rotation ON audit_records; CREATE SEQUENCE rotation_attempt; CREATE FUNCTION retry_dns_rotation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='dns.credential-apply' AND nextval('rotation_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_dns_rotation BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_dns_rotation()")
        .execute(&f.state.pool).await.unwrap();
    save(&f, &s).await.unwrap().unwrap();
    assert_eq!(
        count(&f, "SELECT last_value FROM rotation_attempt").await,
        2
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM audit_records WHERE action='dns.credential-apply'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn changed_credential_connection_zone_batch_or_item_cannot_confirm_old_verification() {
    for query in [
        "UPDATE dns_connections SET revision=revision+1",
        "UPDATE dns_connections SET name='changed'",
        "UPDATE dns_connections SET credential_version=2",
        "UPDATE credentials SET archived=true WHERE kind='dns'",
        "UPDATE credentials SET latest_version=1 WHERE kind='dns'",
        "UPDATE credentials SET revision=revision+1 WHERE kind='dns'",
        "UPDATE dns_zones SET enabled=false,revision=revision+1",
        "INSERT INTO dns_zones(id,connection_id,provider_zone_id,name,enabled) SELECT 'new',id,'new-zone','new.example.test',true FROM dns_connections",
        "UPDATE credential_batches SET created_at=created_at+interval '1 second'",
        "UPDATE credential_batches SET version=1",
        "UPDATE dns_credential_batch_items SET expected_revision=expected_revision+1",
        "UPDATE dns_credential_batch_items SET applied_at=clock_timestamp()",
    ] {
        let f = fixture().await;
        let s = source(&f).await;
        sqlx::query(query).execute(&f.state.pool).await.unwrap();
        let before = snapshot(&f).await;
        assert!(save(&f, &s).await.unwrap().is_err(), "{query}");
        assert_eq!(snapshot(&f).await, before, "{query}");
    }
}

#[tokio::test]
async fn another_batch_job_or_version_cannot_borrow_the_publication_source() {
    for query in [
        "UPDATE dns_operations SET credential_version=1",
        "UPDATE dns_operations SET payload=jsonb_set(payload,'{version}','1')",
        "UPDATE dns_operations SET payload=payload-'expected_revision'",
        "UPDATE dns_operations SET resource_id='another'",
        "UPDATE dns_credential_batch_items SET job_id=(SELECT id FROM jobs WHERE kind<>'dns-credential-apply' LIMIT 1)",
    ] {
        let f = fixture().await;
        // A valid foreign job satisfies the FK but must not authorize this batch.
        if query.contains("SELECT id FROM jobs") {
            sqlx::query("INSERT INTO jobs(id,kind,status,stage,payload_json,available_at,created_at,updated_at) VALUES('foreign','fixture','queued','queued','{}',clock_timestamp(),clock_timestamp(),clock_timestamp())").execute(&f.state.pool).await.unwrap();
        }
        sqlx::query(query).execute(&f.state.pool).await.unwrap();
        let before = snapshot(&f).await;
        assert!(prepared(&f).await.unwrap().is_err(), "{query}");
        assert_eq!(snapshot(&f).await, before);
    }
}

#[tokio::test]
async fn unknown_remote_step_and_partial_scope_cannot_publish() {
    let f = fixture().await;
    let s = source(&f).await;
    let only_job = f
        .leases
        .iter()
        .find(|l| l.resource.starts_with("job:"))
        .unwrap()
        .clone();
    assert!(
        execution::supervise(
            f.state.pool.clone(),
            vec![only_job],
            commit(&f.state.pool, &s)
        )
        .await
        .unwrap()
        .is_err()
    );
    let mut tx = f.state.pool.begin().await.unwrap();
    s.leases
        .iter()
        .find(|l| l.resource.starts_with("dns-connection:"))
        .unwrap()
        .dispatch(&mut tx, "unknown", &json!({}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &s).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
}

#[tokio::test]
async fn takeover_rejects_old_generation_and_current_owner_can_confirm() {
    let mut f = fixture().await;
    let old = source(&f).await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&f.state.pool)
        .await
        .unwrap();
    let mut tx = f.state.pool.begin().await.unwrap();
    let mut next = vec![];
    for l in &f.leases {
        next.push(
            execution::acquire(&mut tx, &l.resource, "next", &f.job.id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    f.leases = next;
    let before = snapshot(&f).await;
    assert!(save(&f, &old).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
    let current = source(&f).await;
    save(&f, &current).await.unwrap().unwrap();
}

#[tokio::test]
async fn a_new_job_generation_cannot_reuse_an_observation_with_the_same_resource_generation() {
    let mut f = fixture().await;
    let old = source(&f).await;
    let job = f
        .leases
        .iter_mut()
        .find(|l| l.resource.starts_with("job:"))
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key=$1").bind(&job.resource).execute(&f.state.pool).await.unwrap();
    let mut tx = f.state.pool.begin().await.unwrap();
    *job = execution::acquire(&mut tx, &job.resource, &job.owner, &job.work)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let before = snapshot(&f).await;
    assert!(save(&f, &old).await.unwrap().is_err());
    assert_eq!(snapshot(&f).await, before);
    let current = source(&f).await;
    save(&f, &current).await.unwrap().unwrap();
}

#[tokio::test]
async fn provider_access_must_complete_and_a_committed_receipt_does_not_repeat_reads() {
    use axum::{Router, routing::get};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    for fail in [false, true] {
        let f = fixture().await;
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        let app = Router::new().fallback(get(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                if fail {
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        Json(json!({"success":false})),
                    )
                } else {
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({"success":true,"result":[],"result_info":{"total_pages":1}})),
                    )
                }
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider = Cloudflare::with_base(
            "fixture".into(),
            format!("http://{}", listener.local_addr().unwrap()),
        )
        .unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let before = snapshot(&f).await;
        let result = execution::supervise(
            f.state.pool.clone(),
            f.leases.clone(),
            apply(&f.state.pool, &provider, &f.job, &f.operation, &f.expected),
        )
        .await
        .unwrap();
        if fail {
            assert!(result.is_err());
            assert_eq!(snapshot(&f).await, before);
        } else {
            result.unwrap();
            assert_eq!(reads.load(Ordering::SeqCst), 3);
            let after = snapshot(&f).await;
            execution::supervise(
                f.state.pool.clone(),
                f.leases.clone(),
                apply(&f.state.pool, &provider, &f.job, &f.operation, &f.expected),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(reads.load(Ordering::SeqCst), 3);
            assert_eq!(snapshot(&f).await, after);
        }
        server.abort();
    }
}

#[tokio::test]
async fn actual_provider_read_holds_no_transaction_and_late_zone_change_rejects_publication() {
    use axum::{Router, routing::get};
    use std::sync::Arc;
    let f = fixture().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let (a, b) = (entered.clone(), resume.clone());
    let app = Router::new()
        .route(
            "/zones",
            get(move || {
                let (a, b) = (a.clone(), b.clone());
                async move {
                    a.notify_one();
                    b.notified().await;
                    Json(json!({"success":true,"result":[],"result_info":{"total_pages":1}}))
                }
            }),
        )
        .route(
            "/zones/remote-zone",
            get(|| async { Json(json!({"success":true,"result":{"id":"remote-zone"}})) }),
        )
        .route(
            "/zones/remote-zone/dns_records",
            get(|| async {
                Json(json!({"success":true,"result":[],"result_info":{"total_pages":1}}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = Cloudflare::with_base(
        "fixture".into(),
        format!("http://{}", listener.local_addr().unwrap()),
    )
    .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let verification = execution::supervise(
        f.state.pool.clone(),
        f.leases.clone(),
        apply(&f.state.pool, &provider, &f.job, &f.operation, &f.expected),
    );
    tokio::pin!(verification);
    tokio::select! {
        result=&mut verification=>panic!("verification stopped before provider read: {result:?}"),
        result=tokio::time::timeout(std::time::Duration::from_secs(5),entered.notified())=>result.unwrap(),
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        sqlx::query("UPDATE dns_zones SET enabled=false,revision=revision+1")
            .execute(&f.state.pool),
    )
    .await
    .unwrap()
    .unwrap();
    resume.notify_one();
    let result = verification.await;
    assert!(result.unwrap().is_err());
    server.abort();
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM dns_connections WHERE credential_version=1"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &f,
            "SELECT count(*) FROM dns_credential_batch_items WHERE applied_at IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(count(&f,"SELECT count(*) FROM dns_operations WHERE action='credential-apply' AND applied_at IS NOT NULL").await,0);
}
