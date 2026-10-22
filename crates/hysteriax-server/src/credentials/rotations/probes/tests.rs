use super::*;
async fn metadata_ready(state: &AppState, rotation: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            metadata::pass(state, rotation).await.unwrap();
            if !metadata::pending_on(&state.pool, rotation).await.unwrap() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("probe fixture must finish its metadata work");
}
async fn fixture() -> (AppState, String, String, String) {
    let (state, ca, r, node) =
        crate::api::managed_mtls::tests::rotation_flow::metadata_fixture(false).await;
    metadata_ready(&state, &r).await;
    (state, ca, r, node)
}
async fn claim(state: &AppState, kind: &str, owner: &str) -> Work {
    sqlx::query(
        "UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1 AND state='queued'",
    )
    .bind(kind)
    .execute(&state.pool)
    .await
    .unwrap();
    maintenance::claim(&state.pool, kind, owner)
        .await
        .unwrap()
        .unwrap()
}
async fn parent(state: &AppState) -> Work {
    claim(state, ADMIT, "admission").await
}
async fn count(state: &AppState, q: &str) -> i64 {
    sqlx::query_scalar(q).fetch_one(&state.pool).await.unwrap()
}
async fn phase(state: &AppState, r: &str) -> String {
    sqlx::query_scalar("SELECT phase FROM mtls_rotations WHERE id=$1")
        .bind(r)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn active(state: &AppState, node: &str) -> i64 {
    sqlx::query_scalar("SELECT ca_version FROM mtls_active_probes WHERE node_id=$1")
        .bind(node)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn real_probe_switch_and_completion_are_fenced_and_do_not_use_the_old_global_lock() {
    let (state, _, r, node) = fixture().await;
    assert_eq!(active(&state, &node).await, 1);
    let w = parent(&state).await;
    let held = db::begin_write(&state.pool).await.unwrap();
    page(&state.pool, &w, "not-complete").await.unwrap();
    assert_eq!(phase(&state, &r).await, "probes");
    let child = claim(&state, CHILD, "switch").await;
    switch(&state.pool, &child).await.unwrap();
    assert_eq!(active(&state, &node).await, 2);
    assert!(switch(&state.pool, &child).await.is_err());
    let w = parent(&state).await;
    page(&state.pool, &w, "completed-audit").await.unwrap();
    assert_eq!(phase(&state, &r).await, "complete");
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE id='completed-audit'"
        )
        .await,
        1
    );
    assert!(page(&state.pool, &w, "duplicate-audit").await.is_err());
    held.rollback().await.unwrap();
}

#[tokio::test]
async fn switch_acknowledgement_failure_rolls_back_pointer_and_member_then_native_retry_commits_once()
 {
    let (state, _, _, node) = fixture().await;
    let w = parent(&state).await;
    page(&state.pool, &w, "admission").await.unwrap();
    let w = claim(&state, CHILD, "owner").await;
    let before: Value = sqlx::query_scalar(
        "SELECT jsonb_agg(to_jsonb(p) ORDER BY node_id) FROM mtls_active_probes p",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_probe_switch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-probe-switch' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject switch'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_probe_switch BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_probe_switch();").execute(&state.pool).await.unwrap();
    assert!(switch(&state.pool, &w).await.is_err());
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(
            "SELECT jsonb_agg(to_jsonb(p) ORDER BY node_id) FROM mtls_active_probes p"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap()
    );
    sqlx::raw_sql("DROP TRIGGER reject_probe_switch ON background_work; CREATE SEQUENCE switch_attempts; CREATE FUNCTION retry_probe_switch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-probe-switch' AND NEW.state='complete' AND nextval('switch_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_probe_switch BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_probe_switch();").execute(&state.pool).await.unwrap();
    switch(&state.pool, &w).await.unwrap();
    assert_eq!(active(&state, &node).await, 2);
    assert_eq!(
        count(&state, "SELECT last_value FROM switch_attempts").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotation_nodes WHERE probe_switched"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn completion_audit_or_acknowledgement_failure_preserves_phase_and_native_retry_uses_fixed_audit()
 {
    for failure in [
        "CREATE FUNCTION reject_probe_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='ca.rotation_completed' THEN RAISE EXCEPTION 'reject audit'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_probe_completion BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_probe_completion();",
        "CREATE FUNCTION reject_probe_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-probe-admission' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject acknowledgement'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_probe_completion BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_probe_completion();",
    ] {
        let (state, _, r, _) = fixture().await;
        let w = parent(&state).await;
        page(&state.pool, &w, "admit").await.unwrap();
        let child = claim(&state, CHILD, "child").await;
        switch(&state.pool, &child).await.unwrap();
        let w = parent(&state).await;
        sqlx::raw_sql(failure).execute(&state.pool).await.unwrap();
        assert!(page(&state.pool, &w, "fixed-completion").await.is_err());
        assert_eq!(phase(&state, &r).await, "probes");
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM audit_records WHERE id='fixed-completion'"
            )
            .await,
            0
        );
        sqlx::raw_sql("DROP TRIGGER IF EXISTS reject_probe_completion ON audit_records; DROP TRIGGER IF EXISTS reject_probe_completion ON background_work; CREATE SEQUENCE completion_attempts; CREATE FUNCTION retry_probe_completion() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='ca.rotation_completed' AND nextval('completion_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_probe_completion BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_probe_completion();").execute(&state.pool).await.unwrap();
        page(&state.pool, &w, "fixed-completion").await.unwrap();
        assert_eq!(phase(&state, &r).await, "complete");
        assert_eq!(
            count(&state, "SELECT last_value FROM completion_attempts").await,
            2
        );
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM audit_records WHERE id='fixed-completion'"
            )
            .await,
            1
        );
    }
}

