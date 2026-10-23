use super::*;
use crate::execution;

async fn fixture() -> PgPool {
    let pool = db::test_pool_with_max_connections(8).await;
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,attempts,available_at,created_at,updated_at) SELECT id,'sync','running','starting',1,now(),now(),now() FROM unnest(ARRAY['one','two'])id").execute(&pool).await.unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&pool).await.unwrap();
    pool
}
fn job(id: &str) -> JobInput {
    JobInput {
        id: id.into(),
        kind: "sync".into(),
        node_id: None,
        target_revision: Some(1),
        payload: json!({}),
        attempts: 1,
    }
}
async fn owner(pool: &PgPool, id: &str, name: &str) -> Vec<execution::Execution> {
    let mut tx = pool.begin().await.unwrap();
    let lease = execution::acquire(&mut tx, &format!("job:{id}"), name, id)
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    vec![lease]
}
async fn count(pool: &PgPool, q: &str) -> i64 {
    sqlx::query_scalar(q).fetch_one(pool).await.unwrap()
}
async fn logs(pool: &PgPool, id: &str) -> Value {
    sqlx::query_scalar("SELECT logs_json FROM jobs WHERE id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn leased_progress_does_not_wait_for_the_old_global_lock_or_an_unrelated_job_row() {
    let pool = fixture().await;
    let leases = owner(&pool, "one", "instance").await;
    let legacy = db::begin_write(&pool).await.unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id='two' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let input = job("one");
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        execution::supervise(
            pool.clone(),
            leases,
            report_progress(&pool, &input, "fetching", "Downloading resources"),
        ),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(logs(&pool, "one").await[0]["stage"], "fetching");
    assert_eq!(logs(&pool, "two").await, json!([]));
    assert_eq!(count(&pool, "SELECT count(*) FROM job_events").await, 1);
    held.rollback().await.unwrap();
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn event_failure_rolls_back_stage_and_logs_and_native_retry_commits_one_complete_progress_record()
 {
    let pool = fixture().await;
    let leases = owner(&pool, "one", "instance").await;
    let input = job("one");
    sqlx::raw_sql("CREATE FUNCTION reject_progress_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.progress' THEN RAISE EXCEPTION 'reject progress'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_progress_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_progress_event();").execute(&pool).await.unwrap();
    assert!(
        execution::supervise(
            pool.clone(),
            leases.clone(),
            report_progress(&pool, &input, "fetching", "Fixed message")
        )
        .await
        .unwrap()
        .is_err()
    );
    assert_eq!(logs(&pool, "one").await, json!([]));
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE id='one' AND stage='starting'"
        )
        .await,
        1
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM job_events").await, 0);
    sqlx::raw_sql("DROP TRIGGER reject_progress_event ON job_events; CREATE SEQUENCE progress_attempts; CREATE FUNCTION retry_progress_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.progress' AND nextval('progress_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_progress_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION retry_progress_event();").execute(&pool).await.unwrap();
    execution::supervise(
        pool.clone(),
        leases,
        report_progress(&pool, &input, "fetching", "Fixed message"),
    )
    .await
    .unwrap()
    .unwrap();
    let saved = logs(&pool, "one").await;
    assert_eq!(saved.as_array().unwrap().len(), 1);
    assert_eq!(saved[0]["message"], "Fixed message");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.progress'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT last_value FROM progress_attempts").await,
        2
    );
}

