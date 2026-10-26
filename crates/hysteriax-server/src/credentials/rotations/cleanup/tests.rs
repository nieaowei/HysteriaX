use super::*;
async fn fixture(n: i32) -> (AppState, String) {
    let (state, ca, _) = super::super::trust::tests::fixture(n).await;
    sqlx::query("UPDATE mtls_ca_generations SET state='retired' WHERE ca_version=1")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_ca_generations SET state='published',published_at=clock_timestamp() WHERE ca_version=2").execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE mtls_authorities SET published_version=2")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_rotations SET phase='complete'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO mtls_trust_cleanup(ca_id,ca_version,next_action_at) VALUES($1,1,'2000-01-01')",
    )
    .bind(&ca)
    .execute(&state.pool)
    .await
    .unwrap();
    (state, ca)
}
async fn scan_work(state: &AppState) -> Work {
    sqlx::query(
        "UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1 AND state='complete'",
    )
    .bind(SCAN)
    .execute(&state.pool)
    .await
    .unwrap();
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN, json!({}))
        .await
        .unwrap();
    maintenance::claim(&state.pool, SCAN, "scanner")
        .await
        .unwrap()
        .unwrap()
}
async fn work(state: &AppState, ca: &str, owner: &str) -> Work {
    let scanner = scan_work(state).await;
    scan(&state.pool, &scanner).await.unwrap();
    sqlx::query(
        "UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1 AND state='queued'",
    )
    .bind(KIND)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE mtls_trust_cleanup SET next_action_at='2000-01-01' WHERE ca_id=$1 AND ca_version=1 AND completed_at IS NULL").bind(ca).execute(&state.pool).await.unwrap();
    maintenance::claim_id(&state.pool, KIND, owner, &format!("trust-cleanup:{ca}:1"))
        .await
        .unwrap()
        .unwrap()
}
async fn count(state: &AppState, q: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(q))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn done(state: &AppState, ca: &str) -> bool {
    sqlx::query_scalar(
        "SELECT completed_at IS NOT NULL FROM mtls_trust_cleanup WHERE ca_id=$1 AND ca_version=1",
    )
    .bind(ca)
    .fetch_one(&state.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn empty_retention_uses_database_time_and_cleanup_finishes_without_the_old_global_lock() {
    let (state, ca) = fixture(0).await;
    let w = work(&state, &ca, "owner").await;
    let held = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), page(&state.pool, &w))
        .await
        .unwrap()
        .unwrap();
    assert!(done(&state, &ca).await);
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    assert!(page(&state.pool, &w).await.is_err());
    held.rollback().await.unwrap();
}

#[tokio::test]
async fn three_hundred_one_live_members_enqueue_one_hundred_jobs_per_page_and_retain_the_obligation()
 {
    let (state, ca) = fixture(301).await;
    for expected in [100, 200, 300, 301] {
        let w = work(&state, &ca, "owner").await;
        page(&state.pool, &w).await.unwrap();
        assert_eq!(
            count(&state, "SELECT count(*) FROM jobs WHERE kind='deploy'").await,
            expected
        );
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_node_operations WHERE stage='cleanup'"
            )
            .await,
            expected
        );
        assert_eq!(
            count(&state, "SELECT count(*) FROM mtls_cleanup_nodes").await,
            expected
        );
        assert!(!done(&state, &ca).await);
    }
    let w = work(&state, &ca, "wrap").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 301);
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
}

#[tokio::test]
async fn busy_early_nodes_are_skipped_and_revisited_after_the_cursor_wraps() {
    let (state, ca) = fixture(102).await;
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='member-0001' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    for expected in [99, 101] {
        let w = work(&state, &ca, "owner").await;
        page(&state.pool, &w).await.unwrap();
        assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, expected);
    }
    held.rollback().await.unwrap();
    let w = work(&state, &ca, "wrap").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 102);
    assert!(!done(&state, &ca).await);
}

