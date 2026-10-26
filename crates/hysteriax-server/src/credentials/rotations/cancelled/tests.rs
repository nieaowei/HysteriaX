use super::*;
use crate::{
    api::{AdminActor, managed_mtls},
    execution,
};
use axum::{
    Json,
    extract::{Extension, Path, State},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub(in crate::credentials::rotations) async fn fixture(n: i32) -> (AppState, String, String) {
    let (state, ca, rotation) = super::super::trust::tests::fixture(n).await;
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,payload_json,status,stage,available_at,created_at,updated_at) SELECT 'old-'||id,'deploy',id,1,'{\"mtls_operation\":true}','queued','queued',now(),now(),now() FROM nodes").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_node_operations(job_id,node_id,ca_id,ca_version,rotation_id,stage,desired_revision,base_revision,base_digest,created_at) SELECT j.id,j.node_id,$1,2,$2,'trust',1,1,'fixture-digest',now() FROM jobs j").bind(&ca).bind(&rotation).execute(&state.pool).await.unwrap();
    (state, ca, rotation)
}
async fn replace(
    state: &AppState,
    ca: &str,
    rotation: &str,
) -> Result<Value, crate::error::ApiError> {
    managed_mtls::publish(
        State(state.clone()),
        Extension(AdminActor { id: "test".into() }),
        Path(ca.into()),
        Json(managed_mtls::PublishAuthority {
            expected_revision: 2,
            certificate: None,
            private_key: None,
            replace_rotation_id: Some(rotation.into()),
            idempotency_key: "replace-old-jobs".into(),
        }),
    )
    .await
    .map(|(_, Json(v))| v)
}
async fn count(state: &AppState, q: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(q))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn claim(state: &AppState, owner: &str) -> Work {
    sqlx::query(
        "UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1 AND state='queued'",
    )
    .bind(KIND)
    .execute(&state.pool)
    .await
    .unwrap();
    maintenance::claim(&state.pool, KIND, owner)
        .await
        .unwrap()
        .unwrap()
}
const SNAPSHOTS: &str = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'history',(SELECT jsonb_agg(to_jsonb(c) ORDER BY node_id,revision) FROM config_versions c),'operations',(SELECT jsonb_agg(to_jsonb(o)-'pending_job' ORDER BY job_id) FROM mtls_node_operations o))";

#[tokio::test]
async fn replacement_does_not_lock_any_node_or_job_and_cancellation_is_atomic_in_hundred_item_pages()
 {
    let (state, ca, rotation) = fixture(301).await;
    let before: Value = sqlx::query_scalar(SNAPSHOTS)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut held = state.pool.begin().await.unwrap();
    for q in [
        "SELECT id FROM nodes ORDER BY id FOR UPDATE",
        "SELECT id FROM jobs ORDER BY id FOR UPDATE",
        "SELECT job_id FROM mtls_node_operations ORDER BY job_id FOR UPDATE",
    ] {
        sqlx::query(q).fetch_all(&mut *held).await.unwrap();
    }
    let receipt = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        replace(&state, &ca, &rotation),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(receipt["version"], 3);
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='queued'").await,
        301
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_superseded_node_jobs").await,
        301
    );
    let work = claim(&state, "busy").await;
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-superseded-node-jobs' AND state='complete'").await,0);
    held.rollback().await.unwrap();
    let legacy = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    for expected in [100, 200, 201, 301] {
        let work = claim(&state, "cancel").await;
        page(&state.pool, &work).await.unwrap();
        assert_eq!(count(&state,"SELECT count(*) FROM jobs WHERE status='cancelled' AND stage='ca_target_superseded'").await,expected);
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM job_events WHERE event_type='job.cancelled'"
            )
            .await,
            expected
        );
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_node_operations WHERE pending_job"
            )
            .await,
            301 - expected
        );
    }
    legacy.rollback().await.unwrap();
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(SNAPSHOTS)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-superseded-node-jobs' AND state='complete'").await,1);
}

