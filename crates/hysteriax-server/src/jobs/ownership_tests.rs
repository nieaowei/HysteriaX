use super::*;

pub(super) async fn fixture() -> PgPool {
    let pool = db::test_pool_with_max_connections(8).await;
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT id,id,'127.0.0.1',22,'root','password','unused','example.test',443,':443',id,'unused','unused','unused',now(),now() FROM unnest(ARRAY['node-a','node-b'])id").execute(&pool).await.unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&pool).await.unwrap();
    pool
}
pub(super) async fn enqueue(pool: &PgPool, id: &str, node: Option<&str>, resource: Option<&str>) {
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,resource_key,status,stage,available_at,created_at,updated_at) VALUES($1,'sync',$2,1,$3,'queued','queued',clock_timestamp(),clock_timestamp(),clock_timestamp())").bind(id).bind(node).bind(resource).execute(pool).await.unwrap();
}
pub(super) async fn claim(pool: &PgPool, owner: &str) -> ClaimedJob {
    claim_next_for(pool, Some(owner)).await.unwrap().unwrap()
}
pub(super) async fn count(pool: &PgPool, q: &str) -> i64 {
    sqlx::query_scalar(q).fetch_one(pool).await.unwrap()
}
async fn lease_snapshot(pool: &PgPool) -> Value {
    sqlx::query_scalar("SELECT coalesce(jsonb_agg(to_jsonb(l) ORDER BY resource_key),'[]'::jsonb) FROM resource_leases l").fetch_one(pool).await.unwrap()
}
pub(super) async fn operation(pool: &PgPool, claimed: &ClaimedJob) -> String {
    let lease = claimed
        .leases
        .iter()
        .find(|l| l.resource.starts_with("node:"))
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let (id, fresh) = lease
        .dispatch(&mut tx, "request", &json!({"request_digest":"fixed"}))
        .await
        .unwrap();
    assert!(fresh);
    tx.commit().await.unwrap();
    id
}

#[tokio::test]
async fn claim_and_release_use_database_time_and_continue_while_the_old_global_lock_is_held() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), Some("dns-zone:zone")).await;
    let legacy = db::begin_write(&pool).await.unwrap();
    let claimed = tokio::time::timeout(Duration::from_secs(2), claim(&pool, "instance-a"))
        .await
        .unwrap();
    assert_eq!(
        claimed
            .leases
            .iter()
            .map(|l| l.resource.as_str())
            .collect::<Vec<_>>(),
        vec!["dns-zone:zone", "job:one", "node:node-a"]
    );
    assert!(
        claimed
            .leases
            .iter()
            .all(|l| l.owner == "instance-a" && l.work == "one" && l.generation == 1)
    );
    assert_eq!(
        count(&pool, "SELECT attempts FROM jobs WHERE id='one'").await,
        1
    );
    let exact:bool=sqlx::query_scalar("SELECT updated_at=started_at AND updated_at=(SELECT created_at FROM job_events WHERE job_id='one' AND event_type='job.started') FROM jobs WHERE id='one'").fetch_one(&pool).await.unwrap();
    assert!(exact);
    sqlx::query("UPDATE jobs SET status='succeeded',stage='finished' WHERE id='one'")
        .execute(&pool)
        .await
        .unwrap();
    finish_execution(&pool, &claimed.job, &claimed.leases)
        .await
        .unwrap();
    assert_eq!(count(&pool,"SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL OR expires_at IS NOT NULL OR work_id IS NOT NULL").await,0);
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn two_instances_cannot_claim_two_jobs_for_the_same_node_but_another_node_remains_available()
{
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    enqueue(&pool, "two", Some("node-a"), None).await;
    let (a, b) = tokio::join!(
        claim_next_for(&pool, Some("a")),
        claim_next_for(&pool, Some("b"))
    );
    let mut claimed = vec![];
    for result in [a, b] {
        if let Some(job) = result.unwrap() {
            claimed.push(job);
        }
    }
    assert_eq!(claimed.len(), 1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE status='running'").await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE status='queued' AND attempts=0"
        )
        .await,
        1
    );
    enqueue(&pool, "three", Some("node-b"), None).await;
    let next = claim(&pool, "other").await;
    assert_eq!(next.job.id, "three");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.started'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn native_claim_retry_rolls_back_every_generation_attempt_and_event() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), Some("dns-zone:zone")).await;
    sqlx::raw_sql("CREATE SEQUENCE claim_attempts; CREATE FUNCTION retry_job_claim() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.started' AND nextval('claim_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_job_claim BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION retry_job_claim();").execute(&pool).await.unwrap();
    let claimed = claim(&pool, "owner").await;
    assert!(claimed.leases.iter().all(|l| l.generation == 1));
    assert_eq!(claimed.job.attempts, 1);
    assert_eq!(count(&pool, "SELECT count(*) FROM job_events").await, 1);
    assert_eq!(
        count(&pool, "SELECT last_value FROM claim_attempts").await,
        2
    );
}