#[tokio::test]
async fn acknowledgement_failure_rolls_back_jobs_events_operations_cache_and_cursor_then_native_retry_is_unique()
 {
    let (state, ca) = fixture(101).await;
    let w = work(&state, &ca, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_cleanup_page() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-trust-cleanup-page' AND NEW.state<>'running' THEN RAISE EXCEPTION 'reject page'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_cleanup_page BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_cleanup_page();").execute(&state.pool).await.unwrap();
    assert!(page(&state.pool, &w).await.is_err());
    for q in [
        "SELECT count(*) FROM jobs",
        "SELECT count(*) FROM job_events",
        "SELECT count(*) FROM mtls_node_operations",
        "SELECT count(*) FROM mtls_cleanup_nodes",
    ] {
        assert_eq!(count(&state, q).await, 0);
    }
    sqlx::raw_sql("DROP TRIGGER reject_cleanup_page ON background_work; CREATE SEQUENCE cleanup_attempts; CREATE FUNCTION retry_cleanup_page() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-trust-cleanup-page' AND NEW.state<>'running' AND nextval('cleanup_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_cleanup_page BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_cleanup_page();").execute(&state.pool).await.unwrap();
    page(&state.pool, &w).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 100);
    assert_eq!(count(&state, "SELECT count(*) FROM job_events").await, 100);
    assert_eq!(
        count(&state, "SELECT last_value FROM cleanup_attempts").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn takeover_preserves_the_cursor_and_fences_every_old_page_write() {
    let (state, ca) = fixture(301).await;
    let w = work(&state, &ca, "first").await;
    page(&state.pool, &w).await.unwrap();
    let old = work(&state, &ca, "old").await;
    assert_eq!(old.payload["cursor"], "member-0100");
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = work(&state, &ca, "next").await;
    assert_eq!(old.payload, next.payload);
    assert!(page(&state.pool, &old).await.is_err());
    page(&state.pool, &next).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 200);
}

async fn real_fixture(expire: bool) -> (AppState, String, Vec<String>) {
    let (state, ca, r, nodes) =
        crate::api::managed_mtls::tests::rotation_flow::probe_switch_fixture().await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let phase: String = sqlx::query_scalar("SELECT phase FROM mtls_rotations WHERE id=$1")
                .bind(&r)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            if phase == "complete" {
                break;
            }
            super::super::advance(&state, &r).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    if expire {
        sqlx::query("UPDATE mtls_probe_versions SET expires_at=clock_timestamp()-interval '6 minutes' WHERE ca_id=$1 AND ca_version=1").bind(&ca).execute(&state.pool).await.unwrap();
    }
    sqlx::query(
        "UPDATE mtls_trust_cleanup SET next_action_at='2000-01-01' WHERE ca_id=$1 AND ca_version=1",
    )
    .bind(&ca)
    .execute(&state.pool)
    .await
    .unwrap();
    (state, ca, nodes)
}

#[tokio::test]
async fn actual_cleanup_preserves_encrypted_sources_and_only_finishes_after_both_verified_deployments()
 {
    let (state, ca, nodes) = real_fixture(true).await;
    let before:Value=sqlx::query_scalar("SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v),'snapshots',(SELECT jsonb_agg(to_jsonb(v) ORDER BY node_id,revision) FROM mtls_deployment_snapshots v))").fetch_one(&state.pool).await.unwrap();
    let w = work(&state, &ca, "owner").await;
    page(&state.pool, &w).await.unwrap();
    let after:Value=sqlx::query_scalar("SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v),'snapshots',(SELECT jsonb_agg(to_jsonb(v) ORDER BY node_id,revision) FROM mtls_deployment_snapshots v))").fetch_one(&state.pool).await.unwrap();
    assert_eq!(before, after);
    for (index, node) in nodes.iter().enumerate() {
        let job: String = sqlx::query_scalar(
            "SELECT job_id FROM mtls_cleanup_nodes WHERE ca_id=$1 AND ca_version=1 AND node_id=$2",
        )
        .bind(&ca)
        .bind(node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        crate::api::managed_mtls::tests::rotation_flow::complete_cleanup_fixture(&state, &job)
            .await;
        let w = work(&state, &ca, "confirm").await;
        page(&state.pool, &w).await.unwrap();
        assert_eq!(done(&state, &ca).await, index == nodes.len() - 1);
    }
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_node_trust WHERE ca_version=1 AND deployed"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_node_trust WHERE ca_version=2 AND deployed"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_cleanup_nodes WHERE completed"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn retained_actual_leaf_deadlines_and_the_waiting_acknowledgement_are_atomic() {
    let (state, ca, _) = real_fixture(false).await;
    let w = work(&state, &ca, "owner").await;
    let before: DateTime<Utc> = sqlx::query_scalar(
        "SELECT next_action_at FROM mtls_trust_cleanup WHERE ca_id=$1 AND ca_version=1",
    )
    .bind(&ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_retention_wait() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-trust-cleanup-page' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject waiting acknowledgement'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_retention_wait BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_retention_wait();").execute(&state.pool).await.unwrap();
    assert!(page(&state.pool, &w).await.is_err());
    let rolled_back: DateTime<Utc> = sqlx::query_scalar(
        "SELECT next_action_at FROM mtls_trust_cleanup WHERE ca_id=$1 AND ca_version=1",
    )
    .bind(&ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(before, rolled_back);
    sqlx::query("DROP TRIGGER reject_retention_wait ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    page(&state.pool, &w).await.unwrap();
    assert!(!done(&state, &ca).await);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_node_operations WHERE stage='cleanup'"
        )
        .await,
        0
    );
    let deadline: DateTime<Utc> = sqlx::query_scalar(RETENTION_DEADLINE)
        .bind(&ca)
        .bind(1_i64)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let stored: DateTime<Utc> = sqlx::query_scalar(
        "SELECT next_action_at FROM mtls_trust_cleanup WHERE ca_id=$1 AND ca_version=1",
    )
    .bind(&ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(deadline, stored);
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-trust-cleanup-page' AND state='complete' AND error_code='trust_cleanup_waiting_retention'").await,1);
}

#[tokio::test]
async fn queued_running_waiting_and_nonstale_failed_jobs_keep_their_pointer_until_explicit_retry_or_source_change()
 {
    let (state, ca) = fixture(5).await;
    let w = work(&state, &ca, "first").await;
    page(&state.pool, &w).await.unwrap();
    sqlx::query("UPDATE jobs SET status=CASE node_id WHEN 'member-0001' THEN 'queued' WHEN 'member-0002' THEN 'running' WHEN 'member-0003' THEN 'waiting_confirmation' WHEN 'member-0004' THEN 'failed' ELSE 'succeeded' END").execute(&state.pool).await.unwrap();
    let before: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(c) ORDER BY node_id) FROM mtls_cleanup_nodes c",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let w = work(&state, &ca, "revisit").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(
            "SELECT jsonb_agg(to_jsonb(c) ORDER BY node_id) FROM mtls_cleanup_nodes c"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap()
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 5);
    sqlx::query("UPDATE nodes SET desired_revision=desired_revision+1,deployed_revision=deployed_revision+1 WHERE id='member-0004'").execute(&state.pool).await.unwrap();
    let w = work(&state, &ca, "changed").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 6);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE node_id='member-0004' AND status='failed'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn deleting_or_recovering_nodes_retain_the_obligation_without_new_remote_jobs() {
    let (state, ca) = fixture(4).await;
    sqlx::query("UPDATE nodes SET state=CASE id WHEN 'member-0001' THEN 'deleting' WHEN 'member-0002' THEN 'delete_failed' ELSE state END,mtls_state=CASE id WHEN 'member-0003' THEN 'recovery_required' ELSE mtls_state END").execute(&state.pool).await.unwrap();
    let w = work(&state, &ca, "owner").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_cleanup_nodes").await,
        4
    );
    assert!(!done(&state, &ca).await);
}

#[tokio::test]
async fn unconfirmed_remote_trust_remains_authoritative_after_deployed_and_desired_flags_are_cleared()
 {
    let (state, ca) = fixture(1).await;
    sqlx::query("UPDATE mtls_node_trust SET desired=false,deployed=false")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET mtls_state='recovery_required'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,status,stage,available_at,created_at,updated_at) VALUES('uncertain','deploy','member-0001','failed','recovery',now(),now(),now())").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_activations(id,node_id,job_id,generation,target_revision,phase,previous_state,target_digest,snapshot_enc,trust,created_at,updated_at) VALUES('uncertain','member-0001','uncertain',1,1,'recovery_required','ready','digest',$1,'[]',now(),now())").bind(state.secrets.encrypt("{}").unwrap()).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_activation_trust(activation_id,ca_id,ca_version) VALUES('uncertain',$1,1)").bind(&ca).execute(&state.pool).await.unwrap();
    let w = work(&state, &ca, "owner").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
    assert!(!done(&state, &ca).await);
    sqlx::query("UPDATE mtls_cleanup_nodes SET completed=true")
        .execute(&state.pool)
        .await
        .unwrap();
    let w = work(&state, &ca, "cached").await;
    page(&state.pool, &w).await.unwrap();
    assert!(!done(&state, &ca).await);
}

