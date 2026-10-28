use super::*;
use crate::{
    execution,
    jobs::ownership_tests::{claim, count, enqueue, fixture, operation},
};

#[tokio::test]
async fn permanent_source_errors_stop_after_one_attempt_but_transport_failures_retry() {
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let result = tokio::time::timeout(
        Duration::from_millis(200),
        retry_persistence(|| async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err::<(), _>(anyhow::anyhow!("CA proof expired"))
        }),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    calls.store(0, std::sync::atomic::Ordering::SeqCst);
    let value = retry_persistence(|| async {
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Err(sqlx::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "lost acknowledgement",
            ))
            .into())
        } else {
            Ok(42)
        }
    })
    .await
    .unwrap();
    assert_eq!(value, 42);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(transient(
        &anyhow::Error::new(crate::error::ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "transaction_retry_exhausted",
            "retry"
        ))
        .context("nested completion")
    ));
    assert!(!transient(
        &sqlx::Error::Protocol("source changed".into()).into()
    ));
    assert!(!transient(&anyhow::Error::new(
        crate::error::ApiError::conflict("revision changed")
    )));
}

#[tokio::test]
async fn rejected_failure_event_still_allows_a_durable_safety_barrier_without_claiming_a_business_result()
 {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    operation(&pool, &one).await;
    sqlx::raw_sql("CREATE FUNCTION reject_failure_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.reconciliation_required' THEN RAISE EXCEPTION 'event rejected'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_failure_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_failure_event()")
        .execute(&pool).await.unwrap();
    assert!(reject(&pool, &one).await.unwrap().is_err());
    let result = execution::supervise(
        pool.clone(),
        one.leases.clone(),
        quarantine_completion(&pool, &one.job),
    )
    .await;
    assert!(result.is_none_or(|r| !r.unwrap()));
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE id='one' AND status='running'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM resource_leases WHERE blocked").await,
        2
    );
    assert_eq!(count(&pool,"SELECT count(*) FROM execution_operations WHERE step='completion-rejected' AND state='needs_attention'").await,1);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.reconciliation_required'"
        )
        .await,
        0
    );
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(claim_next_for(&pool, Some("next")).await.unwrap().is_none());
}

#[tokio::test]
async fn executor_stops_permanent_retry_and_preserves_remote_resources_when_failure_publication_also_fails()
 {
    for mode in 0..3 {
        let pool = fixture().await;
        enqueue(&pool, "one", Some("node-a"), None).await;
        sqlx::raw_sql("UPDATE jobs SET kind='record-remove' WHERE id='one'; UPDATE nodes SET state='deleting' WHERE id='node-a'").execute(&pool).await.unwrap();
        let one = claim(&pool, "owner").await;
        if mode > 0 {
            let op = operation(&pool, &one).await;
            let mut tx = pool.begin().await.unwrap();
            one.leases
                .iter()
                .find(|l| l.resource == "node:node-a")
                .unwrap()
                .verified(&mut tx, &op, &json!({"remote_receipt":"historical"}))
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        sqlx::raw_sql("CREATE FUNCTION reject_executor_success() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.succeeded' THEN RAISE EXCEPTION 'reject successful publication'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_executor_success BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_executor_success()")
            .execute(&pool).await.unwrap();
        if mode == 2 {
            sqlx::raw_sql("CREATE FUNCTION reject_executor_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.reconciliation_required' THEN RAISE EXCEPTION 'reject failure publication'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_executor_failure BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_executor_failure()")
                .execute(&pool).await.unwrap();
        }
        let state = AppState::new(
            pool.clone(),
            SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
        );
        let mut removed = state.removed_nodes.subscribe();
        tokio::time::timeout(Duration::from_secs(10), execute_claimed(state, one))
            .await
            .unwrap();
        assert_eq!(
            count(&pool, "SELECT count(*) FROM nodes WHERE id='node-a'").await,
            1
        );
        assert!(matches!(
            removed.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                | Err(tokio::sync::broadcast::error::TryRecvError::Closed)
        ));
        if mode == 0 {
            assert_eq!(
                count(&pool, "SELECT count(*) FROM jobs WHERE status='failed'").await,
                1
            );
            assert_eq!(
                count(
                    &pool,
                    "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL OR blocked"
                )
                .await,
                0
            );
        } else {
            assert_eq!(
                count(&pool, "SELECT count(*) FROM resource_leases WHERE blocked").await,
                2
            );
            assert_eq!(count(&pool,"SELECT count(*) FROM execution_operations WHERE step='completion-rejected' AND state='needs_attention'").await,1);
            assert_eq!(count(&pool,if mode==1 {"SELECT count(*) FROM jobs WHERE status='failed' AND stage='needs_attention'"} else {"SELECT count(*) FROM jobs WHERE status='running'"}).await,1);
        }
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM job_events WHERE event_type='job.succeeded'"
            )
            .await,
            0
        );
    }
}

