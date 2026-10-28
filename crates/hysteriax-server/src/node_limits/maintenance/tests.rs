use super::*;

async fn fixture(users: i32) -> PgPool {
    let pool = db::test_pool_with_max_connections(8).await;
    nodes(&pool, 2).await;
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) SELECT 'u-'||lpad(i::text,4,'0'),'User',now(),now() FROM generate_series(1,$1)i")
        .bind(users).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) SELECT u.id,n.id,u.id||n.id,'unused',now() FROM users u CROSS JOIN nodes n")
        .execute(&pool).await.unwrap();
    pool
}
async fn nodes(pool: &PgPool, n: i32) {
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT 'n-'||lpad(i::text,4,'0'),'Node','127.0.0.1',22,'root','password','unused','example.test',443,':443','token-'||i,'unused','unused','{}',now(),now() FROM generate_series(1,$1)i ON CONFLICT DO NOTHING")
        .bind(n).execute(pool).await.unwrap();
}
async fn count(pool: &PgPool, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn package(pool: &PgPool, node: &str, blocked: bool) {
    let (node, package) = (
        node.to_owned(),
        Package {
            quota_bytes: blocked.then_some(1),
            ..Package::default()
        },
    );
    db::serializable::<_, ApiError, _>(pool, move |tx| {
        let (node, package) = (node.clone(), package.clone());
        Box::pin(async move {
            // Save the quota with zero usage, then cross it in the same atomic
            // source transaction, as a network sample or usage correction does.
            save(tx, &node, &package).await?;
            sqlx::query("UPDATE node_packages SET usage_bytes=10 WHERE node_id=$1")
                .bind(&node)
                .execute(&mut **tx)
                .await?;
            let now = sqlx::query_scalar("SELECT clock_timestamp()")
                .fetch_one(&mut **tx)
                .await?;
            evaluate(tx, &node, now).await?;
            Ok(())
        })
    })
    .await
    .unwrap();
}
async fn claim(pool: &PgPool, owner: &str) -> Work {
    sqlx::query(
        "UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1 AND state='queued'",
    )
    .bind(KIND)
    .execute(pool)
    .await
    .unwrap();
    work_queue::claim(pool, KIND, owner).await.unwrap().unwrap()
}
async fn scan(pool: &PgPool) -> Work {
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1")
        .bind(SCAN_KIND)
        .execute(pool)
        .await
        .unwrap();
    work_queue::ensure(
        pool,
        "test-node-scan",
        SCAN_KIND,
        json!({"after_node":null}),
    )
    .await
    .unwrap();
    work_queue::claim(pool, SCAN_KIND, "scan-owner")
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn restriction_pages_survive_takeover_and_meter_edits_without_duplicate_jobs() {
    let pool = fixture(257).await;
    package(&pool, "n-0001", true).await;
    assert!(restricted(&pool, "n-0001").await.unwrap());
    assert_eq!(count(&pool, "SELECT count(*) FROM jobs").await, 0);
    let old = claim(&pool, "old").await;
    dispatch_work(&pool, &old).await.unwrap();
    assert_eq!(count(&pool, "SELECT count(*) FROM jobs").await, 128);
    // Still blocked: changing meter/config generation must keep this episode.
    package(&pool, "n-0001", true).await;
    assert_eq!(
        count(
            &pool,
            "SELECT generation FROM node_limit_epochs WHERE node_id='n-0001'"
        )
        .await,
        1
    );
    let paused = claim(&pool, "paused").await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&paused.id).execute(&pool).await.unwrap();
    let next = claim(&pool, "next").await;
    assert_eq!(next.id, paused.id);
    assert!(next.leases[0].generation > paused.leases[0].generation);
    assert!(dispatch_work(&pool, &paused).await.is_err());
    dispatch_work(&pool, &next).await.unwrap();
    assert_eq!(count(&pool, "SELECT count(*) FROM jobs").await, 256);
    drain_for_test(&pool).await;
    assert_eq!(count(&pool, "SELECT count(*) FROM jobs").await, 257);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM kick_requests WHERE reasons ? 'node_limit'"
        )
        .await,
        257
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM job_events").await, 257);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM execution_operations").await,
        0
    );
}