#[tokio::test]
async fn rejected_started_event_leaves_no_partial_ownership_or_running_job() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), Some("dns-zone:zone")).await;
    sqlx::raw_sql("CREATE FUNCTION reject_job_claim() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.started' THEN RAISE EXCEPTION 'reject event'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_job_claim BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_job_claim();").execute(&pool).await.unwrap();
    assert!(claim_next_for(&pool, Some("owner")).await.is_err());
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE status='queued' AND attempts=0 AND started_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM resource_leases").await,
        0
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM job_events").await, 0);
}

async fn blocked_by(pool: &PgPool, blocker: i32) -> i32 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(pid) = sqlx::query_scalar(
            "SELECT pid FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)) LIMIT 1",
        )
        .bind(blocker)
        .fetch_optional(pool)
        .await
        .unwrap()
        {
            return pid;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected a PostgreSQL lock wait"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_resource_acquired_after_candidate_selection_rolls_back_the_entire_partial_claim() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), Some("dns-zone:zone")).await;
    let mut all = vec![];
    for _ in 0..8 {
        let mut c = pool.acquire().await.unwrap();
        sqlx::query("SET lock_timeout='5s'")
            .execute(&mut *c)
            .await
            .unwrap();
        all.push(c);
    }
    drop(all);
    sqlx::raw_sql("CREATE FUNCTION pause_job_claim() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.status='running' THEN PERFORM pg_advisory_xact_lock(480061); END IF; RETURN NEW; END $$; CREATE TRIGGER pause_job_claim BEFORE UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION pause_job_claim();").execute(&pool).await.unwrap();
    let mut barrier = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(480061)")
        .execute(&mut *barrier)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *barrier)
        .await
        .unwrap();
    let other = pool.clone();
    let mut claimant = tokio::spawn(async move { claim_next_for(&other, Some("claimant")).await });
    tokio::select! {_=blocked_by(&pool,pid)=>{},_= &mut claimant=>panic!("claim must wait behind the test barrier")}
    let mut tx = pool.begin().await.unwrap();
    crate::execution::acquire(&mut tx, "node:node-a", "healthy", "other-work")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    let before = lease_snapshot(&pool).await;
    barrier.commit().await.unwrap();
    assert!(claimant.await.unwrap().unwrap().is_none());
    assert_eq!(before, lease_snapshot(&pool).await);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE status='queued' AND attempts=0"
        )
        .await,
        1
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM job_events").await, 0);
}

#[tokio::test]
async fn release_only_touches_its_exact_resources_and_preserves_other_healthy_owners() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "one-owner").await;
    enqueue(&pool, "two", Some("node-b"), None).await;
    let two = claim(&pool, "two-owner").await;
    let q = "SELECT jsonb_agg(to_jsonb(l) ORDER BY resource_key) FROM resource_leases l WHERE work_id='two'";
    let before: Value = sqlx::query_scalar(q).fetch_one(&pool).await.unwrap();
    finish_execution(&pool, &one.job, &one.leases)
        .await
        .unwrap();
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(q)
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM resource_leases WHERE owner='two-owner'"
        )
        .await,
        2
    );
    finish_execution(&pool, &two.job, &two.leases)
        .await
        .unwrap();
}