#[tokio::test]
async fn absent_anchor_cache_is_confirmed_in_bounded_pages_and_busy_node_parents_are_revisited() {
    let (state, ca) = fixture(301).await;
    sqlx::query(
        "INSERT INTO mtls_cleanup_nodes(ca_id,ca_version,node_id) SELECT $1,1,id FROM nodes",
    )
    .bind(&ca)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE mtls_node_trust SET desired=false,deployed=false")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='member-0001' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    for remaining in [202, 103, 4, 1, 1] {
        let w = work(&state, &ca, "owner").await;
        page(&state.pool, &w).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_cleanup_nodes WHERE NOT completed"
            )
            .await,
            remaining
        );
        assert!(!done(&state, &ca).await);
    }
    held.rollback().await.unwrap();
    let w = work(&state, &ca, "resumed").await;
    page(&state.pool, &w).await.unwrap();
    assert!(done(&state, &ca).await);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_cleanup_nodes WHERE NOT completed"
        )
        .await,
        0
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}

#[tokio::test]
async fn healthy_owner_and_incorrect_work_scope_cannot_be_reset_or_publish_results() {
    let (state, ca) = fixture(1).await;
    let w = work(&state, &ca, "healthy").await;
    pass(&state).await.unwrap();
    let mut wrong = w.clone();
    wrong.payload["ca_version"] = json!(2);
    assert!(page(&state.pool, &wrong).await.is_err());
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-trust-cleanup-page' AND owner='healthy' AND state='running'").await,1);
    for lease in &w.leases {
        assert!(lease.renew(&state.pool).await.unwrap());
    }
    page(&state.pool, &w).await.unwrap();
}