#[tokio::test]
async fn busy_node_and_job_rows_wrap_without_blocking_other_cancellations() {
    let (state, ca, rotation) = fixture(102).await;
    replace(&state, &ca, &rotation).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='member-0001' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id='old-member-0002' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    for expected in [98, 100] {
        let work = claim(&state, "skip").await;
        page(&state.pool, &work).await.unwrap();
        assert_eq!(
            count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
            expected
        );
    }
    held.rollback().await.unwrap();
    let work = claim(&state, "wrap").await;
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        102
    );
}

#[tokio::test]
async fn acknowledgement_and_event_failures_roll_back_status_flags_cursor_and_events_then_native_retry_commits_once()
 {
    let (state, ca, rotation) = fixture(101).await;
    replace(&state, &ca, &rotation).await.unwrap();
    let work = claim(&state, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_cancel_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-superseded-node-jobs' AND NEW.state='queued' THEN RAISE EXCEPTION 'reject ack'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_cancel_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_cancel_ack();").execute(&state.pool).await.unwrap();
    assert!(page(&state.pool, &work).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_node_operations WHERE pending_job"
        )
        .await,
        101
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM job_events WHERE event_type='job.cancelled'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_cancel_ack ON background_work; CREATE FUNCTION reject_cancel_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.cancelled' THEN RAISE EXCEPTION 'reject event'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_cancel_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_cancel_event();").execute(&state.pool).await.unwrap();
    assert!(page(&state.pool, &work).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_cancel_event ON job_events; CREATE SEQUENCE cancel_attempts; CREATE FUNCTION retry_cancel_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-superseded-node-jobs' AND NEW.state='queued' AND nextval('cancel_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_cancel_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_cancel_ack();").execute(&state.pool).await.unwrap();
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        100
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM job_events WHERE event_type='job.cancelled'"
        )
        .await,
        100
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM cancel_attempts").await,
        2
    );
}

#[tokio::test]
async fn takeover_rejects_old_owner_and_wrong_scope_and_preserves_the_committed_cursor() {
    let (state, ca, rotation) = fixture(101).await;
    replace(&state, &ca, &rotation).await.unwrap();
    let work = claim(&state, "page").await;
    page(&state.pool, &work).await.unwrap();
    let old = claim(&state, "old").await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = claim(&state, "next").await;
    assert_eq!(old.payload, next.payload);
    assert!(page(&state.pool, &old).await.is_err());
    let mut wrong = next.clone();
    wrong.payload["rotation_id"] = json!("other");
    assert!(page(&state.pool, &wrong).await.is_err());
    wrong = next.clone();
    wrong.payload["ca_id"] = json!("other");
    assert!(page(&state.pool, &wrong).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        100
    );
    page(&state.pool, &next).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        101
    );
}

#[tokio::test]
async fn healthy_or_uncertain_remote_owners_are_preserved_while_other_nodes_cancel() {
    let (state, ca, rotation) = fixture(3).await;
    let mut tx = state.pool.begin().await.unwrap();
    let healthy = execution::acquire(&mut tx, "node:member-0001", "healthy", "old-member-0001")
        .await
        .unwrap()
        .unwrap();
    let uncertain = execution::acquire(&mut tx, "node:member-0002", "remote", "old-member-0002")
        .await
        .unwrap()
        .unwrap();
    let (operation, _) = uncertain
        .dispatch(
            &mut tx,
            "remote-step",
            &json!({"request_digest":"unchanged"}),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let leases:Value=sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(l) ORDER BY resource_key) FROM resource_leases l WHERE resource_key LIKE 'node:%'").fetch_one(&state.pool).await.unwrap();
    replace(&state, &ca, &rotation).await.unwrap();
    let work = claim(&state, "cancel").await;
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        1
    );
    assert_eq!(leases,sqlx::query_scalar::<_,Value>("SELECT jsonb_agg(to_jsonb(l) ORDER BY resource_key) FROM resource_leases l WHERE resource_key LIKE 'node:%'").fetch_one(&state.pool).await.unwrap());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM execution_operations WHERE state='dispatching'"
        )
        .await,
        1
    );
    let mut tx = state.pool.begin().await.unwrap();
    healthy.release(&mut tx).await.unwrap();
    tx.commit().await.unwrap();
    let work = claim(&state, "released").await;
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        2
    );
    let still: String =
        sqlx::query_scalar("SELECT state FROM execution_operations WHERE operation_id=$1")
            .bind(operation)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(still, "dispatching");
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-superseded-node-jobs' AND state='complete'").await,0);
}