#[tokio::test]
async fn concurrent_reports_serialize_on_the_job_and_neither_log_append_is_lost() {
    let pool = fixture().await;
    let leases = owner(&pool, "one", "instance").await;
    let input = job("one");
    let (a, b) = tokio::join!(
        execution::supervise(
            pool.clone(),
            leases.clone(),
            report_progress(&pool, &input, "a", "first")
        ),
        execution::supervise(
            pool.clone(),
            leases,
            report_progress(&pool, &input, "b", "second")
        )
    );
    a.unwrap().unwrap();
    b.unwrap().unwrap();
    let saved = logs(&pool, "one").await;
    let mut messages = saved
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["message"].as_str().unwrap())
        .collect::<Vec<_>>();
    messages.sort();
    assert_eq!(messages, vec!["first", "second"]);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.progress'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn an_expired_owner_cannot_append_and_the_next_generation_can_report() {
    let pool = fixture().await;
    let leases = owner(&pool, "one", "old").await;
    let input = job("one");
    let other = pool.clone();
    let request = input.clone();
    let result=execution::supervise(pool.clone(),leases,async move {
        sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id='one'").execute(&other).await.unwrap();
        report_progress(&other,&request,"old","Must not be saved").await
    }).await;
    assert!(result.is_none_or(|r| r.is_err()));
    assert_eq!(logs(&pool, "one").await, json!([]));
    let next = owner(&pool, "one", "next").await;
    assert_eq!(next[0].generation, 2);
    execution::supervise(
        pool.clone(),
        next,
        report_progress(&pool, &input, "next", "New owner"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(logs(&pool, "one").await[0]["message"], "New owner");
    assert_eq!(count(&pool, "SELECT count(*) FROM job_events").await, 1);
}

#[tokio::test]
async fn another_job_or_mixed_node_owner_cannot_use_a_healthy_scope_to_publish_progress() {
    let pool = fixture().await;
    let leases = owner(&pool, "one", "instance").await;
    let wrong = job("two");
    execution::supervise(pool.clone(), leases.clone(), async {
        assert!(
            report_progress(&pool, &wrong, "wrong", "Wrong job")
                .await
                .is_err()
        );
    })
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let node = execution::acquire(&mut tx, "node:node", "another-instance", "one")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let mut mixed = leases;
    mixed.push(node);
    let mut wrong = job("one");
    wrong.node_id = Some("node".into());
    execution::supervise(pool.clone(), mixed, async {
        assert!(
            report_progress(&pool, &wrong, "wrong", "Wrong node owner")
                .await
                .is_err()
        );
    })
    .await
    .unwrap();
    assert_eq!(logs(&pool, "one").await, json!([]));
    assert_eq!(logs(&pool, "two").await, json!([]));
    assert_eq!(count(&pool, "SELECT count(*) FROM job_events").await, 0);
}

#[tokio::test]
async fn persisted_cancellation_stops_progress_and_terminal_jobs_are_not_restored() {
    let pool = fixture().await;
    let leases = owner(&pool, "one", "instance").await;
    let input = job("one");
    let other = pool.clone();
    let request = input.clone();
    let result = execution::supervise(pool.clone(), leases, async move {
        sqlx::query("UPDATE jobs SET status='cancelled' WHERE id='one'")
            .execute(&other)
            .await
            .unwrap();
        report_progress(&other, &request, "late", "Cancelled progress").await
    })
    .await;
    assert!(result.is_none_or(|r| r.is_err()));
    assert_eq!(logs(&pool, "one").await, json!([]));
    for status in ["cancelled", "succeeded", "failed", "waiting_confirmation"] {
        sqlx::query("UPDATE jobs SET status=$1 WHERE id='one'")
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
        report_progress(&pool, &input, "late", "Terminal progress")
            .await
            .unwrap();
        assert_eq!(
            count(
                &pool,
                "SELECT count(*) FROM jobs WHERE id='one' AND stage='starting'"
            )
            .await,
            1
        );
    }
    assert_eq!(count(&pool, "SELECT count(*) FROM job_events").await, 0);
}

#[tokio::test]
async fn one_hundred_log_limit_preserves_all_events_and_uses_one_database_timestamp_per_commit() {
    let pool = fixture().await;
    let leases = owner(&pool, "one", "instance").await;
    let input = job("one");
    execution::supervise(pool.clone(), leases, async {
        for i in 0..101 {
            report_progress(&pool, &input, "working", &format!("message-{i}"))
                .await
                .unwrap();
        }
    })
    .await
    .unwrap();
    let saved = logs(&pool, "one").await;
    assert_eq!(saved.as_array().unwrap().len(), 100);
    assert_eq!(saved[0]["message"], "message-1");
    assert_eq!(saved[99]["message"], "message-100");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.progress'"
        )
        .await,
        101
    );
    let consistent:bool=sqlx::query_scalar("SELECT updated_at=(logs_json->99->>'created_at')::timestamptz AND updated_at=(SELECT created_at FROM job_events WHERE job_id='one' ORDER BY id DESC LIMIT 1) FROM jobs WHERE id='one'").fetch_one(&pool).await.unwrap();
    assert!(consistent);
}