#[tokio::test]
async fn takeover_fences_old_switches_and_the_parent_cannot_finish_a_healthy_child() {
    let (state, ca, r, node) = fixture().await;
    let w = parent(&state).await;
    page(&state.pool, &w, "admit").await.unwrap();
    let old = claim(&state, CHILD, "old").await;
    pass(&state, &r, &ca, 2).await.unwrap();
    assert_eq!(phase(&state, &r).await, "probes");
    assert_eq!(active(&state, &node).await, 1);
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = claim(&state, CHILD, "next").await;
    assert!(switch(&state.pool, &old).await.is_err());
    switch(&state.pool, &next).await.unwrap();
    let w = parent(&state).await;
    page(&state.pool, &w, "complete").await.unwrap();
    assert_eq!(phase(&state, &r).await, "complete");
}

#[tokio::test]
async fn a_busy_node_does_not_block_a_second_real_verified_node() {
    let (state, ca, r, nodes) =
        crate::api::managed_mtls::tests::rotation_flow::probe_switch_fixture().await;
    metadata_ready(&state, &r).await;
    let w = parent(&state).await;
    page(&state.pool, &w, "admit").await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let lease =
        crate::execution::acquire(&mut tx, &format!("node:{}", nodes[0]), "busy", "unrelated")
            .await
            .unwrap()
            .unwrap();
    tx.commit().await.unwrap();
    let w = claim(&state, CHILD, "healthy").await;
    assert_eq!(w.payload["node_id"], nodes[1]);
    switch(&state.pool, &w).await.unwrap();
    assert_eq!(active(&state, &nodes[1]).await, 2);
    assert_eq!(active(&state, &nodes[0]).await, 1);
    let w = parent(&state).await;
    page(&state.pool, &w, "waiting").await.unwrap();
    assert_eq!(phase(&state, &r).await, "probes");
    let mut tx = state.pool.begin().await.unwrap();
    lease.release(&mut tx).await.unwrap();
    tx.commit().await.unwrap();
    let w = claim(&state, CHILD, "resumed").await;
    switch(&state.pool, &w).await.unwrap();
    let w = parent(&state).await;
    page(&state.pool, &w, "done").await.unwrap();
    assert_eq!(phase(&state, &r).await, "complete");
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
    assert!(!ca.is_empty());
}

#[tokio::test]
async fn metadata_work_must_finish_before_probe_admission_can_proceed() {
    let (state, _, r, node) =
        crate::api::managed_mtls::tests::rotation_flow::metadata_fixture(false).await;
    let w = parent(&state).await;
    page(&state.pool, &w, "too-early").await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-probe-switch'"
        )
        .await,
        0
    );
    assert_eq!(phase(&state, &r).await, "probes");
    metadata::pass(&state, &r).await.unwrap();
    let w = parent(&state).await;
    page(&state.pool, &w, "admit").await.unwrap();
    let w = claim(&state, CHILD, "node").await;
    switch(&state.pool, &w).await.unwrap();
    let w = parent(&state).await;
    page(&state.pool, &w, "done").await.unwrap();
    assert_eq!(active(&state, &node).await, 2);
    assert_eq!(phase(&state, &r).await, "complete");
}

