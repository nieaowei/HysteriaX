use super::*;
use crate::{execution, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
};

#[derive(Clone)]
pub(in crate::dns::worker) struct Work {
    state: AppState,
    job: JobInput,
    operation: String,
    expected: Value,
    pub(in crate::dns::worker) leases: Vec<Execution>,
}
pub(in crate::dns::worker) async fn start(state: &AppState, receipt: &Value) -> Work {
    let id = receipt["job_id"].as_str().unwrap();
    sqlx::query("UPDATE jobs SET status='running',attempts=1 WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
    let row = sqlx::query("SELECT * FROM jobs WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let job = JobInput {
        id: id.into(),
        kind: row.get("kind"),
        node_id: row.get("node_id"),
        target_revision: row.get("target_revision"),
        payload: row.get("payload_json"),
        attempts: 1,
    };
    let operation = receipt["operation_id"].as_str().unwrap().to_owned();
    let expected = sqlx::query_scalar(
        "SELECT to_jsonb(o)-'result'-'applied_at' FROM dns_operations o WHERE id=$1",
    )
    .bind(&operation)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let mut keys = vec![format!("job:{id}"), row.get::<String, _>("resource_key")];
    keys.sort();
    let mut tx = state.pool.begin().await.unwrap();
    let mut leases = vec![];
    for key in keys {
        leases.push(
            execution::acquire(&mut tx, &key, &state.instance_id, id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    Work {
        state: state.clone(),
        job,
        operation,
        expected,
        leases,
    }
}
async fn enqueue_zone(state: &AppState, id: &str) -> Value {
    let revision = sqlx::query_scalar("SELECT revision FROM dns_zones WHERE id=$1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    crate::api::dns::refresh_zone(
        State(state.clone()),
        Path(id.into()),
        Json(crate::api::dns::ActionInput {
            expected_revision: revision,
            idempotency_key: Uuid::new_v4().to_string(),
        }),
    )
    .await
    .unwrap()
    .1
    .0
}
pub(in crate::dns::worker) async fn refresh_zone_for_test(
    state: &AppState,
    provider: &Cloudflare,
    id: &str,
) -> Value {
    let receipt = enqueue_zone(state, id).await;
    let work = start(state, &receipt).await;
    execution::supervise(
        state.pool.clone(),
        work.leases.clone(),
        zone(
            &state.pool,
            provider,
            &work.job,
            &work.operation,
            &work.expected,
        ),
    )
    .await
    .unwrap()
    .unwrap()
}
async fn fixture(zone: bool) -> Work {
    let (state, _, connection, id) = crate::dns::tests::fixture().await;
    let receipt = if zone {
        enqueue_zone(&state, &id).await
    } else {
        crate::api::dns::refresh_connection(
            State(state.clone()),
            Path(connection),
            Json(crate::api::dns::ActionInput {
                expected_revision: 1,
                idempotency_key: "refresh-connection".into(),
            }),
        )
        .await
        .unwrap()
        .1
        .0
    };
    start(&state, &receipt).await
}
#[tokio::test]
async fn another_zone_of_the_same_connection_can_commit_while_the_first_zone_is_locked() {
    let w = fixture(true).await;
    let first = source(&w).await;
    sqlx::query("INSERT INTO dns_zones(id,connection_id,provider_zone_id,name,enabled) VALUES('other-zone',$1,'other-remote','other.test',true)")
        .bind(&first.connection).execute(&w.state.pool).await.unwrap();
    let receipt = enqueue_zone(&w.state, "other-zone").await;
    let other = start(&w.state, &receipt).await;
    let s = source(&other).await;
    let mut held = w.state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM dns_connections WHERE id=$1 FOR SHARE")
        .bind(&first.connection)
        .fetch_one(&mut *held)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM dns_zones WHERE id='zone' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let mut observed = record("new");
    observed["name"] = json!("new.other.test");
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        save(&other, &s, &[observed]),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(count(&other,"SELECT count(*) FROM dns_records WHERE zone_id='other-zone' AND provider_record_id='new'").await,1);
    held.rollback().await.unwrap();
}

async fn source(w: &Work) -> Source {
    match execution::supervise(
        w.state.pool.clone(),
        w.leases.clone(),
        prepare(&w.state.pool, &w.job, &w.operation, &w.expected),
    )
    .await
    .unwrap()
    .unwrap()
    {
        Prepared::Ready(s) => s,
        _ => panic!("pending refresh expected"),
    }
}
async fn save(w: &Work, s: &Source, records: &[Value]) -> Option<Result<Value>> {
    execution::supervise(w.state.pool.clone(), w.leases.clone(), async {
        if s.zone.is_some() {
            commit_zone(&w.state.pool, s, records).await
        } else {
            commit_connection(&w.state.pool, s, records).await
        }
    })
    .await
}
async fn snapshot(w: &Work) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('connections',(SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM dns_connections c),'zones',(SELECT jsonb_agg(to_jsonb(z) ORDER BY id) FROM dns_zones z),'records',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM dns_records r),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM dns_operations o),'audits',(SELECT jsonb_agg(to_jsonb(a) ORDER BY id) FROM audit_records a))")
        .fetch_one(&w.state.pool).await.unwrap()
}
async fn count(w: &Work, q: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(q))
        .fetch_one(&w.state.pool)
        .await
        .unwrap()
}
fn record(id: &str) -> Value {
    json!({"id":id,"type":"A","name":format!("{id}.example.test"),"content":"8.8.8.8","ttl":1,"proxied":false})
}

