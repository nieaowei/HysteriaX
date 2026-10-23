use super::*;
use crate::execution;
use serde_json::Value;

async fn fixture() -> (PgPool, JobInput, Vec<execution::Execution>) {
    let pool = crate::db::test_pool_with_max_connections(6).await;
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('historical-user','Historical user',now(),now())")
        .execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,state,created_at,updated_at) VALUES('node','node','127.0.0.1',22,'root','password','unused','example.test',443,':443','hash','unused','unused','{}','deleting',now(),now())").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,attempts,available_at,created_at,updated_at) VALUES('job','record-remove','node',1,'running','starting',1,now(),now(),now())").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) SELECT i::text,'node','historical-user','instance',0,0,10,20,now()+make_interval(secs=>i) FROM generate_series(1,5)i").execute(&pool).await.unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&pool).await.unwrap();
    let job = JobInput {
        id: "job".into(),
        kind: "record-remove".into(),
        node_id: Some("node".into()),
        target_revision: Some(1),
        payload: json!({}),
        attempts: 1,
    };
    let mut tx = pool.begin().await.unwrap();
    let mut leases = vec![];
    for key in ["job:job", "node:node"] {
        leases.push(
            execution::acquire(&mut tx, key, "owner", "job")
                .await
                .unwrap()
                .unwrap(),
        );
    }
    tx.commit().await.unwrap();
    (pool, job, leases)
}
async fn page(
    pool: &PgPool,
    job: &JobInput,
    leases: Vec<execution::Execution>,
) -> Option<Result<(i64, Option<(DateTime<Utc>, String)>), sqlx::Error>> {
    execution::supervise(
        pool.clone(),
        leases,
        purge_batch(pool, job, "node", &HISTORY[0], None, 2, 0),
    )
    .await
}
async fn snapshot(pool: &PgPool) -> Value {
    sqlx::query_scalar("SELECT jsonb_build_object('records',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM traffic_records r),'jobs',(SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j))").fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn batch_checkpoint_is_bounded_and_commits_without_the_old_global_lock() {
    let (pool, job, leases) = fixture().await;
    let legacy = db::begin_write(&pool).await.unwrap();
    let (removed, cursor) =
        tokio::time::timeout(std::time::Duration::from_secs(2), page(&pool, &job, leases))
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    assert_eq!(removed, 2);
    assert_eq!(cursor.unwrap().1, "4");
    let result: Value = sqlx::query_scalar("SELECT result_json FROM jobs WHERE id='job'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(result["result"]["removed_history_rows"], 2);
    assert_eq!(result["result"]["cleanup_cursor"]["id"], "4");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        3
    );
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn checkpoint_rejection_rolls_back_history_and_native_conflict_commits_one_page() {
    let (pool, job, leases) = fixture().await;
    let before = snapshot(&pool).await;
    sqlx::raw_sql("CREATE FUNCTION reject_cleanup_checkpoint() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.stage='removing_history' THEN RAISE EXCEPTION 'reject'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_cleanup_checkpoint BEFORE UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION reject_cleanup_checkpoint();").execute(&pool).await.unwrap();
    assert!(page(&pool, &job, leases.clone()).await.unwrap().is_err());
    assert_eq!(before, snapshot(&pool).await);
    sqlx::raw_sql("DROP TRIGGER reject_cleanup_checkpoint ON jobs; CREATE SEQUENCE cleanup_attempts; CREATE FUNCTION retry_cleanup_checkpoint() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.stage='removing_history' AND nextval('cleanup_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='retry'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_cleanup_checkpoint BEFORE UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION retry_cleanup_checkpoint();").execute(&pool).await.unwrap();
    assert_eq!(page(&pool, &job, leases).await.unwrap().unwrap().0, 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM traffic_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM cleanup_attempts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn old_generation_wrong_job_or_changed_node_revision_cannot_delete_or_checkpoint() {
    let (pool, job, leases) = fixture().await;
    let before = snapshot(&pool).await;
    for mode in 0..4 {
        let mut wrong = job.clone();
        match mode {
            0 => wrong.id = "other".into(),
            1 => wrong.attempts += 1,
            2 => wrong.payload = json!({"changed":true}),
            _ => wrong.node_id = Some("other-node".into()),
        }
        assert!(page(&pool, &wrong, leases.clone()).await.unwrap().is_err());
        assert_eq!(before, snapshot(&pool).await);
    }
    sqlx::query("UPDATE nodes SET desired_revision=2 WHERE id='node'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(page(&pool, &job, leases.clone()).await.unwrap().is_err());
    assert_eq!(before, snapshot(&pool).await);
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    for l in &leases {
        execution::acquire(&mut tx, &l.resource, "next", "job")
            .await
            .unwrap()
            .unwrap();
    }
    tx.commit().await.unwrap();
    assert!(page(&pool, &job, leases).await.is_none());
    assert_eq!(before, snapshot(&pool).await);
}

#[tokio::test]
async fn persisted_cancellation_prevents_a_new_cleanup_page_and_keeps_the_cursor() {
    let (pool, job, leases) = fixture().await;
    sqlx::query("UPDATE jobs SET status='cancelled' WHERE id='job'")
        .execute(&pool)
        .await
        .unwrap();
    let before = snapshot(&pool).await;
    assert!(page(&pool, &job, leases).await.is_none());
    assert_eq!(before, snapshot(&pool).await);
}

#[tokio::test]
async fn a_busy_history_row_cannot_be_skipped_by_the_durable_cursor() {
    let (pool, job, leases) = fixture().await;
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM traffic_records WHERE id='5' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let before = snapshot(&pool).await;
    let error = page(&pool, &job, leases.clone())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("55P03")
    );
    assert_eq!(before, snapshot(&pool).await);
    held.rollback().await.unwrap();
    assert_eq!(page(&pool, &job, leases).await.unwrap().unwrap().0, 2);
}