#[tokio::test]
async fn expired_discarded_recovering_or_changed_actual_proof_cannot_switch_or_complete() {
    for change in [
        "UPDATE mtls_probe_versions SET expires_at=clock_timestamp()-interval '1 second' WHERE ca_version=2",
        "UPDATE mtls_probe_versions SET discarded=true WHERE ca_version=2",
        "UPDATE mtls_activations SET phase='recovery_required' WHERE phase='committed'",
        "UPDATE nodes SET deployed_content_sha256='changed-digest'",
    ] {
        let (state, _, r, node) = fixture().await;
        let w = parent(&state).await;
        page(&state.pool, &w, "admit").await.unwrap();
        let w = claim(&state, CHILD, "owner").await;
        sqlx::query(change).execute(&state.pool).await.unwrap();
        switch(&state.pool, &w).await.unwrap();
        assert_eq!(active(&state, &node).await, 1);
        assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-rotation-probe-switch' AND state='queued' AND error_code='rotation_probe_waiting_proof' AND available_at>clock_timestamp()+interval '50 seconds'").await,1);
        let w = parent(&state).await;
        page(&state.pool, &w, "not-done").await.unwrap();
        assert_eq!(phase(&state, &r).await, "probes");
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM audit_records WHERE action='ca.rotation_completed'"
            )
            .await,
            0
        );
    }
}

#[tokio::test]
async fn obsolete_sources_cannot_switch_or_finish_the_new_rotation() {
    for change in [
        "UPDATE credentials SET archived=true WHERE kind='ca_identity'",
        "UPDATE mtls_ca_generations SET expires_at=clock_timestamp()-interval '1 second' WHERE ca_version=2",
        "UPDATE mtls_rotations SET phase='complete',outcome='superseded_expired_publication'",
    ] {
        let (state, _, _, node) = fixture().await;
        let w = parent(&state).await;
        page(&state.pool, &w, "admit").await.unwrap();
        let w = claim(&state, CHILD, "owner").await;
        sqlx::query(change).execute(&state.pool).await.unwrap();
        switch(&state.pool, &w).await.unwrap();
        assert_eq!(active(&state, &node).await, 1);
        let w = parent(&state).await;
        page(&state.pool, &w, "not-done").await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM audit_records WHERE action='ca.rotation_completed'"
            )
            .await,
            0
        );
    }
}

#[tokio::test]
async fn deletion_or_an_incorrect_work_scope_cannot_create_a_ghost_probe() {
    let (state, _, _, node) = fixture().await;
    let w = parent(&state).await;
    page(&state.pool, &w, "admit").await.unwrap();
    let w = claim(&state, CHILD, "owner").await;
    let mut wrong = w.clone();
    wrong.payload["node_id"] = json!("another-node");
    assert!(switch(&state.pool, &wrong).await.is_err());
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    switch(&state.pool, &w).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_active_probes").await,
        0
    );
    assert_eq!(count(&state, "SELECT count(*) FROM nodes").await, 0);
}

async fn admission_fixture(n: i32) -> (AppState, String, String) {
    let (state, ca, r) = super::super::trust::tests::fixture(n).await;
    sqlx::query("UPDATE mtls_ca_generations SET state='retired' WHERE ca_version=1")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_ca_generations SET state='published',published_at=clock_timestamp() WHERE ca_version=2").execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE mtls_authorities SET published_version=2")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_rotations SET phase='probes',published_at=clock_timestamp()")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    enqueue_on(&mut tx, &r, &ca, 2).await.unwrap();
    tx.commit().await.unwrap();
    (state, ca, r)
}
#[tokio::test]
async fn three_hundred_one_members_are_admitted_in_bounded_pages_without_remote_work_or_early_completion()
 {
    let (state, _, r) = admission_fixture(301).await;
    for expected in [100, 200, 300, 301] {
        let w = parent(&state).await;
        page(&state.pool, &w, "admit").await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-probe-switch'"
            )
            .await,
            expected
        );
        assert_eq!(phase(&state, &r).await, "probes");
    }
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_rotation_nodes").await,
        0
    );
}