#[tokio::test]
async fn connection_refresh_is_atomic_idempotent_and_does_not_overwrite_another_connections_zone() {
    let w = fixture(false).await;
    let s = source(&w).await;
    sqlx::query("INSERT INTO dns_connections(id,name,provider,credential_id,credential_version,status) SELECT 'foreign','Foreign',provider,credential_id,credential_version,status FROM dns_connections WHERE id=$1").bind(&s.connection).execute(&w.state.pool).await.unwrap();
    sqlx::raw_sql("INSERT INTO dns_zones(id,connection_id,provider_zone_id,name,enabled) VALUES('foreign-zone','foreign','foreign-remote','foreign.test',true)").execute(&w.state.pool).await.unwrap();
    let before: Value =
        sqlx::query_scalar("SELECT to_jsonb(z) FROM dns_zones z WHERE id='foreign-zone'")
            .fetch_one(&w.state.pool)
            .await
            .unwrap();
    let zones = vec![
        json!({"id":"remote-zone","name":"renamed.test"}),
        json!({"id":"new-zone","name":"new.test"}),
        json!({"id":"foreign-remote","name":"hijacked.test"}),
    ];
    save(&w, &s, &zones).await.unwrap().unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, Value>(
            "SELECT to_jsonb(z) FROM dns_zones z WHERE id='foreign-zone'"
        )
        .fetch_one(&w.state.pool)
        .await
        .unwrap(),
        before
    );
    assert_eq!(
        count(
            &w,
            "SELECT count(*) FROM dns_zones WHERE provider_zone_id='new-zone'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &w,
            "SELECT count(*) FROM dns_zones WHERE id='zone' AND name='renamed.test' AND revision=2"
        )
        .await,
        1
    );
    let applied = snapshot(&w).await;
    save(&w, &s, &zones).await.unwrap().unwrap();
    assert_eq!(snapshot(&w).await, applied);
    assert_eq!(
        count(
            &w,
            "SELECT count(*) FROM audit_records WHERE action='dns.connection-refresh'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn zone_refresh_publishes_records_missing_markers_receipt_and_audit_together_with_native_retry()
 {
    let w = fixture(true).await;
    let s = source(&w).await;
    sqlx::raw_sql("INSERT INTO dns_records(id,zone_id,provider_record_id,name,record_type,content,state) VALUES('gone','zone','gone','gone.example.test','A','1.1.1.1','synced')").execute(&w.state.pool).await.unwrap();
    let before = snapshot(&w).await;
    sqlx::raw_sql("CREATE FUNCTION reject_refresh_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='dns.zone-refresh' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_refresh_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_refresh_audit()")
        .execute(&w.state.pool).await.unwrap();
    assert!(save(&w, &s, &[record("new")]).await.unwrap().is_err());
    assert_eq!(snapshot(&w).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_refresh_audit ON audit_records; CREATE SEQUENCE refresh_attempt; CREATE FUNCTION retry_refresh_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.applied_at IS NOT NULL AND OLD.applied_at IS NULL AND nextval('refresh_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40P01'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_refresh_receipt BEFORE UPDATE ON dns_operations FOR EACH ROW EXECUTE FUNCTION retry_refresh_receipt()")
        .execute(&w.state.pool).await.unwrap();
    save(&w, &s, &[record("new")]).await.unwrap().unwrap();
    assert_eq!(count(&w, "SELECT last_value FROM refresh_attempt").await, 2);
    assert_eq!(
        count(
            &w,
            "SELECT count(*) FROM dns_records WHERE provider_record_id='new'"
        )
        .await,
        1
    );
    assert_eq!(count(&w,"SELECT count(*) FROM dns_records WHERE id='gone' AND state='remote_missing' AND deleted_at IS NOT NULL").await,1);
    let applied = snapshot(&w).await;
    save(&w, &s, &[record("new")]).await.unwrap().unwrap();
    assert_eq!(snapshot(&w).await, applied);
    assert_eq!(
        count(
            &w,
            "SELECT count(*) FROM audit_records WHERE action='dns.zone-refresh'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn changed_connection_zone_or_job_cannot_publish_a_stale_refresh() {
    for change in [
        "UPDATE dns_connections SET revision=revision+1",
        "UPDATE dns_zones SET revision=revision+1 WHERE id='zone'",
        "UPDATE jobs SET payload_json='{}' WHERE kind='dns-zone-refresh'",
        "UPDATE dns_zones SET enabled=false WHERE id='zone'",
    ] {
        let w = fixture(true).await;
        let s = source(&w).await;
        sqlx::query(change).execute(&w.state.pool).await.unwrap();
        let before = snapshot(&w).await;
        assert!(save(&w, &s, &[record("new")]).await.unwrap().is_err());
        assert_eq!(snapshot(&w).await, before);
    }
}

#[tokio::test]
async fn takeover_fences_an_old_prepared_refresh_and_the_current_owner_can_finish() {
    let mut w = fixture(true).await;
    let old = source(&w).await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&w.state.pool)
        .await
        .unwrap();
    let mut tx = w.state.pool.begin().await.unwrap();
    let mut leases = vec![];
    for lease in &w.leases {
        leases.push(
            execution::acquire(&mut tx, &lease.resource, "next", &w.job.id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    assert!(save(&w, &old, &[record("new")]).await.is_none());
    w.leases = leases;
    assert!(save(&w, &old, &[record("new")]).await.unwrap().is_err());
    let current = source(&w).await;
    save(&w, &current, &[record("new")]).await.unwrap().unwrap();
    assert_eq!(
        count(
            &w,
            "SELECT count(*) FROM dns_records WHERE provider_record_id='new'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn returning_a_missing_record_with_the_same_snapshot_still_advances_its_business_revision() {
    let w = fixture(true).await;
    let s = source(&w).await;
    let observed = record("returned");
    sqlx::query("INSERT INTO dns_records(id,zone_id,provider_record_id,name,record_type,content,ttl,proxied,remote_snapshot,state,deleted_at,revision) VALUES('returned','zone','returned','returned.example.test','A','8.8.8.8',1,false,$1,'remote_missing',clock_timestamp(),5)")
        .bind(&observed).execute(&w.state.pool).await.unwrap();
    save(&w, &s, &[observed]).await.unwrap().unwrap();
    assert!(sqlx::query_scalar::<_,bool>("SELECT revision=6 AND deleted_at IS NULL AND state='synced' AND resolution_status='unchecked' FROM dns_records WHERE id='returned'").fetch_one(&w.state.pool).await.unwrap());
}

#[tokio::test]
async fn an_unverified_provider_request_cannot_be_resolved_by_refreshing_matching_values() {
    let w = fixture(true).await;
    let s = source(&w).await;
    let mut tx = w.state.pool.begin().await.unwrap();
    s.lease
        .dispatch(&mut tx, "unknown-request", &json!({"digest":"old"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before = snapshot(&w).await;
    assert!(save(&w, &s, &[record("same")]).await.unwrap().is_err());
    let admitted = execution::supervise(
        w.state.pool.clone(),
        w.leases.clone(),
        prepare(&w.state.pool, &w.job, &w.operation, &w.expected),
    )
    .await;
    assert!(admitted.unwrap().is_err());
    assert_eq!(snapshot(&w).await, before);
}

#[tokio::test]
async fn terminal_reconciliation_requires_the_exact_pending_revision_and_retains_older_intent_history()
 {
    let (state, _, _, zone) = crate::dns::tests::fixture().await;
    let (_, Json(receipt)) = crate::api::dns::create_record(
        State(state.clone()),
        Json(crate::api::dns::CreateRecord {
            zone_id: zone.clone(),
            idempotency_key: "pending".into(),
            record: crate::dns::RecordInput {
                name: "pending.example.test".into(),
                record_type: "A".into(),
                content: "8.8.8.8".into(),
                ttl: 1,
                proxied: false,
            },
        }),
    )
    .await
    .unwrap();
    let previous = start(&state, &receipt).await;
    let id = receipt["resource_id"].as_str().unwrap();
    let old_job = receipt["job_id"].as_str().unwrap();
    let old_operation = receipt["operation_id"].as_str().unwrap();
    sqlx::query("UPDATE jobs SET status='failed',stage='failed' WHERE id=$1")
        .bind(old_job)
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    for lease in &previous.leases {
        lease.release(&mut tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    let refresh_receipt = enqueue_zone(&state, &zone).await;
    let w = start(&state, &refresh_receipt).await;
    let s = source(&w).await;
    let mut remote = record("remote-pending");
    remote["name"] = json!("pending.example.test");
    remote["comment"] = json!(format!("HysteriaX operation:{old_operation}"));
    // An unrelated historical operation must not be marked applied together.
    sqlx::query("INSERT INTO dns_operations(id,idempotency_key,request_sha256,job_id,connection_id,credential_version,resource_type,resource_id,action,payload) SELECT 'older','older','older',job_id,connection_id,credential_version,resource_type,resource_id,action,jsonb_set(payload,'{source_revision}','0') FROM dns_operations WHERE id=$1")
        .bind(old_operation).execute(&state.pool).await.unwrap();
    save(&w, &s, &[remote]).await.unwrap().unwrap();
    let result: Value = sqlx::query_scalar("SELECT result FROM dns_operations WHERE id=$1")
        .bind(old_operation)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(result["terminal_job_id"], old_job);
    assert_eq!(result["refresh_operation_id"], w.operation);
    assert_eq!(
        count(
            &w,
            "SELECT count(*) FROM dns_operations WHERE id='older' AND applied_at IS NULL"
        )
        .await,
        1
    );
    assert!(
        sqlx::query_scalar::<_, bool>(
            "SELECT desired IS NULL AND state='synced' FROM dns_records WHERE id=$1"
        )
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    );
}

#[tokio::test]
async fn active_or_unproven_pending_sources_survive_an_observation_of_the_same_value() {
    for mode in 0..4 {
        let (state, _, _, zone) = crate::dns::tests::fixture().await;
        let (_, Json(receipt)) = crate::api::dns::create_record(
            State(state.clone()),
            Json(crate::api::dns::CreateRecord {
                zone_id: zone.clone(),
                idempotency_key: "pending".into(),
                record: crate::dns::RecordInput {
                    name: "pending.example.test".into(),
                    record_type: "A".into(),
                    content: "8.8.8.8".into(),
                    ttl: 1,
                    proxied: false,
                },
            }),
        )
        .await
        .unwrap();
        let id = receipt["resource_id"].as_str().unwrap();
        let old_op = receipt["operation_id"].as_str().unwrap();
        let previous = if mode == 1 || mode == 2 {
            Some(start(&state, &receipt).await)
        } else {
            None
        };
        if mode > 0 {
            sqlx::query("UPDATE jobs SET status='failed',stage='failed' WHERE id=$1")
                .bind(receipt["job_id"].as_str())
                .execute(&state.pool)
                .await
                .unwrap();
        }
        if mode == 1 {
            sqlx::query("UPDATE dns_records SET revision=revision+1 WHERE id=$1")
                .bind(id)
                .execute(&state.pool)
                .await
                .unwrap();
        }
        if mode == 2 {
            sqlx::query("UPDATE dns_operations SET payload=payload-'source_revision' WHERE id=$1")
                .bind(old_op)
                .execute(&state.pool)
                .await
                .unwrap();
        }
        if let Some(previous) = previous {
            let mut tx = state.pool.begin().await.unwrap();
            for lease in &previous.leases {
                lease.release(&mut tx).await.unwrap();
            }
            tx.commit().await.unwrap();
        }
        let refresh_receipt = enqueue_zone(&state, &zone).await;
        let w = start(&state, &refresh_receipt).await;
        let s = source(&w).await;
        let before: Value = sqlx::query_scalar("SELECT to_jsonb(r) FROM dns_records r WHERE id=$1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let mut remote = record("remote-pending");
        remote["name"] = json!("pending.example.test");
        remote["comment"] = json!(format!("HysteriaX operation:{old_op}"));
        save(&w, &s, &[remote]).await.unwrap().unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, Value>("SELECT to_jsonb(r) FROM dns_records r WHERE id=$1")
                .bind(id)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            before
        );
        assert!(
            sqlx::query_scalar::<_, bool>(
                "SELECT applied_at IS NULL FROM dns_operations WHERE id=$1"
            )
            .bind(old_op)
            .fetch_one(&state.pool)
            .await
            .unwrap()
        );
    }
}

#[tokio::test]
async fn actual_provider_read_holds_no_catalog_transaction_and_a_later_edit_rejects_the_response() {
    use std::sync::Arc;
    let w = fixture(true).await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (handler_entered, handler_release) = (entered.clone(), release.clone());
    let app=axum::Router::new().route("/zones/remote-zone/dns_records",axum::routing::get(move || {
        let (entered,release)=(handler_entered.clone(),handler_release.clone());
        async move {entered.notify_one();release.notified().await;Json(json!({"success":true,"result":[record("new")],"result_info":{"total_pages":1}}))}
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = Cloudflare::with_base(
        "fixture-token".into(),
        format!("http://{}", listener.local_addr().unwrap()),
    )
    .unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let current = w.clone();
    let worker = tokio::spawn(async move {
        execution::supervise(
            current.state.pool.clone(),
            current.leases.clone(),
            zone(
                &current.state.pool,
                &provider,
                &current.job,
                &current.operation,
                &current.expected,
            ),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        sqlx::query("UPDATE dns_zones SET revision=revision+1 WHERE id='zone'")
            .execute(&w.state.pool),
    )
    .await
    .unwrap()
    .unwrap();
    let before = snapshot(&w).await;
    release.notify_one();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(result.to_string().contains("DNS zone changed"));
    assert_eq!(snapshot(&w).await, before);
    server.abort();
}
