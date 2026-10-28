use super::*;
use crate::{
    api::{self, AdminActor},
    security::SecretBox,
};
use axum::{
    Json,
    extract::{Extension, State},
};

async fn fixture(n: i32) -> (AppState, String) {
    fixture_through(n, i64::MAX).await
}
async fn fixture_through(n: i32, version: i64) -> (AppState, String) {
    let pool = db::test_pool_through_version(8, version).await;
    let state = AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    super::super::migration::migrate(&state.pool, &state.secrets)
        .await
        .unwrap();
    let (_, Json(ca)) = api::managed_mtls::create(
        State(state.clone()),
        Extension(AdminActor { id: "test".into() }),
        Json(
            serde_json::from_value(
                json!({"name":"CA","source":"generated","idempotency_key":"probe-maintenance-ca"}),
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    let ca = ca["id"].as_str().unwrap().to_owned();
    let (_,Json(node))=api::nodes::create(State(state.clone()),Json(super::super::test_node_request(&state,json!({"name":"Node","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"unused","public_host":"example.test","public_port":443,"listen_addr":":443"})).await)).await.unwrap();
    let first = node["node"]["id"].as_str().unwrap();
    let ssh: (String, i64) =
        sqlx::query_as("SELECT ssh_credential_id,ssh_credential_version FROM nodes WHERE id=$1")
            .bind(first)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(first)
        .execute(&state.pool)
        .await
        .unwrap();
    // Native constraints and domain triggers remain active; only the old global
    // SQL lock is disabled to exercise independent node/resource ownership.
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    for i in 1..=n {
        let node = format!("00000000-0000-4000-8000-{i:012}");
        sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_credential_id,ssh_credential_version,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,mtls_state,created_at,updated_at) VALUES($1,'Node','127.0.0.1',22,'root',$3,$4,'example.test',443,':443',$1,'unused','unused',$2,'ready',now(),now())")
            .bind(&node).bind(state.secrets.encrypt("{}").unwrap()).bind(&ssh.0).bind(ssh.1).execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,desired,deployed) VALUES($1,$2,1,true,true)").bind(&node).bind(&ca).execute(&state.pool).await.unwrap();
    }
    // Scanning tests use actual native immutable leaf references without paying
    // for hundreds of signatures; renewal tests use a real signed predecessor.
    if n > 2 {
        sqlx::query("INSERT INTO credentials(id,name,kind,created_at,updated_at) SELECT 'probe-'||id,'Probe','tls_identity',now(),now() FROM nodes").execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) SELECT 'probe-'||id,1,'fixture',now() FROM nodes").execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO mtls_probe_versions(node_id,credential_id,credential_version,ca_id,ca_version,expires_at,next_action_at) SELECT id,'probe-'||id,1,$1,1,now()-interval '1 day',now()-interval '1 day' FROM nodes").bind(&ca).execute(&state.pool).await.unwrap();
    } else {
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM nodes ORDER BY id")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        for id in ids {
            super::super::maintenance::renew_probe_on(&state.pool, &state.secrets, &id, &ca, 1)
                .await
                .unwrap();
        }
        sqlx::query("UPDATE mtls_probe_versions SET expires_at=clock_timestamp()-interval '1 day',next_action_at=clock_timestamp()-interval '1 day'").execute(&state.pool).await.unwrap();
    }
    super::super::reference_index::initialize(&state.pool, &state.secrets)
        .await
        .unwrap();
    (state, ca)
}
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn scan(state: &AppState) -> Work {
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1")
        .bind(SCAN_KIND)
        .execute(&state.pool)
        .await
        .unwrap();
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN_KIND, json!({}))
        .await
        .unwrap();
    maintenance::claim(&state.pool, SCAN_KIND, "admit")
        .await
        .unwrap()
        .unwrap()
}
async fn admit(state: &AppState) {
    let work = scan(state).await;
    admit_work(&state.pool, &work).await.unwrap();
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

#[tokio::test]
async fn admission_pages_skip_busy_leaf_rows_and_do_not_reset_existing_owners() {
    let (state, _) = fixture(301).await;
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT node_id FROM mtls_probe_versions ORDER BY node_id LIMIT 1 FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    for expected in [100, 200, 300] {
        admit(&state).await;
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-probe-renewal'"
            )
            .await,
            expected
        );
    }
    let work = claim(&state, "healthy").await;
    assert_eq!(work.leases.len(), 3);
    admit(&state).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE owner='healthy' AND state='running'"
        )
        .await,
        1
    );
    held.rollback().await.unwrap();
    admit(&state).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-probe-renewal'"
        )
        .await,
        301
    );
}

