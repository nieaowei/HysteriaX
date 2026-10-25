use super::ownership_tests::{claim, count, enqueue, fixture, operation};
use super::*;
use crate::execution;

async fn failed(
    pool: &PgPool,
    claimed: &ClaimedJob,
    retry: bool,
) -> Option<Result<(), sqlx::Error>> {
    execution::supervise(
        pool.clone(),
        claimed.leases.clone(),
        fail(
            pool,
            &claimed.job,
            "transport failed",
            retry,
            false,
            false,
            true,
        ),
    )
    .await
}
async fn snapshot(pool: &PgPool) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('jobs',(SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j),'nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM job_events e))").fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn failure_and_retry_deadline_share_database_time_without_the_global_or_unrelated_job_lock() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "a").await;
    enqueue(&pool, "two", Some("node-b"), None).await;
    let legacy = db::hold_retired_global_write_lock(&pool).await.unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id='two' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), failed(&pool, &one, true))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(sqlx::query_scalar::<_,bool>("SELECT status='queued' AND stage='retry_wait' AND finished_at IS NULL AND available_at=updated_at+interval '30 seconds' AND updated_at=(SELECT created_at FROM job_events WHERE job_id='one' AND event_type='job.retrying') FROM jobs WHERE id='one'").fetch_one(&pool).await.unwrap());
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE id='two' AND status='queued' AND attempts=0"
        )
        .await,
        1
    );
    held.rollback().await.unwrap();
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn event_failure_rolls_back_job_node_and_error_then_native_conflict_commits_once() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    let before = snapshot(&pool).await;
    sqlx::raw_sql("CREATE FUNCTION reject_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.failed' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_failure BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_failure();").execute(&pool).await.unwrap();
    assert!(failed(&pool, &one, false).await.unwrap().is_err());
    assert_eq!(before, snapshot(&pool).await);
    sqlx::raw_sql("DROP TRIGGER reject_failure ON job_events; CREATE SEQUENCE failure_attempts; CREATE FUNCTION retry_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.failed' AND nextval('failure_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40P01',MESSAGE='retry'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_failure BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION retry_failure();").execute(&pool).await.unwrap();
    failed(&pool, &one, false).await.unwrap().unwrap();
    assert_eq!(
        count(&pool, "SELECT last_value FROM failure_attempts").await,
        2
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.failed'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM nodes WHERE id='node-a' AND state='sync_failed'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn wrong_job_source_attempt_node_payload_and_resource_are_rejected_atomically() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), Some("dns-zone:one")).await;
    let one = claim(&pool, "owner").await;
    enqueue(&pool, "two", Some("node-b"), None).await;
    let two = claim(&pool, "other").await;
    let before = snapshot(&pool).await;
    for mode in 0..6 {
        let mut bad = one.job.clone();
        match mode {
            0 => bad = two.job.clone(),
            1 => bad.attempts += 1,
            2 => bad.kind = "deploy".into(),
            3 => bad.node_id = Some("node-b".into()),
            4 => bad.target_revision = Some(99),
            _ => bad.payload = json!({"changed":true}),
        }
        assert!(
            execution::supervise(
                pool.clone(),
                one.leases.clone(),
                fail(&pool, &bad, "stale", false, false, false, false)
            )
            .await
            .unwrap()
            .is_err()
        );
        assert_eq!(before, snapshot(&pool).await);
    }
    let mut missing = one.leases.clone();
    missing.retain(|l| l.resource != "dns-zone:one");
    assert!(
        execution::supervise(
            pool.clone(),
            missing,
            fail(&pool, &one.job, "stale", false, false, false, false)
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(before, snapshot(&pool).await);
    sqlx::query("UPDATE jobs SET resource_key='dns-zone:new' WHERE id='one'")
        .execute(&pool)
        .await
        .unwrap();
    let before = snapshot(&pool).await;
    assert!(failed(&pool, &one, false).await.unwrap().is_err());
    assert_eq!(before, snapshot(&pool).await);
}

#[tokio::test]
async fn expiry_takeover_and_persisted_cancellation_prevent_late_failure() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "old").await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(failed(&pool, &one, false).await.is_none());
    let mut tx = pool.begin().await.unwrap();
    let mut leases = vec![];
    for l in &one.leases {
        leases.push(
            execution::acquire(&mut tx, &l.resource, "next", "one")
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    let before = snapshot(&pool).await;
    assert!(failed(&pool, &one, false).await.is_none());
    assert_eq!(before, snapshot(&pool).await);
    sqlx::query("UPDATE jobs SET status='cancelled',stage='node_removed' WHERE id='one'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        execution::supervise(
            pool.clone(),
            leases,
            fail(&pool, &one.job, "late", true, false, false, false)
        )
        .await
        .is_none()
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type!='job.started'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn uncertain_resource_or_attention_cannot_be_automatically_retried_or_downgraded() {
    for mode in 0..3 {
        let pool = fixture().await;
        enqueue(&pool, "one", Some("node-a"), None).await;
        let one = claim(&pool, "owner").await;
        let op = operation(&pool, &one).await;
        if mode == 0 {
            sqlx::query(
                "UPDATE execution_operations SET state='needs_attention' WHERE operation_id=$1",
            )
            .bind(&op)
            .execute(&pool)
            .await
            .unwrap();
        }
        if mode == 1 {
            sqlx::query("UPDATE jobs SET stage='needs_attention' WHERE id='one'")
                .execute(&pool)
                .await
                .unwrap();
        }
        if mode == 2 {
            sqlx::query("UPDATE execution_operations SET work_id='older-work',owner='old',generation=0 WHERE operation_id=$1").bind(&op).execute(&pool).await.unwrap();
        }
        let nodes: Value =
            sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n")
                .fetch_one(&pool)
                .await
                .unwrap();
        failed(&pool, &one, true).await.unwrap().unwrap();
        assert_eq!(
            nodes,
            sqlx::query_scalar::<_, Value>(
                "SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n"
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        );
        let (status, stage): (String, String) =
            sqlx::query_as("SELECT status,stage FROM jobs WHERE id='one'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "failed");
        assert_eq!(
            stage,
            if mode < 2 {
                "needs_attention"
            } else {
                "reconciling"
            }
        );
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM job_events WHERE event_type='job.retrying'"
            )
            .await,
            0
        );
        assert_eq!(count(&pool,"SELECT count(*) FROM execution_operations WHERE state IN ('dispatching','needs_attention')").await,1);
    }
}

#[tokio::test]
async fn concurrent_failure_observers_commit_one_terminal_event() {
    let pool = fixture().await;
    enqueue(&pool, "one", None, None).await;
    let one = claim(&pool, "owner").await;
    let (a, b) = tokio::join!(failed(&pool, &one, false), failed(&pool, &one, false));
    for result in [a, b].into_iter().flatten() {
        if let Err(e) = result {
            assert!(e.to_string().contains("running execution"), "{e}");
        }
    }
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.failed'"
        )
        .await,
        1
    );
}

async fn dns_fixture() -> (PgPool, ClaimedJob, String) {
    let (state, _, _, zone) = crate::dns::tests::fixture().await;
    let (_, axum::Json(receipt)) = crate::api::dns::create_record(
        axum::extract::State(state.clone()),
        axum::Json(crate::api::dns::CreateRecord {
            zone_id: zone,
            idempotency_key: "failure-test".into(),
            record: crate::dns::RecordInput {
                name: "failure.example.test".into(),
                record_type: "A".into(),
                content: "8.8.8.8".into(),
                ttl: 300,
                proxied: false,
            },
        }),
    )
    .await
    .unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    let claimed = claim(&state.pool, "dns-owner").await;
    assert_eq!(claimed.job.id, receipt["job_id"].as_str().unwrap());
    let record: String =
        sqlx::query_scalar("SELECT resource_id FROM dns_operations WHERE job_id=$1")
            .bind(&claimed.job.id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    (state.pool, claimed, record)
}

#[tokio::test]
async fn dns_failure_only_changes_the_same_unapplied_operation_and_desired_document() {
    for mode in 0..8 {
        let (pool, one, record) = dns_fixture().await;
        match mode {
            0 => {}
            1 => {
                sqlx::query("UPDATE dns_operations SET applied_at=clock_timestamp()")
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            2 => {
                sqlx::query("UPDATE dns_records SET desired=jsonb_set(desired,'{content}','\"9.9.9.9\"'),revision=revision+1").execute(&pool).await.unwrap();
            }
            3 => {
                sqlx::query("UPDATE dns_operations SET job_id=NULL")
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            5 => {
                sqlx::query("UPDATE dns_records SET revision=revision+1")
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            6 => {
                sqlx::query("UPDATE dns_operations SET payload=payload-'source_revision'")
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            7 => {
                let mut tx = pool.begin().await.unwrap();
                one.leases
                    .iter()
                    .find(|l| l.resource.starts_with("dns-zone:"))
                    .unwrap()
                    .dispatch(
                        &mut tx,
                        "lost-dns-response",
                        &json!({"request_digest":"fixed"}),
                    )
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            }
            _ => {
                sqlx::query("UPDATE dns_records SET desired=NULL,state='synced'")
                    .execute(&pool)
                    .await
                    .unwrap();
            }
        }
        failed(&pool, &one, false).await.unwrap().unwrap();
        let state: String = sqlx::query_scalar("SELECT state FROM dns_records WHERE id=$1")
            .bind(&record)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            state,
            match mode {
                0 => "failed",
                4 => "synced",
                _ => "pending",
            }
        );
    }
}

#[tokio::test]
async fn provider_retry_delay_is_separate_from_persisted_payload_and_uses_database_time() {
    let (pool, one, _) = dns_fixture().await;
    execution::supervise(
        pool.clone(),
        one.leases.clone(),
        fail_after(
            &pool,
            &one.job,
            "rate limited",
            true,
            false,
            false,
            false,
            Some(97),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(sqlx::query_scalar::<_,bool>("SELECT available_at=updated_at+interval '97 seconds' AND NOT (payload_json ? 'dns_retry_after') FROM jobs WHERE id=$1").bind(&one.job.id).fetch_one(&pool).await.unwrap());
}

#[tokio::test]
async fn explicit_dns_retry_moves_operation_ownership_and_can_fail_the_same_pending_document() {
    let (pool, one, record) = dns_fixture().await;
    failed(&pool, &one, false).await.unwrap().unwrap();
    finish_execution(&pool, &one.job, &one.leases)
        .await
        .unwrap();
    let state = AppState::new(
        pool.clone(),
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    let (_, axum::Json(receipt)) = crate::api::job_retries::retry(
        axum::extract::State(state),
        axum::extract::Path(one.job.id.clone()),
        axum::Json(crate::api::job_retries::RetryRequest {
            expected_revision: 1,
        }),
    )
    .await
    .unwrap();
    let child = claim(&pool, "next-owner").await;
    assert_eq!(child.job.id, receipt["job_id"].as_str().unwrap());
    assert_ne!(child.job.id, one.job.id);
    let owner: String = sqlx::query_scalar("SELECT job_id FROM dns_operations WHERE id=$1")
        .bind(child.job.payload["dns_operation_id"].as_str())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(owner, child.job.id);
    assert!(failed(&pool, &one, false).await.is_none());
    failed(&pool, &child, true).await.unwrap().unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM dns_records WHERE id=$1")
            .bind(record)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "pending"
    );
}

#[tokio::test]
async fn delete_intent_uses_the_delete_marker_and_keeps_retry_pending_before_terminal_failure() {
    let (pool, one, record) = dns_fixture().await;
    sqlx::query("UPDATE dns_records SET desired=NULL,state='synced',provider_record_id='remote-id' WHERE id=$1").bind(&record).execute(&pool).await.unwrap();
    sqlx::query("UPDATE dns_operations SET applied_at=clock_timestamp() WHERE job_id=$1")
        .bind(&one.job.id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET status='succeeded' WHERE id=$1")
        .bind(&one.job.id)
        .execute(&pool)
        .await
        .unwrap();
    finish_execution(&pool, &one.job, &one.leases)
        .await
        .unwrap();
    let state = AppState::new(
        pool.clone(),
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    let _ = crate::api::dns::delete_record(
        axum::extract::State(state),
        axum::extract::Path(record.clone()),
        axum::Json(crate::api::dns::ActionInput {
            expected_revision: 1,
            idempotency_key: "delete-failure-test".into(),
        }),
    )
    .await
    .unwrap();
    let delete = claim(&pool, "delete-owner").await;
    assert_eq!(delete.job.kind, "dns-record-delete");
    failed(&pool, &delete, true).await.unwrap().unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM dns_records WHERE id=$1")
            .bind(&record)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "pending"
    );
    finish_execution(&pool, &delete.job, &delete.leases)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET available_at=clock_timestamp() WHERE id=$1")
        .bind(&delete.job.id)
        .execute(&pool)
        .await
        .unwrap();
    let next = claim(&pool, "delete-next").await;
    assert_eq!(next.job.id, delete.job.id);
    assert_eq!(next.job.attempts, 2);
    failed(&pool, &next, false).await.unwrap().unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM dns_records WHERE id=$1")
            .bind(record)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "failed"
    );
}

#[tokio::test]
async fn identical_new_api_edit_is_a_different_source_and_old_failure_cannot_mark_it_failed() {
    let (pool, one, record) = dns_fixture().await;
    // Model refresh clearing the original pending document before its worker's
    // failure arrives. The old operation remains unapplied and has revision 1.
    sqlx::query("UPDATE dns_records SET remote_snapshot=desired,desired=NULL,state='synced',provider_record_id='remote-id' WHERE id=$1").bind(&record).execute(&pool).await.unwrap();
    let state = AppState::new(
        pool.clone(),
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    let (_, axum::Json(receipt)) = crate::api::dns::update_record(
        axum::extract::State(state),
        axum::extract::Path(record.clone()),
        axum::Json(crate::api::dns::UpdateRecord {
            expected_revision: 1,
            idempotency_key: "identical-new-edit".into(),
            record: crate::dns::RecordInput {
                name: "failure.example.test".into(),
                record_type: "A".into(),
                content: "8.8.8.8".into(),
                ttl: 300,
                proxied: false,
            },
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM dns_records WHERE id=$1")
            .bind(&record)
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    let pins: Vec<Value> = sqlx::query_scalar("SELECT payload->'source_revision' FROM dns_operations ORDER BY (payload->>'source_revision')::bigint").fetch_all(&pool).await.unwrap();
    assert_eq!(pins, vec![json!(1), json!(2)]);
    failed(&pool, &one, false).await.unwrap().unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM dns_records WHERE id=$1")
            .bind(&record)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "pending"
    );
    finish_execution(&pool, &one.job, &one.leases)
        .await
        .unwrap();
    let newer = claim(&pool, "new-edit-owner").await;
    assert_eq!(newer.job.id, receipt["job_id"].as_str().unwrap());
    failed(&pool, &newer, false).await.unwrap().unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM dns_records WHERE id=$1")
            .bind(&record)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "failed"
    );
}