#[tokio::test]
async fn release_rejects_old_generation_wrong_job_missing_resources_and_mixed_owners() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), Some("dns-zone:zone")).await;
    let one = claim(&pool, "owner").await;
    let before = lease_snapshot(&pool).await;
    for mode in 0..5 {
        let mut leases = one.leases.clone();
        let mut job = one.job.clone();
        match mode {
            0 => leases[0].generation += 1,
            1 => job.id = "other".into(),
            2 => {
                leases.pop();
            }
            3 => leases[0].owner = "other-owner".into(),
            _ => {
                leases.push(leases[0].clone());
            }
        }
        assert!(finish_execution(&pool, &job, &leases).await.is_err());
        assert_eq!(before, lease_snapshot(&pool).await);
    }
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id='one'").execute(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let mut next = vec![];
    for lease in &one.leases {
        next.push(
            crate::execution::acquire(&mut tx, &lease.resource, "next", "one")
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    let before = lease_snapshot(&pool).await;
    assert!(
        finish_execution(&pool, &one.job, &one.leases)
            .await
            .is_err()
    );
    assert_eq!(before, lease_snapshot(&pool).await);
    finish_execution(&pool, &one.job, &next).await.unwrap();
}

#[tokio::test]
async fn uncertain_release_and_event_acknowledgement_are_atomic_and_native_retry_quarantines_once()
{
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    let op = operation(&pool, &one).await;
    sqlx::query("UPDATE jobs SET status='failed',stage='failed',error_message='response lost' WHERE id='one'").execute(&pool).await.unwrap();
    let before = lease_snapshot(&pool).await;
    sqlx::raw_sql("CREATE FUNCTION reject_release_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.reconciliation_required' THEN RAISE EXCEPTION 'reject event'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_release_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_release_event();").execute(&pool).await.unwrap();
    assert!(
        finish_execution(&pool, &one.job, &one.leases)
            .await
            .is_err()
    );
    assert_eq!(before, lease_snapshot(&pool).await);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM execution_operations WHERE state='dispatching'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE stage='failed'").await,
        1
    );
    sqlx::raw_sql("DROP TRIGGER reject_release_event ON job_events; CREATE SEQUENCE release_attempts; CREATE FUNCTION retry_release_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.reconciliation_required' AND nextval('release_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_release_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION retry_release_event();").execute(&pool).await.unwrap();
    finish_execution(&pool, &one.job, &one.leases)
        .await
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM resource_leases WHERE work_id='one' AND blocked AND owner='owner'"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE stage='reconciling' AND error_message='response lost'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM job_events WHERE event_type='job.reconciliation_required'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT last_value FROM release_attempts").await,
        2
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM execution_operations WHERE operation_id=$1")
            .bind(op)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "reconciling");
}

#[tokio::test]
async fn needs_attention_is_not_downgraded_and_unowned_operations_are_not_rewritten() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    let op = operation(&pool, &one).await;
    sqlx::query("UPDATE execution_operations SET state='needs_attention' WHERE operation_id=$1")
        .bind(&op)
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let extra = crate::execution::acquire(&mut tx, "dns-zone:unrelated", "healthy", "one")
        .await
        .unwrap()
        .unwrap();
    let (other, _) = extra
        .dispatch(
            &mut tx,
            "unrelated-request",
            &json!({"request_digest":"other"}),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let q = "SELECT to_jsonb(l) FROM resource_leases l WHERE resource_key='dns-zone:unrelated'";
    let before: Value = sqlx::query_scalar(q).fetch_one(&pool).await.unwrap();
    finish_execution(&pool, &one.job, &one.leases)
        .await
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE stage='needs_attention' AND status='failed'"
        )
        .await,
        1
    );
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(q)
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM execution_operations WHERE operation_id=$1")
            .bind(other)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "dispatching");
    let state: String =
        sqlx::query_scalar("SELECT state FROM execution_operations WHERE operation_id=$1")
            .bind(op)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "needs_attention");
}

#[tokio::test]
async fn an_uncertain_operation_from_another_work_on_the_resource_also_prevents_release() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    let op = operation(&pool, &one).await;
    sqlx::query("UPDATE execution_operations SET work_id='older-work',owner='older-owner',generation=0 WHERE operation_id=$1").bind(&op).execute(&pool).await.unwrap();
    finish_execution(&pool, &one.job, &one.leases)
        .await
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM resource_leases WHERE work_id='one' AND blocked"
        )
        .await,
        2
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM execution_operations WHERE operation_id=$1")
            .bind(op)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "dispatching");
}

#[tokio::test]
async fn empty_owner_cannot_claim_and_a_changed_resource_binding_cannot_release_old_resources() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), Some("dns-zone:zone")).await;
    assert!(claim_next_for(&pool, Some("")).await.is_err());
    assert_eq!(
        count(&pool, "SELECT count(*) FROM resource_leases").await,
        0
    );
    let one = claim(&pool, "owner").await;
    let before = lease_snapshot(&pool).await;
    sqlx::query("UPDATE jobs SET resource_key='dns-zone:other' WHERE id='one'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        finish_execution(&pool, &one.job, &one.leases)
            .await
            .is_err()
    );
    assert_eq!(before, lease_snapshot(&pool).await);
}