#[tokio::test]
async fn cancelled_source_is_excluded_from_the_real_queue_before_any_status_materialization() {
    let (state, ca, rotation) = fixture(3).await;
    replace(&state, &ca, &rotation).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='queued'").await,
        3
    );
    assert!(
        crate::jobs::claim_next(&state.pool)
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('unrelated','record-remove','queued','queued',now(),now(),now())").execute(&state.pool).await.unwrap();
    assert_eq!(
        crate::jobs::claim_next(&state.pool)
            .await
            .unwrap()
            .unwrap()
            .id,
        "unrelated"
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='queued'").await,
        3
    );
}

async fn leased_job(state: &AppState, id: &str, node: &str) -> Vec<execution::Execution> {
    sqlx::query("UPDATE jobs SET status='running',stage='starting' WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let mut leases = vec![];
    for key in [format!("job:{id}"), format!("node:{node}")] {
        leases.push(
            execution::acquire(&mut tx, &key, "executor", id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    leases
}

#[tokio::test]
async fn supersession_prevents_polling_a_new_remote_step_even_with_healthy_job_and_node_leases() {
    let (state, ca, rotation) = fixture(1).await;
    let leases = leased_job(&state, "old-member-0001", "member-0001").await;
    replace(&state, &ca, &rotation).await.unwrap();
    let sends = Arc::new(AtomicUsize::new(0));
    let counter = sends.clone();
    let outcome = execution::supervise(
        state.pool.clone(),
        leases,
        execution::remote(
            "ssh-command",
            "digest".into(),
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |_| json!({"verified":true}),
        ),
    )
    .await;
    assert!(outcome.is_none());
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
}

#[tokio::test]
async fn a_step_dispatched_before_replacement_keeps_its_intent_and_cannot_publish_old_success() {
    let (state, ca, rotation) = fixture(1).await;
    let leases = leased_job(&state, "old-member-0001", "member-0001").await;
    let sends = Arc::new(AtomicUsize::new(0));
    let counter = sends.clone();
    let app = state.clone();
    let outcome = execution::supervise(
        state.pool.clone(),
        leases,
        execution::remote(
            "ssh-command",
            "digest".into(),
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                replace(&app, &ca, &rotation).await.unwrap();
                Ok(())
            },
            |_| json!({"verified":true}),
        ),
    )
    .await;
    assert!(outcome.is_none_or(|result| result.is_err()));
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    assert_eq!(count(&state,"SELECT count(*) FROM execution_operations WHERE state='dispatching' AND evidence IS NULL").await,1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='running'").await,
        1
    );
    let mut tx = state.pool.begin().await.unwrap();
    assert!(
        execution::acquire(&mut tx, "node:member-0001", "other", "new-work")
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn pending_job_index_and_trigger_follow_all_authoritative_status_transitions() {
    let (state, ca, rotation) = fixture(1001).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_node_operations WHERE pending_job"
        )
        .await,
        1001
    );
    for status in [
        "running",
        "waiting_confirmation",
        "failed",
        "queued",
        "cancelled",
    ] {
        sqlx::query("UPDATE jobs SET status=$1")
            .bind(status)
            .execute(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_node_operations WHERE pending_job"
            )
            .await,
            if matches!(status, "queued" | "waiting_confirmation") {
                1001
            } else {
                0
            }
        );
    }
    sqlx::query("ANALYZE mtls_node_operations")
        .execute(&state.pool)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE,FORMAT JSON) {PENDING}"
    )))
    .bind(&rotation)
    .bind(&ca)
    .bind(2_i64)
    .bind("")
    .bind("")
    .bind(PAGE)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(
        plan.to_string().contains("mtls_rotation_pending_jobs"),
        "{plan}"
    );
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
}

