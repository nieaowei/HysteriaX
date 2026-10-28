use super::*;
async fn fixture() -> (AppState, String, String) {
    let (state, ca, rotation) = super::super::trust::tests::fixture(0).await;
    sqlx::query("UPDATE mtls_rotations SET phase='identities'")
        .execute(&state.pool)
        .await
        .unwrap();
    (state, ca, rotation)
}
async fn work(state: &AppState, ca: &str, rotation: &str, owner: &str) -> Work {
    maintenance::ensure(
        &state.pool,
        &format!("rotation-publication:{rotation}"),
        KIND,
        payload(rotation, ca, 2),
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1 AND state='queued'",
    )
    .bind(KIND)
    .execute(&state.pool)
    .await
    .unwrap();
    maintenance::claim_id(
        &state.pool,
        KIND,
        owner,
        &format!("maintenance:rotation-publication:{rotation}"),
    )
    .await
    .unwrap()
    .unwrap()
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn published(state: &AppState) -> i64 {
    count(state, "SELECT published_version FROM mtls_authorities").await
}

#[tokio::test]
async fn publication_confirms_all_outboxes_atomically() {
    let (state, ca, rotation) = fixture().await;
    let w = work(&state, &ca, &rotation, "owner").await;
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            commit(&state.pool, &w, "fixed-audit")
        )
        .await
        .unwrap()
        .unwrap()
    );
    assert_eq!(published(&state).await, 2);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotations WHERE phase='probes' AND published_at IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind IN ('mtls-identity-publication-schedule','mtls-node-publication-admission') AND state='queued'").await,2);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL"
        )
        .await,
        0
    );
    assert!(commit(&state.pool, &w, "fixed-audit").await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE id='fixed-audit'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn acknowledgement_failure_rolls_back_publication_and_native_retry_preserves_one_audit() {
    let (state, ca, rotation) = fixture().await;
    let w = work(&state, &ca, &rotation, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_release_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-publication' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject acknowledgement'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_release_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_release_ack();").execute(&state.pool).await.unwrap();
    assert!(commit(&state.pool, &w, "fixed-audit").await.is_err());
    assert_eq!(published(&state).await, 1);
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_ca_generations WHERE ca_version=2 AND state='preparing' AND published_at IS NULL").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.published'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_trust_cleanup").await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind IN ('mtls-identity-publication-schedule','mtls-node-publication-admission')").await,0);
    sqlx::raw_sql("DROP TRIGGER reject_release_ack ON background_work; CREATE SEQUENCE release_attempts; CREATE FUNCTION retry_release_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-publication' AND NEW.state='complete' AND nextval('release_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_release_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_release_ack();").execute(&state.pool).await.unwrap();
    assert!(commit(&state.pool, &w, "fixed-audit").await.unwrap());
    assert_eq!(
        count(&state, "SELECT last_value FROM release_attempts").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE id='fixed-audit'"
        )
        .await,
        1
    );
    assert_eq!(published(&state).await, 2);
}

#[tokio::test]
async fn takeover_and_wrong_ca_scope_cannot_publish_or_acknowledge_another_work() {
    let (state, ca, rotation) = fixture().await;
    let old = work(&state, &ca, &rotation, "old").await;
    assert!(!pass(&state, &rotation, &ca, 2).await.unwrap());
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = maintenance::claim(&state.pool, KIND, "next")
        .await
        .unwrap()
        .unwrap();
    assert!(commit(&state.pool, &old, "old-audit").await.is_err());
    let mut wrong = next.clone();
    wrong.payload["ca_id"] = json!("another-ca");
    assert!(commit(&state.pool, &wrong, "wrong-audit").await.is_err());
    assert_eq!(published(&state).await, 1);
    assert!(commit(&state.pool, &next, "new-audit").await.unwrap());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.published'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn archived_expired_superseded_and_already_published_targets_only_end_old_work() {
    for change in [
        "UPDATE credentials SET archived=true WHERE kind='ca_identity'",
        "UPDATE mtls_ca_generations SET expires_at=clock_timestamp()-interval '1 second' WHERE ca_version=2",
        "UPDATE mtls_rotations SET phase='complete',outcome='superseded_unpublished_target'",
        "UPDATE mtls_rotations SET phase='probes',published_at=clock_timestamp()",
    ] {
        let (state, ca, rotation) = fixture().await;
        let w = work(&state, &ca, &rotation, "owner").await;
        sqlx::query(change).execute(&state.pool).await.unwrap();
        assert!(!commit(&state.pool, &w, "invalid-audit").await.unwrap());
        assert_eq!(published(&state).await, 1);
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM audit_records WHERE action='ca.published'"
            )
            .await,
            0
        );
        assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE state='complete' AND error_code='rotation_publication_superseded'").await,1);
    }
}