#[tokio::test]
async fn page_confirmation_failure_rolls_back_all_children_and_cursor_then_native_retry_is_unique()
{
    let (state, _, _) = admission_fixture(101).await;
    let w = parent(&state).await;
    sqlx::raw_sql("CREATE FUNCTION reject_probe_page() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-probe-admission' AND NEW.state<>'running' THEN RAISE EXCEPTION 'reject page'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_probe_page BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_probe_page();").execute(&state.pool).await.unwrap();
    assert!(page(&state.pool, &w, "admit").await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-probe-switch'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_probe_page ON background_work; CREATE SEQUENCE probe_page_attempts; CREATE FUNCTION retry_probe_page() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-probe-admission' AND NEW.state<>'running' AND nextval('probe_page_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_probe_page BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_probe_page();").execute(&state.pool).await.unwrap();
    page(&state.pool, &w, "admit").await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-probe-switch'"
        )
        .await,
        100
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM probe_page_attempts").await,
        2
    );
}

#[tokio::test]
async fn admission_takeover_preserves_cursor_and_rejects_old_page_results() {
    let (state, _, _) = admission_fixture(301).await;
    let w = parent(&state).await;
    page(&state.pool, &w, "first").await.unwrap();
    let old = parent(&state).await;
    assert_eq!(old.payload["cursor"], "member-0100");
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = parent(&state).await;
    assert_eq!(old.payload, next.payload);
    assert!(page(&state.pool, &old, "old").await.is_err());
    page(&state.pool, &next, "next").await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-probe-switch'"
        )
        .await,
        200
    );
}

#[tokio::test]
async fn a_late_member_behind_the_cursor_is_admitted_after_wrap_despite_cached_completion() {
    let (state, ca, r) = admission_fixture(301).await;
    for _ in 0..4 {
        let w = parent(&state).await;
        page(&state.pool, &w, "admit").await.unwrap();
    }
    super::super::trust::tests::nodes(&state, &ca, 302).await;
    sqlx::query("DELETE FROM mtls_node_trust WHERE node_id='member-0302'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET id='early-member' WHERE id='member-0302'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,desired,deployed) VALUES('early-member',$1,1,true,true)").bind(&ca).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_rotation_nodes(rotation_id,node_id,expected_revision,trust_prepared,probe_switched) VALUES($1,'early-member',1,true,true)").bind(&r).execute(&state.pool).await.unwrap();
    let w = parent(&state).await;
    page(&state.pool, &w, "wrap").await.unwrap();
    let w = parent(&state).await;
    page(&state.pool, &w, "revisit").await.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-rotation-probe-switch' AND payload->>'node_id'='early-member'").await,1);
    assert_eq!(phase(&state, &r).await, "probes");
}
async fn scanner(state: &AppState) -> Work {
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
async fn publish_all_fixture_generations(state: &AppState) {
    sqlx::query("UPDATE mtls_authorities SET published_version=2")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_rotations SET phase='probes',published_at=clock_timestamp()")
        .execute(&state.pool)
        .await
        .unwrap();
}
#[tokio::test]
async fn rotation_admission_is_bounded_preserves_healthy_owners_and_seeks_empty_future_ranges() {
    let (state, ca, _) = admission_fixture(0).await;
    super::super::trust::tests::many_rotations(&state, &ca, 300).await;
    publish_all_fixture_generations(&state).await;
    let live = parent(&state).await;
    for expected in [101, 201, 301] {
        let scan_work = scanner(&state).await;
        scan(&state.pool, &scan_work).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-probe-admission'"
            )
            .await,
            expected
        );
        for lease in &live.leases {
            assert!(lease.renew(&state.pool).await.unwrap());
        }
    }
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-rotation-probe-admission' AND state='running' AND owner='admission'").await,1);
    let (state, ca, _) = admission_fixture(0).await;
    super::super::trust::tests::many_rotations(&state, &ca, 1000).await;
    publish_all_fixture_generations(&state).await;
    sqlx::query("UPDATE mtls_rotations SET next_action_at=clock_timestamp()+interval '1 day'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("ANALYZE mtls_rotations; ANALYZE mtls_ca_generations; ANALYZE credentials; ANALYZE mtls_authorities; ANALYZE background_work;").execute(&state.pool).await.unwrap();
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar(&format!("EXPLAIN (ANALYZE,FORMAT JSON) {ADMISSION}"))
        .bind(now)
        .bind(PAGE)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(plan.to_string().contains("mtls_rotations_due"), "{plan}");
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
}