#[tokio::test]
async fn source_change_does_not_cancel_another_target_and_repeat_admission_preserves_a_healthy_owner()
 {
    let (state, ca, rotation) = fixture(1).await;
    replace(&state, &ca, &rotation).await.unwrap();
    let work = claim(&state, "healthy").await;
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(w) FROM background_work w WHERE id=$1")
        .bind(&work.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    enqueue_on(&mut tx, &rotation, &ca, 2).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>("SELECT to_jsonb(w) FROM background_work w WHERE id=$1")
            .bind(&work.id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    assert!(
        maintenance::claim(&state.pool, KIND, "other")
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE mtls_rotations SET outcome=NULL WHERE id=$1")
        .bind(&rotation)
        .execute(&state.pool)
        .await
        .unwrap();
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-superseded-node-jobs' AND state='complete' AND error_code='superseded_job_source_changed'").await,1);
}

#[tokio::test]
async fn migration_0060_to_0061_backfills_only_derived_flags_and_keeps_all_job_history() {
    let pool = db::test_pool_through_version(8, 60).await;
    let (state, ca, rotation) = super::super::trust::tests::fixture_with_pool(pool, 3).await;
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,available_at,created_at,updated_at) SELECT 'old-'||id,'deploy',id,1,CASE WHEN id='member-0001' THEN 'queued' WHEN id='member-0002' THEN 'waiting_confirmation' ELSE 'succeeded' END,'history',now(),now(),now() FROM nodes").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_node_operations(job_id,node_id,ca_id,ca_version,rotation_id,stage,desired_revision,created_at) SELECT id,node_id,$1,2,$2,'trust',1,now() FROM jobs").bind(&ca).bind(&rotation).execute(&state.pool).await.unwrap();
    let snapshot = format!(
        "SELECT jsonb_build_object('sources',({SNAPSHOTS}),'jobs',(SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j),'events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM job_events e))"
    );
    let before: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(snapshot.as_str()))
        .fetch_one(&state.pool)
        .await
        .unwrap();
    for _ in 0..2 {
        db::run_migrations(&state.pool).await.unwrap();
        assert_eq!(
            before,
            sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(snapshot.as_str()))
                .fetch_one(&state.pool)
                .await
                .unwrap()
        );
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_node_operations WHERE pending_job"
            )
            .await,
            2
        );
        assert_eq!(
            count(&state, "SELECT count(*) FROM mtls_superseded_node_jobs").await,
            0
        );
    }
}

async fn blocked_by(state: &AppState, blocker: i32) -> i32 {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Some(pid) = sqlx::query_scalar(
            "SELECT pid FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)) LIMIT 1",
        )
        .bind(blocker)
        .fetch_optional(&state.pool)
        .await
        .unwrap()
        {
            return pid;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected real PostgreSQL source lock contention"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn dispatch_intent_pins_the_rotation_until_commit_before_replacement_can_publish_its_fact() {
    let (state, ca, rotation) = fixture(1).await;
    let mut connections = vec![];
    for _ in 0..8 {
        let mut c = state.pool.acquire().await.unwrap();
        sqlx::query("SET lock_timeout='5s'")
            .execute(&mut *c)
            .await
            .unwrap();
        connections.push(c);
    }
    drop(connections);
    let leases = leased_job(&state, "old-member-0001", "member-0001").await;
    sqlx::raw_sql("CREATE FUNCTION pause_source_dispatch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock(450061); RETURN NEW; END $$; CREATE TRIGGER pause_source_dispatch BEFORE INSERT ON execution_operations FOR EACH ROW EXECUTE FUNCTION pause_source_dispatch();").execute(&state.pool).await.unwrap();
    let mut barrier = state.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(450061)")
        .execute(&mut *barrier)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *barrier)
        .await
        .unwrap();
    let (published, mut wait_published) = tokio::sync::watch::channel(false);
    let sends = Arc::new(AtomicUsize::new(0));
    let counter = sends.clone();
    let app = state.clone();
    let remote = tokio::spawn(async move {
        execution::supervise(
            app.pool.clone(),
            leases,
            execution::remote(
                "ssh-command",
                "pinned-digest".into(),
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    wait_published.wait_for(|done| *done).await.unwrap();
                    Ok(())
                },
                |_| json!({"verified":true}),
            ),
        )
        .await
    });
    let dispatcher = blocked_by(&state, pid).await;
    let app = state.clone();
    let old = rotation.clone();
    let authority = ca.clone();
    let mut publisher = tokio::spawn(async move { replace(&app, &authority, &old).await });
    tokio::select! {_=blocked_by(&state,dispatcher)=>{},result=&mut publisher=>panic!("replacement crossed uncommitted dispatch source pin: {result:?}")}
    barrier.commit().await.unwrap();
    assert_eq!(publisher.await.unwrap().unwrap()["version"], 3);
    let _ = published.send(true);
    let result = remote.await.unwrap();
    assert!(result.is_none_or(|r| r.is_err()));
    assert!(sends.load(Ordering::SeqCst) <= 1);
    assert_eq!(count(&state,"SELECT count(*) FROM execution_operations WHERE state='dispatching' AND evidence IS NULL").await,1);
}