#[tokio::test]
async fn a_late_member_overrides_cached_preparation_and_keeps_the_same_work_recoverable() {
    let (state, ca, rotation) = fixture().await;
    let w = work(&state, &ca, &rotation, "first").await;
    super::super::trust::tests::nodes(&state, &ca, 1).await;
    sqlx::query("INSERT INTO mtls_rotation_nodes(rotation_id,node_id,expected_revision,trust_prepared) VALUES($1,'member-0001',1,true)").bind(&rotation).execute(&state.pool).await.unwrap();
    assert!(!commit(&state.pool, &w, "late-audit").await.unwrap());
    assert_eq!(published(&state).await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotations WHERE phase='trust'"
        )
        .await,
        1
    );
    // Taking the work again while trust is unresolved cannot discard the
    // obligation as superseded; the same durable item must continue waiting.
    let w = work(&state, &ca, &rotation, "second").await;
    assert!(!commit(&state.pool, &w, "still-late").await.unwrap());
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-rotation-publication' AND state='queued'").await,1);
    // Removing the only member is a real authoritative membership change.
    sqlx::query("UPDATE mtls_node_trust SET desired=false,deployed=false")
        .execute(&state.pool)
        .await
        .unwrap();
    let w = work(&state, &ca, &rotation, "third").await;
    assert!(commit(&state.pool, &w, "final-audit").await.unwrap());
}

#[tokio::test]
async fn a_late_assignment_and_missing_leaf_both_keep_publication_closed() {
    let (state, ca, rotation, node) =
        crate::api::managed_mtls::tests::rotation_flow::release_fixture().await;
    sqlx::query(
        "INSERT INTO users(id,name,created_at,updated_at) VALUES('late-user','Late',now(),now())",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('late-user',$1,'hash',$2,now())").bind(&node).bind(state.secrets.encrypt("late-user").unwrap()).execute(&state.pool).await.unwrap();
    let w = work(&state, &ca, &rotation, "missing-identity").await;
    assert!(!commit(&state.pool, &w, "late-user-audit").await.unwrap());
    assert_eq!(published(&state).await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotations WHERE phase='identities'"
        )
        .await,
        1
    );
    sqlx::query("INSERT INTO mtls_user_identities(id,user_id,ca_id,created_at,updated_at) VALUES(gen_random_uuid()::text,'late-user',$1,now(),now())").bind(&ca).execute(&state.pool).await.unwrap();
    let w = work(&state, &ca, &rotation, "missing-leaf").await;
    assert!(!commit(&state.pool, &w, "missing-leaf-audit").await.unwrap());
    assert_eq!(published(&state).await, 1);
    let prepared =
        identities::prepare_user(&state.pool, &state.secrets, "late-user", &ca, 2, false)
            .await
            .unwrap();
    let secrets = state.secrets.clone();
    db::serializable::<_, anyhow::Error, _>(&state.pool, move |tx| {
        let (prepared, secrets) = (prepared.clone(), secrets.clone());
        Box::pin(async move { identities::commit_user(tx, &secrets, prepared).await })
    })
    .await
    .unwrap();
    let w = work(&state, &ca, &rotation, "ready").await;
    assert!(commit(&state.pool, &w, "ready-audit").await.unwrap());
    assert_eq!(published(&state).await, 2);
}

#[tokio::test]
async fn actual_verified_activation_publishes_without_rewriting_locked_node_or_cached_member_rows()
{
    let (state, ca, rotation, node) =
        crate::api::managed_mtls::tests::rotation_flow::release_fixture().await;
    let w = work(&state, &ca, &rotation, "owner").await;
    let snapshot = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'members',(SELECT jsonb_agg(to_jsonb(r) ORDER BY node_id) FROM mtls_rotation_nodes r),'history',(SELECT jsonb_agg(to_jsonb(v) ORDER BY id) FROM config_versions v))";
    let before: Value = sqlx::query_scalar(snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR UPDATE")
        .bind(&node)
        .fetch_one(&mut *held)
        .await
        .unwrap();
    sqlx::query("SELECT rotation_id FROM mtls_rotation_nodes FOR UPDATE")
        .fetch_all(&mut *held)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            commit(&state.pool, &w, "actual-proof-audit")
        )
        .await
        .unwrap()
        .unwrap()
    );
    let after: Value = sqlx::query_scalar(snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(published(&state).await, 2);
    held.rollback().await.unwrap();
}