async fn reject(pool: &PgPool, claimed: &ClaimedJob) -> Option<anyhow::Result<bool>> {
    execution::supervise(
        pool.clone(),
        claimed.leases.clone(),
        reject_completion(pool, &claimed.job, "completion rejected"),
    )
    .await
}
async fn snapshot(pool: &PgPool) -> Value {
    // A healthy supervisor may extend expires_at; ownership and blocking must
    // stay unchanged when the entire rejection transaction rolls back.
    sqlx::query_scalar("SELECT jsonb_build_object('jobs',(SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j),'leases',(SELECT jsonb_agg(to_jsonb(l)-'expires_at' ORDER BY resource_key) FROM resource_leases l),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY operation_id) FROM execution_operations o),'events',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM job_events e),'nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n))")
        .fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn database_only_rejection_fails_once_and_releases_without_creating_remote_uncertainty() {
    let pool = fixture().await;
    enqueue(&pool, "one", None, None).await;
    let one = claim(&pool, "owner").await;
    let result = reject(&pool, &one).await;
    assert!(result.is_none_or(|r| r.unwrap()));
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE status='failed' AND stage='failed'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM execution_operations").await,
        0
    );
    finish_execution(&pool, &one.job, &one.leases)
        .await
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL OR blocked"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.failed'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn verified_remote_rejection_preserves_node_and_atomically_blocks_its_whole_scope() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), Some("dns-zone:z")).await;
    let one = claim(&pool, "owner").await;
    let op = operation(&pool, &one).await;
    let mut tx = pool.begin().await.unwrap();
    one.leases
        .iter()
        .find(|l| l.resource == "node:node-a")
        .unwrap()
        .verified(&mut tx, &op, &json!({"remote_digest":"applied"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(n) FROM nodes n WHERE id='node-a'")
        .fetch_one(&pool)
        .await
        .unwrap();
    enqueue(&pool, "two", Some("node-b"), None).await;
    let two = claim(&pool, "other").await;
    let other:Value=sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(l) ORDER BY resource_key) FROM resource_leases l WHERE work_id='two'").fetch_one(&pool).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), reject(&pool, &one))
        .await
        .unwrap();
    assert!(result.is_none_or(|r| !r.unwrap()));
    assert_eq!(
        sqlx::query_scalar::<_, Value>("SELECT to_jsonb(n) FROM nodes n WHERE id='node-a'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        before
    );
    assert_eq!(count(&pool,"SELECT count(*) FROM jobs WHERE id='one' AND status='failed' AND stage='needs_attention'").await,1);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM resource_leases WHERE work_id='one' AND blocked"
        )
        .await,
        3
    );
    assert_eq!(count(&pool,"SELECT count(*) FROM execution_operations WHERE step='completion-rejected' AND state='needs_attention'").await,1);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.reconciliation_required'"
        )
        .await,
        1
    );
    assert_eq!(sqlx::query_scalar::<_,Value>("SELECT jsonb_agg(to_jsonb(l) ORDER BY resource_key) FROM resource_leases l WHERE work_id='two'").fetch_one(&pool).await.unwrap(),other);
    assert_eq!(two.job.id, "two");
    assert!(claim_next_for(&pool, Some("next")).await.unwrap().is_none());
}

#[tokio::test]
async fn rejection_event_failure_rolls_back_barrier_and_every_block_then_native_retry_commits_once()
{
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    operation(&pool, &one).await;
    sqlx::raw_sql("CREATE FUNCTION reject_barrier_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.reconciliation_required' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_barrier_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_barrier_event()")
        .execute(&pool).await.unwrap();
    let before = snapshot(&pool).await;
    assert!(reject(&pool, &one).await.unwrap().is_err());
    assert_eq!(snapshot(&pool).await, before);
    sqlx::raw_sql("DROP TRIGGER reject_barrier_event ON job_events; CREATE SEQUENCE barrier_attempt; CREATE FUNCTION retry_barrier_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.reconciliation_required' AND nextval('barrier_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_barrier_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION retry_barrier_event()")
        .execute(&pool).await.unwrap();
    let result = reject(&pool, &one).await;
    assert!(result.is_none_or(|r| !r.unwrap()));
    assert_eq!(
        count(&pool, "SELECT last_value FROM barrier_attempt").await,
        2
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM execution_operations WHERE step='completion-rejected'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM resource_leases WHERE blocked").await,
        2
    );
}

#[tokio::test]
async fn wrong_source_or_old_generation_cannot_quarantine_or_overwrite_the_current_job() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    operation(&pool, &one).await;
    for field in ["attempt", "payload", "revision", "node"] {
        let mut wrong = one.job.clone();
        match field {
            "attempt" => wrong.attempts += 1,
            "payload" => wrong.payload = json!({"changed":true}),
            "revision" => wrong.target_revision = Some(99),
            _ => wrong.node_id = Some("node-b".into()),
        }
        let before = snapshot(&pool).await;
        let result = execution::supervise(
            pool.clone(),
            one.leases.clone(),
            reject_completion(&pool, &wrong, "wrong source"),
        )
        .await;
        assert!(result.unwrap().is_err());
        assert_eq!(snapshot(&pool).await, before);
    }
    sqlx::query("UPDATE resource_leases SET generation=generation+1,owner='replacement'")
        .execute(&pool)
        .await
        .unwrap();
    let before = snapshot(&pool).await;
    assert!(reject(&pool, &one).await.is_none());
    assert_eq!(snapshot(&pool).await, before);
}

#[tokio::test]
async fn actual_success_rejection_stops_retrying_and_quarantines_without_deleting_the_node() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    operation(&pool, &one).await;
    let state = AppState::new(
        pool.clone(),
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    let output = JobOutput {
        stage: "done".into(),
        result: json!({}),
        node_state: None,
        deployed_revision: None,
        deployed_config: None,
        deployed_sha256: None,
        published_connection: None,
        delete_node: true,
    };
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let result = execution::supervise(pool.clone(), one.leases.clone(), async {
        let error = tokio::time::timeout(
            Duration::from_millis(200),
            retry_persistence(|| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                persist_success(&state, &one.job, output.clone())
            }),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(!transient(&error));
        reject_completion(&pool, &one.job, "invalid completed output").await
    })
    .await;
    assert!(result.is_none_or(|r| !r.unwrap()));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM nodes WHERE id='node-a'").await,
        1
    );
    assert_eq!(count(&pool,"SELECT count(*) FROM jobs WHERE id='one' AND status='failed' AND stage='needs_attention'").await,1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM resource_leases WHERE blocked").await,
        2
    );
}