#[tokio::test]
async fn renewal_and_active_probe_commit_with_acknowledgement_and_native_retry_does_not_duplicate_versions()
 {
    let (state, _) = fixture(1).await;
    admit(&state).await;
    let work = claim(&state, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_probe_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-probe-renewal' AND NEW.state='complete' THEN RAISE EXCEPTION 'ack failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_probe_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_probe_ack();").execute(&state.pool).await.unwrap();
    assert!(renew_work(&state, &work).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_probe_versions").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT credential_version FROM mtls_active_probes").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE metadata->>'internal_probe'='true'"
        )
        .await,
        1
    );
    sqlx::raw_sql("DROP TRIGGER reject_probe_ack ON background_work; CREATE SEQUENCE probe_work_attempts; CREATE FUNCTION retry_probe_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-probe-renewal' AND NEW.state='complete' AND nextval('probe_work_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_probe_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_probe_ack();").execute(&state.pool).await.unwrap();
    renew_work(&state, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT last_value FROM probe_work_attempts").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_probe_versions").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT credential_version FROM mtls_active_probes").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner='owner'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
}

#[tokio::test]
async fn node_jobs_block_only_their_probe_and_takeover_fences_prepared_results() {
    let (state, _) = fixture(2).await;
    let node = "00000000-0000-4000-8000-000000000001";
    let lease = db::write(&state.pool, |tx| {
        Box::pin(async move {
            Ok::<_, anyhow::Error>(
                crate::execution::acquire(tx, &format!("node:{node}"), "node-job", "job")
                    .await?
                    .unwrap(),
            )
        })
    })
    .await
    .unwrap();
    admit(&state).await;
    let old = claim(&state, "old").await;
    let target = Target::from_work(&old).unwrap();
    assert_ne!(target.node, node);
    let prepared = probe::prepare(&state.pool, &state.secrets, &target.node, &target.ca, 1)
        .await
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = claim(&state, "next").await;
    assert_eq!(old.id, next.id);
    assert!(
        commit_work(&state.pool, &old, Some(prepared))
            .await
            .is_err()
    );
    renew_work(&state, &next).await.unwrap();
    assert!(lease.renew(&state.pool).await.unwrap());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_probe_versions WHERE credential_version=2"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn no_longer_current_sources_cannot_publish_or_overwrite_newer_errors() {
    let (state, _) = fixture(1).await;
    admit(&state).await;
    let work = claim(&state, "owner").await;
    let target = Target::from_work(&work).unwrap();
    let prepared = probe::prepare(&state.pool, &state.secrets, &target.node, &target.ca, 1)
        .await
        .unwrap();
    // A separately prepared/manual publication supersedes this source version.
    let fresh = probe::prepare(&state.pool, &state.secrets, &target.node, &target.ca, 1)
        .await
        .unwrap();
    probe::commit(&state.pool, fresh).await.unwrap();
    sqlx::query("UPDATE nodes SET mtls_error='newer unrelated failure'")
        .execute(&state.pool)
        .await
        .unwrap();
    commit_work(&state.pool, &work, Some(prepared))
        .await
        .unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_probe_versions").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM nodes WHERE mtls_error='newer unrelated failure'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE error_code='probe_superseded'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn failed_signing_is_sanitized_and_revision_changes_defer_old_errors() {
    let (state, _) = fixture(1).await;
    admit(&state).await;
    let work = claim(&state, "owner").await;
    sqlx::query("UPDATE nodes SET desired_revision=desired_revision+1")
        .execute(&state.pool)
        .await
        .unwrap();
    record_error(&state.pool, &work, 1, "stale error")
        .await
        .unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM nodes WHERE mtls_error IS NOT NULL"
        )
        .await,
        0
    );
    let work = claim(&state, "next").await;
    let bad = AppState::new(
        state.pool.clone(),
        SecretBox::from_base64("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE").unwrap(),
    );
    renew_work(&bad, &work).await.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM nodes WHERE mtls_error='Probe certificate renewal failed: renewal deferred'").await,1);
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_probe_versions WHERE next_action_at>clock_timestamp()+interval '59 minutes'").await,1);
}