#[tokio::test]
async fn an_old_publication_snapshot_is_rejected_by_native_ssi_after_a_real_api_binding() {
    let (state, ca, rotation) = fixture().await;
    let w = work(&state, &ca, &rotation, "owner").await;
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1")
        .bind(&ca)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let node = crate::api::managed_mtls::tests::release_late_binding(&state, &ca).await;
    let error = match commit_on(&mut tx, &w, "stale-audit").await {
        Ok(_) => anyhow::Error::from(tx.commit().await.unwrap_err()),
        Err(error) => {
            tx.rollback().await.unwrap();
            error
        }
    };
    assert!(
        crate::db::RetryableTransactionError::retryable_transaction(&error),
        "{error:#}"
    );
    assert_eq!(published(&state).await, 1);
    assert!(!commit(&state.pool, &w, "fresh-audit").await.unwrap());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotations WHERE phase='trust'"
        )
        .await,
        1
    );
    let live:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mtls_node_trust WHERE node_id=$1 AND desired AND ca_version=1)").bind(node).fetch_one(&state.pool).await.unwrap();
    assert!(live);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.published'"
        )
        .await,
        0
    );
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
#[tokio::test]
async fn three_hundred_one_publications_are_admitted_in_pages_without_resetting_a_healthy_owner() {
    let (state, ca, rotation) = fixture().await;
    super::super::trust::tests::many_rotations(&state, &ca, 300).await;
    sqlx::query("UPDATE mtls_rotations SET phase='identities'")
        .execute(&state.pool)
        .await
        .unwrap();
    for expected in [100, 200, 300, 301] {
        let scan = scan_work(&state).await;
        admit(&state.pool, &scan).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-publication'"
            )
            .await,
            expected
        );
    }
    let live = maintenance::claim_id(
        &state.pool,
        KIND,
        "live",
        &format!("maintenance:rotation-publication:{rotation}"),
    )
    .await
    .unwrap()
    .unwrap();
    let scan = scan_work(&state).await;
    admit(&state.pool, &scan).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-publication'"
        )
        .await,
        301
    );
    assert!(!pass(&state, &rotation, &ca, 2).await.unwrap());
    for lease in &live.leases {
        assert!(lease.renew(&state.pool).await.unwrap());
    }
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE owner='live' AND state='running'"
        )
        .await,
        1
    );
    assert_eq!(published(&state).await, 1);
}

#[tokio::test]
async fn admission_confirmation_failure_and_native_retry_preserve_one_bounded_page() {
    let (state, ca, _) = fixture().await;
    super::super::trust::tests::many_rotations(&state, &ca, 100).await;
    sqlx::query("UPDATE mtls_rotations SET phase='identities'")
        .execute(&state.pool)
        .await
        .unwrap();
    let scan = scan_work(&state).await;
    sqlx::raw_sql("CREATE FUNCTION reject_release_scan() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-publication-admission' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject scan'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_release_scan BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_release_scan();").execute(&state.pool).await.unwrap();
    assert!(admit(&state.pool, &scan).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-publication'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_release_scan ON background_work; CREATE SEQUENCE release_scan_attempts; CREATE FUNCTION retry_release_scan() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-publication-admission' AND NEW.state='complete' AND nextval('release_scan_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_release_scan BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_release_scan();").execute(&state.pool).await.unwrap();
    admit(&state.pool, &scan).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-publication'"
        )
        .await,
        100
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM release_scan_attempts").await,
        2
    );
}

#[tokio::test]
async fn publication_admission_uses_an_empty_due_index_range_for_future_rotations() {
    let (state, ca, _) = fixture().await;
    super::super::trust::tests::many_rotations(&state, &ca, 1000).await;
    sqlx::query("UPDATE mtls_rotations SET phase='identities',next_action_at=clock_timestamp()+interval '1 day'").execute(&state.pool).await.unwrap();
    sqlx::raw_sql("ANALYZE mtls_rotations; ANALYZE mtls_ca_generations; ANALYZE credentials; ANALYZE background_work;").execute(&state.pool).await.unwrap();
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
    assert!(plan.to_string().contains("mtls_rotations_due"), "{plan}");
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
}