#[tokio::test]
async fn restored_pages_preserve_permanent_reasons_and_old_restore_cannot_clear_a_new_episode() {
    let pool = fixture(257).await;
    package(&pool, "n-0001", true).await;
    drain_for_test(&pool).await;
    db::serializable::<_, ApiError, _>(&pool, |tx| {
        Box::pin(async move {
            crate::api::enqueue_job_with_payload_in_tx(
                tx,
                "kick",
                Some("n-0001"),
                None,
                json!({"user_id":"u-0001","kick_reason":"credentials_revoked"}),
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    package(&pool, "n-0001", false).await;
    let old = claim(&pool, "restore").await;
    dispatch_work(&pool, &old).await.unwrap();
    assert_eq!(count(&pool,"SELECT count(*) FROM kick_requests WHERE reasons ? 'node_limit' AND state NOT IN ('completed','cancelled')").await,129);
    assert_eq!(count(&pool,"SELECT count(*) FROM kick_requests WHERE reasons ? 'credentials_revoked' AND state NOT IN ('completed','cancelled')").await,1);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE status='queued'").await,
        130
    );
    // Restore remaining pages after re-blocking would incorrectly remove new
    // obligations. The independent episode generation rejects them instead.
    package(&pool, "n-0001", true).await;
    sqlx::query("UPDATE background_work SET available_at='1990-01-01' WHERE id=$1")
        .bind(&old.id)
        .execute(&pool)
        .await
        .unwrap();
    let stale = work_queue::claim(&pool, KIND, "stale-restore")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale.id, old.id);
    dispatch_work(&pool, &stale).await.unwrap();
    assert_eq!(count(&pool,"SELECT count(*) FROM kick_requests WHERE reasons ? 'node_limit' AND state NOT IN ('completed','cancelled')").await,129);
    drain_for_test(&pool).await;
    assert_eq!(count(&pool,"SELECT count(*) FROM kick_requests WHERE reasons ? 'node_limit' AND state NOT IN ('completed','cancelled')").await,257);
    package(&pool, "n-0001", false).await;
    drain_for_test(&pool).await;
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE status='queued'").await,
        1
    );
    assert_eq!(count(&pool,"SELECT count(*) FROM kick_requests WHERE reasons ? 'node_limit' AND state NOT IN ('completed','cancelled')").await,0);
}

#[tokio::test]
async fn scan_is_bounded_skips_busy_nodes_and_wraps_to_revisit_them() {
    let pool = fixture(0).await;
    nodes(&pool, 301).await;
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='n-0001' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    for expected in [128, 256, 300] {
        let work = scan(&pool).await;
        scan_work(&pool, &work).await.unwrap();
        assert_eq!(
            count(&pool, "SELECT count(*) FROM node_packages").await,
            expected
        );
    }
    held.rollback().await.unwrap();
    let work = scan(&pool).await;
    assert!(work.payload["after_node"].is_null());
    scan_work(&pool, &work).await.unwrap();
    assert_eq!(
        count(&pool, "SELECT count(*) FROM node_packages").await,
        301
    );
    assert!(
        work_queue::claim(&pool, SCAN_KIND, "other")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn busy_node_only_defers_its_work_and_deleting_node_keeps_obligations_without_jobs() {
    let pool = fixture(2).await;
    package(&pool, "n-0001", true).await;
    package(&pool, "n-0002", true).await;
    let first = claim(&pool, "first").await;
    assert_eq!(first.payload["node_id"], "n-0001");
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='n-0001' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    assert!(dispatch_work(&pool, &first).await.is_err());
    first.retry(&pool).await.unwrap();
    let second = work_queue::claim(&pool, KIND, "second")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.payload["node_id"], "n-0002");
    dispatch_work(&pool, &second).await.unwrap();
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE node_id='n-0002'").await,
        2
    );
    held.rollback().await.unwrap();
    sqlx::query("UPDATE nodes SET state='deleting' WHERE id='n-0001'")
        .execute(&pool)
        .await
        .unwrap();
    drain_for_test(&pool).await;
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE node_id='n-0001'").await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM kick_requests WHERE node_id='n-0001'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn fanout_ack_failure_rolls_back_jobs_events_and_cursor_and_native_conflict_retries_once() {
    let pool = fixture(2).await;
    package(&pool, "n-0001", true).await;
    let work = claim(&pool, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION fail_node_limit_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='node-limit-dispatch' AND NEW.state IN ('queued','complete') AND OLD.state='running' THEN RAISE EXCEPTION 'injected acknowledgement failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER test_node_limit_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION fail_node_limit_ack();").execute(&pool).await.unwrap();
    assert!(dispatch_work(&pool, &work).await.is_err());
    assert_eq!(count(&pool, "SELECT count(*) FROM jobs").await, 0);
    assert_eq!(count(&pool, "SELECT count(*) FROM job_events").await, 0);
    assert_eq!(count(&pool, "SELECT count(*) FROM kick_requests").await, 0);
    assert_eq!(count(&pool,"SELECT count(*) FROM background_work WHERE state='running' AND payload->'after_user'='null'").await,1);
    sqlx::raw_sql("DROP TRIGGER test_node_limit_ack ON background_work; CREATE SEQUENCE retry_node_limit; CREATE FUNCTION retry_node_limit_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='node-limit-dispatch' AND NEW.state='complete' AND OLD.state='running' AND nextval('retry_node_limit')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER test_node_limit_retry BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_node_limit_ack();").execute(&pool).await.unwrap();
    dispatch_work(&pool, &work).await.unwrap();
    assert_eq!(
        count(&pool, "SELECT last_value FROM retry_node_limit").await,
        2
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM jobs").await, 2);
    assert_eq!(count(&pool, "SELECT count(*) FROM job_events").await, 2);
    assert_eq!(
        count(&pool, "SELECT max(generation) FROM kick_requests").await,
        1
    );
}

#[tokio::test]
async fn restriction_admission_failure_rolls_back_source_usage_marker_epoch_and_audit() {
    let pool = fixture(1).await;
    package(&pool, "n-0001", false).await;
    let before: Value =
        sqlx::query_scalar("SELECT to_jsonb(p) FROM node_packages p WHERE node_id='n-0001'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_node_limit_admission() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='node-limit-dispatch' THEN RAISE EXCEPTION 'admission failed'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_limit_work BEFORE INSERT ON background_work FOR EACH ROW EXECUTE FUNCTION reject_node_limit_admission();").execute(&pool).await.unwrap();
    let result: Result<(), ApiError> = db::serializable(&pool, |tx| {
        Box::pin(async move {
            save(
                tx,
                "n-0001",
                &Package {
                    quota_bytes: Some(1),
                    ..Package::default()
                },
            )
            .await?;
            Ok(())
        })
    })
    .await;
    assert!(result.is_err());
    let after: Value =
        sqlx::query_scalar("SELECT to_jsonb(p) FROM node_packages p WHERE node_id='n-0001'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM node_limit_epochs").await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM audit_records WHERE action='node.restricted'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn new_expiry_supersedes_queued_restore_and_removed_node_work_finishes_without_recreating_data()
 {
    let pool = fixture(1).await;
    package(&pool, "n-0001", true).await;
    drain_for_test(&pool).await;
    package(&pool, "n-0001", false).await;
    let restore = claim(&pool, "restore").await;
    sqlx::query("UPDATE node_packages SET config=jsonb_set(config,'{expires_at}',to_jsonb('2000-01-01T00:00:00Z'::text)) WHERE node_id='n-0001'").execute(&pool).await.unwrap();
    dispatch_work(&pool, &restore).await.unwrap();
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE status='queued'").await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT generation FROM node_limit_epochs WHERE node_id='n-0001'"
        )
        .await,
        3
    );
    let pending = claim(&pool, "pending").await;
    sqlx::query("DELETE FROM nodes WHERE id='n-0001'")
        .execute(&pool)
        .await
        .unwrap();
    dispatch_work(&pool, &pending).await.unwrap();
    assert_eq!(
        count(&pool, "SELECT count(*) FROM node_limit_epochs").await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM background_work WHERE error_code='node_removed'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn scan_ack_failure_rolls_back_alerts_source_transitions_outboxes_and_progress() {
    let pool = fixture(0).await;
    sqlx::query("INSERT INTO node_packages(node_id,period_id,config) SELECT id,id,$1 FROM nodes")
        .bind(json!(Package {
            expires_at: Some("2000-01-01T00:00:00Z".parse().unwrap()),
            ..Package::default()
        }))
        .execute(&pool)
        .await
        .unwrap();
    let work = scan(&pool).await;
    sqlx::raw_sql("CREATE FUNCTION fail_package_scan_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='node-limit-scan' AND NEW.state='complete' THEN RAISE EXCEPTION 'scan acknowledgement failed'; END IF; RETURN NEW; END $$; CREATE TRIGGER fail_scan_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION fail_package_scan_ack();").execute(&pool).await.unwrap();
    assert!(scan_work(&pool, &work).await.is_err());
    assert_eq!(
        count(&pool, "SELECT count(*) FROM node_packages WHERE restricted").await,
        0
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM node_alerts").await, 0);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM node_limit_epochs").await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM background_work WHERE kind='node-limit-dispatch'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM audit_records WHERE action='node.restricted'"
        )
        .await,
        0
    );
    assert_eq!(count(&pool,"SELECT count(*) FROM background_work WHERE state='running' AND payload->'after_node'='null'").await,1);
    sqlx::query("DROP TRIGGER fail_scan_ack ON background_work")
        .execute(&pool)
        .await
        .unwrap();
    scan_work(&pool, &work).await.unwrap();
    assert_eq!(
        count(&pool, "SELECT count(*) FROM node_packages WHERE restricted").await,
        2
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM background_work WHERE kind='node-limit-dispatch'"
        )
        .await,
        2
    );
}