#[tokio::test]
async fn direct_switch_pass_uses_an_empty_index_range_for_a_large_future_child_backlog() {
    let (state, _, r) = admission_fixture(0).await;
    sqlx::query("INSERT INTO background_work(id,kind,payload,available_at) SELECT 'future-probe-'||i,$1,jsonb_build_object('rotation_id',$2::text),clock_timestamp()+interval '1 day' FROM generate_series(1,1001)i").bind(CHILD).bind(&r).execute(&state.pool).await.unwrap();
    sqlx::query("ANALYZE background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar(&format!("EXPLAIN (ANALYZE,FORMAT JSON) {CHILD_DUE}"))
        .bind(CHILD)
        .bind(&r)
        .bind(now)
        .bind(PAGE)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(
        plan.to_string()
            .contains("background_work_rotation_identity_due"),
        "{plan}"
    );
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
}

#[tokio::test]
async fn a_real_late_binding_rejects_stale_completion_with_native_ssi_and_keeps_the_barrier_closed()
{
    let (state, ca, r, _) = fixture().await;
    let w = parent(&state).await;
    page(&state.pool, &w, "admit").await.unwrap();
    let child = claim(&state, CHILD, "switch").await;
    switch(&state.pool, &child).await.unwrap();
    let w = parent(&state).await;
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM mtls_rotations WHERE id=$1")
        .bind(&r)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let node = crate::api::managed_mtls::tests::probe_late_binding(&state, &ca).await;
    let error = match page_on(&mut tx, &w, "stale-completion").await {
        Ok(()) => anyhow::Error::from(tx.commit().await.unwrap_err()),
        Err(error) => {
            tx.rollback().await.unwrap();
            error
        }
    };
    assert!(
        crate::db::RetryableTransactionError::retryable_transaction(&error),
        "{error:#}"
    );
    assert_eq!(phase(&state, &r).await, "probes");
    page(&state.pool, &w, "fresh-completion").await.unwrap();
    assert_eq!(phase(&state, &r).await, "probes");
    // Random API UUIDs can sort before the persistent cursor. The first
    // fresh page then wraps; the next page must admit that same late member.
    let next = parent(&state).await;
    page(&state.pool, &next, "after-wrap").await.unwrap();
    assert_eq!(phase(&state, &r).await, "probes");
    let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_work WHERE kind=$1 AND payload->>'node_id'=$2 AND state='queued')").bind(CHILD).bind(node).fetch_one(&state.pool).await.unwrap();
    assert!(pending);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.rotation_completed'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn obsolete_member_cache_is_removed_in_bounded_pages_and_locked_rows_are_revisited_before_completion()
 {
    let (state, _, r) = admission_fixture(301).await;
    sqlx::query("INSERT INTO mtls_rotation_nodes(rotation_id,node_id,expected_revision) SELECT $1,id,desired_revision FROM nodes").bind(&r).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE mtls_node_trust SET desired=false,deployed=false")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT node_id FROM mtls_rotation_nodes WHERE node_id='member-0001' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    for expected in [201, 101, 1, 1] {
        let w = parent(&state).await;
        page(&state.pool, &w, "not-done").await.unwrap();
        assert_eq!(
            count(&state, "SELECT count(*) FROM mtls_rotation_nodes").await,
            expected
        );
        assert_eq!(phase(&state, &r).await, "probes");
    }
    held.rollback().await.unwrap();
    let w = parent(&state).await;
    page(&state.pool, &w, "done").await.unwrap();
    assert_eq!(phase(&state, &r).await, "complete");
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_rotation_nodes").await,
        0
    );
}

#[tokio::test]
async fn an_already_current_probe_only_repairs_stale_member_cache_without_changing_pointer_revision()
 {
    let (state, ca, r, node) = fixture().await;
    let mut tx = state.pool.begin().await.unwrap();
    assert!(switch_probe_on(&mut tx, &node, &ca, 2).await.unwrap());
    tx.commit().await.unwrap();
    let before: Value =
        sqlx::query_scalar("SELECT to_jsonb(p) FROM mtls_active_probes p WHERE node_id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    sqlx::query("UPDATE mtls_rotation_nodes SET trust_prepared=false,probe_switched=false")
        .execute(&state.pool)
        .await
        .unwrap();
    let w = parent(&state).await;
    page(&state.pool, &w, "admit-cache-repair").await.unwrap();
    assert_eq!(phase(&state, &r).await, "probes");
    let w = claim(&state, CHILD, "repair").await;
    switch(&state.pool, &w).await.unwrap();
    let after: Value =
        sqlx::query_scalar("SELECT to_jsonb(p) FROM mtls_active_probes p WHERE node_id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(before, after);
    let w = parent(&state).await;
    page(&state.pool, &w, "complete").await.unwrap();
    assert_eq!(phase(&state, &r).await, "complete");
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotation_nodes WHERE trust_prepared AND probe_switched"
        )
        .await,
        1
    );
}