async fn scanner(state: &AppState, owner: &str) -> Work {
    maintenance::ensure(&state.pool, "mtls:superseded-node-jobs", SCAN, json!({}))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1 AND state='queued'",
    )
    .bind(SCAN)
    .execute(&state.pool)
    .await
    .unwrap();
    maintenance::claim(&state.pool, SCAN, owner)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn historical_supersessions_and_recovered_jobs_are_readmitted_without_resetting_healthy_or_future_work()
 {
    let (state, ca, rotation) = fixture(1).await;
    replace(&state, &ca, &rotation).await.unwrap();
    sqlx::query("DELETE FROM background_work WHERE kind=$1")
        .bind(KIND)
        .execute(&state.pool)
        .await
        .unwrap();
    let work = scanner(&state, "history").await;
    scan(&state.pool, &work).await.unwrap();
    let work = claim(&state, "cancel").await;
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        1
    );
    sqlx::query("UPDATE jobs SET status='queued',stage='recovered'")
        .execute(&state.pool)
        .await
        .unwrap();
    let work = scanner(&state, "recover").await;
    scan(&state.pool, &work).await.unwrap();
    let owner = claim(&state, "healthy").await;
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(w) FROM background_work w WHERE id=$1")
        .bind(&owner.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let work = scanner(&state, "observe").await;
    scan(&state.pool, &work).await.unwrap();
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>("SELECT to_jsonb(w) FROM background_work w WHERE id=$1")
            .bind(&owner.id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    page(&state.pool, &owner).await.unwrap();
    sqlx::query("UPDATE jobs SET status='queued',stage='recovered'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET state='failed',available_at=clock_timestamp()+interval '1 hour' WHERE kind=$1").bind(KIND).execute(&state.pool).await.unwrap();
    let work = scanner(&state, "future").await;
    scan(&state.pool, &work).await.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-superseded-node-jobs' AND state='failed'").await,1);
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1")
        .bind(KIND)
        .execute(&state.pool)
        .await
        .unwrap();
    let work = scanner(&state, "due").await;
    scan(&state.pool, &work).await.unwrap();
    let work = claim(&state, "last").await;
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM job_events WHERE event_type='job.cancelled'"
        )
        .await,
        3
    );
}

#[tokio::test]
async fn historical_admission_is_bounded_to_one_hundred_rotations_per_scan() {
    let (state, ca, rotation) = fixture(1).await;
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) SELECT credential_id,i,payload_enc,metadata,created_at FROM credential_versions CROSS JOIN generate_series(3,303)i WHERE credential_id=$1 AND version=1").bind(&ca).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_ca_generations(ca_id,ca_version,state,expires_at) SELECT ca_id,i,'retired',expires_at FROM mtls_ca_generations CROSS JOIN generate_series(3,303)i WHERE ca_id=$1 AND ca_version=1").bind(&ca).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_rotations(id,ca_id,ca_version,phase,batch_id,next_action_at,created_at,updated_at,outcome) SELECT 'history-'||lpad(i::text,4,'0'),ca_id,i,'complete',batch_id,now(),now(),now(),'superseded_unpublished_target' FROM mtls_rotations CROSS JOIN generate_series(3,303)i WHERE id=$1").bind(&rotation).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,status,stage,available_at,created_at,updated_at) SELECT 'history-'||i,'deploy','member-0001','queued','history',now(),now(),now() FROM generate_series(3,303)i").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_node_operations(job_id,node_id,ca_id,ca_version,rotation_id,stage,desired_revision,created_at) SELECT 'history-'||i,'member-0001',$1,i,'history-'||lpad(i::text,4,'0'),'trust',1,now() FROM generate_series(3,303)i").bind(&ca).execute(&state.pool).await.unwrap();
    for expected in [100, 200, 300, 301] {
        let work = scanner(&state, "admit").await;
        scan(&state.pool, &work).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-superseded-node-jobs'"
            )
            .await,
            expected
        );
    }
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='queued'").await,
        302
    );
}