async fn many(state: &AppState, ca: &str, n: i32) {
    super::super::trust::tests::many_rotations(state, ca, n).await;
    sqlx::query("UPDATE mtls_authorities SET published_version=2")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_rotations SET phase='complete'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO mtls_trust_cleanup(ca_id,ca_version,next_action_at) SELECT credential_id,1,'2000-01-01' FROM mtls_authorities ON CONFLICT DO NOTHING").execute(&state.pool).await.unwrap();
}
#[tokio::test]
async fn three_hundred_one_cleanup_generations_are_admitted_in_pages_without_resetting_healthy_work()
 {
    let (state, ca) = fixture(0).await;
    let live = work(&state, &ca, "healthy").await;
    many(&state, &ca, 300).await;
    for expected in [101, 201, 301] {
        let scanner = scan_work(&state).await;
        scan(&state.pool, &scanner).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-trust-cleanup-page'"
            )
            .await,
            expected
        );
    }
    for lease in &live.leases {
        assert!(lease.renew(&state.pool).await.unwrap());
    }
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE owner='healthy' AND state='running'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn admission_acknowledgement_failure_and_native_retry_keep_a_single_bounded_page() {
    let (state, ca) = fixture(0).await;
    many(&state, &ca, 100).await;
    let scanner = scan_work(&state).await;
    sqlx::raw_sql("CREATE FUNCTION reject_cleanup_scan() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-trust-cleanup-admission' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject scan'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_cleanup_scan BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_cleanup_scan();").execute(&state.pool).await.unwrap();
    assert!(scan(&state.pool, &scanner).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-trust-cleanup-page'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_cleanup_scan ON background_work; CREATE SEQUENCE cleanup_scan_attempts; CREATE FUNCTION retry_cleanup_scan() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-trust-cleanup-admission' AND NEW.state='complete' AND nextval('cleanup_scan_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_cleanup_scan BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_cleanup_scan();").execute(&state.pool).await.unwrap();
    scan(&state.pool, &scanner).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-trust-cleanup-page'"
        )
        .await,
        100
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM cleanup_scan_attempts").await,
        2
    );
}