#[tokio::test]
async fn admission_ack_failure_rolls_back_all_children_and_wrong_prepared_scope_is_rejected() {
    let (state, _) = fixture(2).await;
    let scan = scan(&state).await;
    sqlx::raw_sql("CREATE FUNCTION reject_probe_admission_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-probe-admission' AND NEW.state='complete' THEN RAISE EXCEPTION 'admission ack failed'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_probe_admission_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_probe_admission_ack();").execute(&state.pool).await.unwrap();
    assert!(admit_work(&state.pool, &scan).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-probe-renewal'"
        )
        .await,
        0
    );
    sqlx::query("DROP TRIGGER reject_probe_admission_ack ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    admit_work(&state.pool, &scan).await.unwrap();
    let work = claim(&state, "owner").await;
    let target = Target::from_work(&work).unwrap();
    let other: String = sqlx::query_scalar("SELECT id FROM nodes WHERE id<>$1")
        .bind(&target.node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let wrong = probe::prepare(&state.pool, &state.secrets, &other, &target.ca, 1)
        .await
        .unwrap();
    assert!(commit_work(&state.pool, &work, Some(wrong)).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_probe_versions").await,
        2
    );
}

#[tokio::test]
async fn prepared_work_is_retired_when_node_ca_or_trust_no_longer_authorizes_renewal() {
    for change in [
        "UPDATE nodes SET state='deleting'",
        "UPDATE nodes SET state='delete_failed'",
        "UPDATE mtls_ca_generations SET state='retired'",
        "UPDATE credentials SET archived=true WHERE kind='ca_identity'",
        "UPDATE mtls_node_trust SET desired=false,deployed=false",
        "UPDATE mtls_probe_versions SET discarded=true",
        "DELETE FROM nodes",
    ] {
        let (state, _) = fixture(1).await;
        admit(&state).await;
        let work = claim(&state, "owner").await;
        let target = Target::from_work(&work).unwrap();
        let prepared = probe::prepare(&state.pool, &state.secrets, &target.node, &target.ca, 1)
            .await
            .unwrap();
        sqlx::query(change).execute(&state.pool).await.unwrap();
        commit_work(&state.pool, &work, Some(prepared))
            .await
            .unwrap();
        assert_eq!(
            count(&state, "SELECT count(*) FROM mtls_probe_versions").await,
            1,
            "{change}"
        );
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM credential_versions WHERE metadata->>'internal_probe'='true'"
            )
            .await,
            1,
            "{change}"
        );
        assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE error_code='probe_superseded' AND state='complete'").await,1,"{change}");
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM resource_leases WHERE owner='owner'"
            )
            .await,
            0,
            "{change}"
        );
        admit(&state).await;
        assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-probe-renewal' AND state='queued'").await,0,"{change}");
    }
}

#[tokio::test]
async fn error_writeback_and_retry_deadline_roll_back_with_failed_confirmation() {
    let (state, _) = fixture(1).await;
    admit(&state).await;
    let work = claim(&state, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_error_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-probe-renewal' AND NEW.state='complete' THEN RAISE EXCEPTION 'error acknowledgement failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_error_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_error_ack();").execute(&state.pool).await.unwrap();
    assert!(
        record_error(&state.pool, &work, 1, "fixed generic failure")
            .await
            .is_err()
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM nodes WHERE mtls_error IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_probe_versions WHERE next_action_at<clock_timestamp()"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner='owner'"
        )
        .await,
        3
    );
    sqlx::query("DROP TRIGGER reject_error_ack ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    record_error(&state.pool, &work, 1, "fixed generic failure")
        .await
        .unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM nodes WHERE mtls_error='fixed generic failure'"
        )
        .await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_probe_versions WHERE next_action_at>clock_timestamp()+interval '59 minutes'").await,1);
}

#[tokio::test]
async fn empty_due_range_uses_the_due_index_with_many_future_probes() {
    let (state, _) = fixture(1001).await;
    sqlx::query("UPDATE mtls_probe_versions SET next_action_at=clock_timestamp()+interval '1 day'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("ANALYZE mtls_probe_versions")
        .execute(&state.pool)
        .await
        .unwrap();
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE,BUFFERS,FORMAT JSON) {DUE}"
    )))
    .bind(now)
    .bind(PAGE)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    fn indexed_empty(plan: &serde_json::Value) -> bool {
        if plan["Index Name"] == "mtls_probe_versions_due"
            && plan["Actual Rows"].as_f64() == Some(0.0)
        {
            return true;
        }
        match plan {
            serde_json::Value::Array(values) => values.iter().any(indexed_empty),
            serde_json::Value::Object(values) => values.values().any(indexed_empty),
            _ => false,
        }
    }
    assert!(indexed_empty(&plan), "{plan}");
}

#[tokio::test]
async fn index_upgrade_and_repeat_migration_preserve_existing_certificates_nodes_and_current_probe_bytes()
 {
    let (state, _) = fixture_through(1, 55).await;
    let snapshot = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n)-'traffic_capture_mode' ORDER BY id) FROM nodes n),'credentials',(SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM credentials c),'material',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v),'probes',(SELECT jsonb_agg(to_jsonb(p) ORDER BY node_id,credential_id,credential_version) FROM mtls_probe_versions p),'active',(SELECT jsonb_agg(to_jsonb(a) ORDER BY node_id) FROM mtls_active_probes a),'trust',(SELECT jsonb_agg(to_jsonb(t) ORDER BY node_id,ca_id,ca_version) FROM mtls_node_trust t))";
    let before: serde_json::Value = sqlx::query_scalar(snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    db::run_migrations(&state.pool).await.unwrap();
    db::run_migrations(&state.pool).await.unwrap();
    let after: serde_json::Value = sqlx::query_scalar(snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(count(&state,"SELECT count(*) FROM pg_indexes WHERE schemaname=current_schema() AND indexname='mtls_probe_current_scope_idx'").await,1);
}