#[tokio::test]
async fn retention_seeks_the_latest_leaf_including_discarded_history_of_a_deleted_node() {
    let (state, ca, rotation, node) =
        crate::api::managed_mtls::tests::rotation_flow::release_fixture().await;
    // Retention is conservative even for discarded probe history whose node
    // has disappeared. Use its actual persisted credential/version FK.
    sqlx::query("INSERT INTO mtls_probe_versions(node_id,credential_id,credential_version,ca_id,ca_version,expires_at,next_action_at,discarded) SELECT 'deleted-probe-'||i,p.credential_id,p.credential_version,p.ca_id,p.ca_version,clock_timestamp()+make_interval(days=>i),clock_timestamp(),true FROM mtls_probe_versions p CROSS JOIN generate_series(1,1001)i WHERE p.node_id=$1 AND p.ca_version=1 LIMIT 1001").bind(&node).execute(&state.pool).await.unwrap();
    let expected: DateTime<Utc> = sqlx::query_scalar(
        "SELECT max(expires_at)+interval '5 minutes' FROM mtls_probe_versions WHERE ca_version=1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    sqlx::raw_sql("ANALYZE mtls_probe_versions; ANALYZE mtls_user_versions;")
        .execute(&state.pool)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE,FORMAT JSON) {RETENTION_DEADLINE}"
    )))
    .bind(&ca)
    .bind(1_i64)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(
        plan.to_string().contains("mtls_probe_versions_ca_expiry"),
        "{plan}"
    );
    let w = work(&state, &ca, &rotation, "owner").await;
    assert!(commit(&state.pool, &w, "retention-audit").await.unwrap());
    let deadline: DateTime<Utc> = sqlx::query_scalar(
        "SELECT next_action_at FROM mtls_trust_cleanup WHERE ca_id=$1 AND ca_version=1",
    )
    .bind(ca)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(deadline, expected);
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_probe_versions WHERE discarded AND node_id LIKE 'deleted-probe-%'").await,1001);
}

#[tokio::test]
async fn expired_actual_probe_cannot_be_hidden_by_a_cached_ready_member() {
    let (state, ca, rotation, _) =
        crate::api::managed_mtls::tests::rotation_flow::release_fixture().await;
    sqlx::query("UPDATE mtls_rotation_nodes SET trust_prepared=true")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_probe_versions SET expires_at=clock_timestamp()-interval '1 second' WHERE ca_version=2").execute(&state.pool).await.unwrap();
    let w = work(&state, &ca, &rotation, "expired-proof").await;
    assert!(
        !commit(&state.pool, &w, "expired-proof-audit")
            .await
            .unwrap()
    );
    assert_eq!(published(&state).await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotations WHERE phase='trust'"
        )
        .await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-rotation-publication' AND state='queued' AND error_code='rotation_publication_waiting_trust'").await,1);
}

#[tokio::test]
async fn a_restored_source_resumes_its_stable_work_without_resetting_healthy_or_backing_off_owners()
{
    let (state, ca, rotation) = fixture().await;
    let w = work(&state, &ca, &rotation, "before-archive").await;
    sqlx::query("UPDATE credentials SET archived=true WHERE id=$1")
        .bind(&ca)
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(!commit(&state.pool, &w, "archived-audit").await.unwrap());
    sqlx::query("UPDATE credentials SET archived=false WHERE id=$1")
        .bind(&ca)
        .execute(&state.pool)
        .await
        .unwrap();
    let scan = scan_work(&state).await;
    admit(&state.pool, &scan).await.unwrap();
    let restored = maintenance::claim(&state.pool, KIND, "restored")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.id, w.id);
    assert!(!pass(&state, &rotation, &ca, 2).await.unwrap());
    restored.retry(&state.pool).await.unwrap();
    assert!(!pass(&state, &rotation, &ca, 2).await.unwrap());
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-rotation-publication' AND state='queued' AND error_code='maintenance_deferred' AND available_at>clock_timestamp()").await,1);
    let next = work(&state, &ca, &rotation, "next").await;
    assert!(commit(&state.pool, &next, "restored-audit").await.unwrap());
    assert_eq!(published(&state).await, 2);
}