#[tokio::test]
async fn an_empty_future_cleanup_range_uses_the_ledger_deadline_index() {
    let (state, ca) = fixture(0).await;
    many(&state, &ca, 1000).await;
    sqlx::query("UPDATE mtls_trust_cleanup SET next_action_at=clock_timestamp()+interval '1 day'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("ANALYZE mtls_trust_cleanup; ANALYZE mtls_ca_generations; ANALYZE mtls_authorities; ANALYZE background_work;").execute(&state.pool).await.unwrap();
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE,FORMAT JSON) {ADMISSION}"
    )))
    .bind(now)
    .bind(PAGE)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(
        plan.to_string().contains("mtls_trust_cleanup_due"),
        "{plan}"
    );
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
}

#[tokio::test]
async fn final_confirmation_failure_keeps_the_ledger_open_until_atomic_acknowledgement() {
    let (state, ca) = fixture(0).await;
    let w = work(&state, &ca, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_cleanup_finish() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-trust-cleanup-page' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject completion'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_cleanup_finish BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_cleanup_finish();").execute(&state.pool).await.unwrap();
    assert!(page(&state.pool, &w).await.is_err());
    assert!(!done(&state, &ca).await);
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-trust-cleanup-page' AND owner='owner' AND state='running'").await,1);
    sqlx::query("DROP TRIGGER reject_cleanup_finish ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    page(&state.pool, &w).await.unwrap();
    assert!(done(&state, &ca).await);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn current_or_preparing_anchors_and_a_closed_ledger_cannot_be_removed_by_old_work() {
    for change in [
        "UPDATE mtls_ca_generations SET state='retired' WHERE ca_version=2; UPDATE mtls_ca_generations SET state='published' WHERE ca_version=1; UPDATE mtls_authorities SET published_version=1;",
        "UPDATE mtls_ca_generations SET state='preparing' WHERE ca_version=1;",
        "UPDATE mtls_trust_cleanup SET completed_at=clock_timestamp();",
    ] {
        let (state, ca) = fixture(1).await;
        let w = work(&state, &ca, "owner").await;
        sqlx::raw_sql(change).execute(&state.pool).await.unwrap();
        page(&state.pool, &w).await.unwrap();
        assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_node_trust WHERE ca_version=1 AND deployed"
            )
            .await,
            1
        );
        assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-trust-cleanup-page' AND state='complete' AND error_code='trust_cleanup_superseded_or_not_due'").await,1);
    }
}