#[tokio::test]
async fn removed_node_history_keeps_its_id_and_releases_the_original_owned_scope() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    sqlx::query("DELETE FROM nodes WHERE id='node-a'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM jobs WHERE id='one' AND node_id='node-a'"
        )
        .await,
        1
    );
    finish_execution(&pool, &one.job, &one.leases)
        .await
        .unwrap();
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM nodes WHERE id='node-a'").await,
        0
    );
}

#[tokio::test]
async fn verified_evidence_is_preserved_and_does_not_cause_false_quarantine() {
    let pool = fixture().await;
    enqueue(&pool, "one", Some("node-a"), None).await;
    let one = claim(&pool, "owner").await;
    let op = operation(&pool, &one).await;
    let mut tx = pool.begin().await.unwrap();
    one.leases
        .iter()
        .find(|l| l.resource == "node:node-a")
        .unwrap()
        .verified(&mut tx, &op, &json!({"observed_digest":"verified"}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE jobs SET status='succeeded',stage='finished' WHERE id='one'")
        .execute(&pool)
        .await
        .unwrap();
    let q = "SELECT to_jsonb(o) FROM execution_operations o WHERE operation_id=$1";
    let before: Value = sqlx::query_scalar(q)
        .bind(&op)
        .fetch_one(&pool)
        .await
        .unwrap();
    finish_execution(&pool, &one.job, &one.leases)
        .await
        .unwrap();
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(q)
            .bind(&op)
            .fetch_one(&pool)
            .await
            .unwrap()
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL OR blocked"
        )
        .await,
        0
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE status='succeeded'").await,
        1
    );
}

#[tokio::test]
async fn uncertain_kick_obligations_commit_with_quarantine_and_never_acknowledge_a_newer_generation()
 {
    for newer in [false, true] {
        let pool = fixture().await;
        enqueue(&pool, "one", Some("node-a"), None).await;
        sqlx::query("UPDATE jobs SET kind='kick',payload_json='{\"user_id\":\"deleted-user\",\"kick_generation\":1,\"kick_reasons\":{\"user_deleted\":true}}' WHERE id='one'").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO kick_requests(node_id,user_id,reasons,state,latest_job_id) VALUES('node-a','deleted-user','{\"user_deleted\":true}','active','one')").execute(&pool).await.unwrap();
        let one = claim(&pool, "owner").await;
        operation(&pool, &one).await;
        if newer {
            sqlx::query("UPDATE kick_requests SET generation=2,reasons=reasons||'{\"credentials_revoked\":true}'::jsonb").execute(&pool).await.unwrap();
        }
        let q = "SELECT to_jsonb(r) FROM kick_requests r";
        let before: Value = sqlx::query_scalar(q).fetch_one(&pool).await.unwrap();
        sqlx::raw_sql("CREATE FUNCTION reject_kick_quarantine() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.reconciliation_required' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_kick_quarantine BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_kick_quarantine();").execute(&pool).await.unwrap();
        assert!(
            finish_execution(&pool, &one.job, &one.leases)
                .await
                .is_err()
        );
        assert_eq!(
            before,
            sqlx::query_scalar::<_, Value>(q)
                .fetch_one(&pool)
                .await
                .unwrap()
        );
        assert_eq!(
            count(&pool, "SELECT count(*) FROM resource_leases WHERE blocked").await,
            0
        );
        sqlx::raw_sql("DROP TRIGGER reject_kick_quarantine ON job_events")
            .execute(&pool)
            .await
            .unwrap();
        finish_execution(&pool, &one.job, &one.leases)
            .await
            .unwrap();
        let after: Value = sqlx::query_scalar(q).fetch_one(&pool).await.unwrap();
        if newer {
            assert_eq!(before, after);
        } else {
            assert_eq!(after["state"], "needs_attention");
            assert_eq!(after["generation"], 1);
            assert_eq!(after["reasons"], before["reasons"]);
        }
        assert_eq!(
            count(&pool, "SELECT count(*) FROM resource_leases WHERE blocked").await,
            2
        );
    }
}