#[tokio::test]
async fn admission_confirmation_failure_rolls_back_every_outbox_and_native_retry_admits_one() {
    let (state, ca, rotation) = fixture(1).await;
    replace(&state, &ca, &rotation).await.unwrap();
    sqlx::query("DELETE FROM background_work WHERE kind=$1")
        .bind(KIND)
        .execute(&state.pool)
        .await
        .unwrap();
    let work = scanner(&state, "scan").await;
    sqlx::raw_sql("CREATE FUNCTION reject_cancel_scan() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-superseded-node-job-admission' AND NEW.state='queued' THEN RAISE EXCEPTION 'reject scan'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_cancel_scan BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_cancel_scan();").execute(&state.pool).await.unwrap();
    assert!(scan(&state.pool, &work).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-superseded-node-jobs'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_cancel_scan ON background_work; CREATE SEQUENCE cancel_scan_attempts; CREATE FUNCTION retry_cancel_scan() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-superseded-node-job-admission' AND NEW.state='queued' AND nextval('cancel_scan_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_cancel_scan BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_cancel_scan();").execute(&state.pool).await.unwrap();
    scan(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-superseded-node-jobs'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM cancel_scan_attempts").await,
        2
    );
}

#[tokio::test]
async fn only_queued_and_waiting_jobs_are_materialized_and_old_terminal_history_is_preserved() {
    let (state, ca, rotation) = fixture(6).await;
    for (i, status) in [
        "queued",
        "waiting_confirmation",
        "running",
        "failed",
        "succeeded",
        "cancelled",
    ]
    .iter()
    .enumerate()
    {
        sqlx::query("UPDATE jobs SET status=$2 WHERE id=$1")
            .bind(format!("old-member-{:04}", i + 1))
            .bind(status)
            .execute(&state.pool)
            .await
            .unwrap();
    }
    let before: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j WHERE id>'old-member-0002'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    replace(&state, &ca, &rotation).await.unwrap();
    let work = claim(&state, "cancel").await;
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(
            "SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j WHERE id>'old-member-0002'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap()
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM job_events WHERE event_type='job.cancelled'"
        )
        .await,
        2
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-superseded-node-jobs' AND state='complete'").await,1);
}

#[tokio::test]
async fn an_expired_published_target_cancels_old_jobs_without_discarding_its_published_material() {
    let (state, ca, rotation) = fixture(1).await;
    sqlx::query("UPDATE mtls_ca_generations SET state='retired' WHERE ca_id=$1 AND ca_version=1")
        .bind(&ca)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_ca_generations SET state='published',published_at=clock_timestamp(),expires_at=clock_timestamp()-interval '1 second' WHERE ca_id=$1 AND ca_version=2").bind(&ca).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE mtls_authorities SET published_version=2 WHERE credential_id=$1")
        .bind(&ca)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE mtls_rotations SET phase='probes',published_at=clock_timestamp() WHERE id=$1",
    )
    .bind(&rotation)
    .execute(&state.pool)
    .await
    .unwrap();
    replace(&state, &ca, &rotation).await.unwrap();
    let work = claim(&state, "cancel").await;
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE status='cancelled'").await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_ca_generations WHERE ca_version=2 AND state='published' AND published_at IS NOT NULL").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-superseded-identity-flags'"
        )
        .await,
        0
    );
}
