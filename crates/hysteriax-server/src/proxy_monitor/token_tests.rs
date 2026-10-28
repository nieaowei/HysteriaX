use super::*;
use crate::{background::Work, execution};

/// Build an actual persisted proxy work and both resource leases for callback
/// fixtures which do not run a sampling executor.
pub(crate) async fn install_fixture(state: &AppState, node: &str, token: &str) -> Work {
    let work = work_fixture(state, node).await;
    install_token(state, &work, &token_digest(token))
        .await
        .unwrap();
    work
}

async fn work_fixture(state: &AppState, node: &str) -> Work {
    let cycle = Uuid::new_v4();
    let id = format!("{cycle}:{node}");
    let mut tx = state.pool.begin().await.unwrap();
    let at: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let revision: Option<i64> =
        sqlx::query_scalar("SELECT deployed_revision FROM nodes WHERE id=$1")
            .bind(node)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    let revision = revision.expect("monitoring fixture requires a deployed node");
    let payload = json!({"captured_at":at,"deployed_revision":revision});
    sqlx::query("INSERT INTO sampling_cycles(id,kind,period_start,deadline_at,expected_nodes) VALUES($1,$2,$3,$3+interval '1 hour',1)")
        .bind(cycle).bind(format!("proxy-fixture-{cycle}")).bind(at).execute(&mut *tx).await.unwrap();
    let mut leases = Vec::new();
    for key in [format!("node:{node}"), format!("work:{id}")] {
        leases.push(
            execution::acquire(&mut tx, &key, &state.instance_id, &id)
                .await
                .unwrap()
                .unwrap(),
        );
    }
    let generation = leases[1].generation;
    sqlx::query("INSERT INTO background_work(id,kind,cycle_id,node_id,payload,state,owner,generation,deadline_at) VALUES($1,'proxy',$2,$3,$4,'running',$5,$6,$7+interval '1 hour')")
        .bind(&id).bind(cycle).bind(node).bind(&payload).bind(&state.instance_id).bind(generation).bind(at).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    Work {
        id,
        kind: "proxy".into(),
        cycle,
        node: node.into(),
        period_start: at,
        expected: 1,
        payload,
        leases,
    }
}

async fn fixture_on(pool: sqlx::PgPool) -> AppState {
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,deployed_revision,created_at,updated_at) SELECT id,id,'127.0.0.1',22,'root','password','unused','example.test',443,':443',CASE WHEN id='a' THEN $1 ELSE id END,'unused','unused','{}',1,clock_timestamp(),clock_timestamp() FROM unnest(ARRAY['a','b'])id")
        .bind(token_digest("node-auth")).execute(&pool).await.unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$")
        .execute(&pool).await.unwrap();
    AppState::new(
        pool,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    )
}
async fn fixture() -> AppState {
    fixture_on(crate::db::test_pool_with_max_connections(8).await).await
}
async fn auth(state: &AppState, token: &str) -> Value {
    serde_json::to_value(
        crate::api::subscriptions::hy2_auth(
            axum::extract::State(state.clone()),
            axum::extract::Path(("a".into(), "node-auth".into())),
            axum::Json(serde_json::from_value(json!({"addr":"","auth":token,"tx":0})).unwrap()),
        )
        .await
        .unwrap()
        .0,
    )
    .unwrap()
}
async fn row(state: &AppState) -> Value {
    sqlx::query_scalar("SELECT coalesce((SELECT to_jsonb(p) FROM monitoring_probe_tokens p WHERE node_id='a'),'null'::jsonb)")
        .fetch_one(&state.pool).await.unwrap()
}

#[tokio::test]
async fn callback_requires_both_live_incarnations_and_exact_current_work_and_node() {
    for invalid in [
        "UPDATE resource_leases SET blocked=true WHERE resource_key='node:a'",
        "UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE resource_key LIKE 'work:%'",
        "UPDATE resource_leases SET generation=generation+1 WHERE resource_key='node:a'",
        "UPDATE resource_leases SET generation=generation+1 WHERE resource_key LIKE 'work:%'",
        "UPDATE resource_leases SET owner='foreign' WHERE resource_key='node:a'",
        "UPDATE background_work SET state='complete',owner=NULL",
        "UPDATE background_work SET generation=generation+1",
        "UPDATE background_work SET kind='network'",
        "UPDATE nodes SET deployed_revision=2 WHERE id='a'",
        "UPDATE nodes SET mtls_state='recovery_required' WHERE id='a'",
        "UPDATE nodes SET state='deleting' WHERE id='a'",
        "UPDATE monitoring_probe_tokens SET expires_at=clock_timestamp()-interval '1 second'",
    ] {
        let state = fixture().await;
        let work = install_fixture(&state, "a", "probe").await;
        assert_eq!(
            auth(&state, "probe").await,
            json!({"ok":true,"id":identity("a",&token_digest("probe"))})
        );
        sqlx::query(invalid).execute(&state.pool).await.unwrap();
        assert_eq!(auth(&state, "probe").await["ok"], false, "{invalid}");
        assert_eq!(row(&state).await["work_id"], work.id);
    }
}

#[tokio::test]
async fn token_is_database_timed_and_independent_of_legacy_and_other_node_locks() {
    let state = fixture().await;
    let legacy = crate::db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    let mut other = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='b' FOR UPDATE")
        .fetch_one(&mut *other)
        .await
        .unwrap();
    let work = tokio::time::timeout(
        Duration::from_secs(2),
        install_fixture(&state, "a", "probe"),
    )
    .await
    .unwrap();
    let valid:bool=sqlx::query_scalar("SELECT expires_at BETWEEN clock_timestamp()+interval '25 seconds' AND clock_timestamp()+interval '31 seconds' FROM monitoring_probe_tokens WHERE node_id='a'")
        .fetch_one(&state.pool).await.unwrap();
    assert!(valid);
    assert_eq!(
        row(&state).await["work_generation"],
        work.leases[1].generation
    );
    let first = auth(&state, "probe").await["id"].clone();
    install_token(&state, &work, &token_digest("next"))
        .await
        .unwrap();
    assert_eq!(auth(&state, "probe").await["ok"], false);
    assert_ne!(first, auth(&state, "next").await["id"]);
    other.rollback().await.unwrap();
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn source_and_scope_mismatch_cannot_issue_or_clear_another_attempts_credential() {
    let state = fixture().await;
    let work = install_fixture(&state, "a", "probe").await;
    let before = row(&state).await;
    let mut invalid = vec![];
    let mut wrong = work.clone();
    wrong.node = "b".into();
    invalid.push(wrong);
    let mut wrong = work.clone();
    wrong.id = "different".into();
    invalid.push(wrong);
    let mut wrong = work.clone();
    wrong.kind = "traffic".into();
    invalid.push(wrong);
    let mut wrong = work.clone();
    wrong.payload["deployed_revision"] = json!(2);
    invalid.push(wrong);
    let mut wrong = work.clone();
    wrong.leases.pop();
    invalid.push(wrong);
    let mut wrong = work.clone();
    wrong.leases[0].owner = "foreign".into();
    invalid.push(wrong);
    for wrong in invalid {
        assert!(install_token(&state, &wrong, "replacement").await.is_err());
        assert!(
            save_result(
                &state,
                &wrong,
                ProbeResult::default(),
                &token_digest("probe")
            )
            .await
            .is_err()
        );
        assert_eq!(row(&state).await, before);
    }
    sqlx::query("UPDATE background_work SET state='failed',owner=NULL WHERE id=$1")
        .bind(work.id.clone())
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(install_token(&state, &work, "replacement").await.is_err());
    assert!(
        save_result(
            &state,
            &work,
            ProbeResult::default(),
            &token_digest("probe")
        )
        .await
        .is_err()
    );
    assert_eq!(row(&state).await, before);
}

#[tokio::test]
async fn takeover_fences_late_token_writes_results_and_cleanup_even_with_the_new_hash() {
    let state = fixture().await;
    let old = install_fixture(&state, "a", "old").await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(auth(&state, "old").await["ok"], false);
    let new = install_fixture(&state, "a", "new").await;
    assert!(new.leases[0].generation > old.leases[0].generation);
    let before = row(&state).await;
    assert!(
        install_token(&state, &old, &token_digest("old"))
            .await
            .is_err()
    );
    assert!(
        save_result(&state, &old, ProbeResult::default(), &token_digest("new"))
            .await
            .is_err()
    );
    assert_eq!(row(&state).await, before);
    assert_eq!(auth(&state, "new").await["ok"], true);
    assert_eq!(auth(&state, "old").await["ok"], false);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxy_probe_samples")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    crate::background::finish(&state.pool, &new, true)
        .await
        .unwrap();
    assert_eq!(auth(&state, "new").await["ok"], false);
}

#[tokio::test]
async fn native_retry_and_failed_cleanup_commit_one_sample_with_its_health_and_credential() {
    let state = fixture().await;
    let work = install_fixture(&state, "a", "old").await;
    sqlx::raw_sql("CREATE SEQUENCE probe_attempt; CREATE FUNCTION probe_insert_retry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('probe_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER probe_insert_retry BEFORE INSERT ON monitoring_probe_tokens FOR EACH ROW EXECUTE FUNCTION probe_insert_retry()")
        .execute(&state.pool).await.unwrap();
    install_token(&state, &work, &token_digest("fresh"))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM probe_attempt")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(auth(&state, "fresh").await["ok"], true);
    let result = ProbeResult {
        status: "ok".into(),
        connection_ms: Some(1.0),
        ..Default::default()
    };
    let before = row(&state).await;
    sqlx::raw_sql("CREATE FUNCTION probe_delete_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject cleanup'; END $$; CREATE TRIGGER probe_delete_failure BEFORE DELETE ON monitoring_probe_tokens FOR EACH ROW EXECUTE FUNCTION probe_delete_failure()")
        .execute(&state.pool).await.unwrap();
    assert!(
        save_result(&state, &work, result.clone(), &token_digest("fresh"))
            .await
            .is_err()
    );
    assert_eq!(row(&state).await, before);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxy_probe_samples")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM monitoring_probe_state")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    sqlx::raw_sql("DROP TRIGGER probe_delete_failure ON monitoring_probe_tokens; CREATE SEQUENCE cleanup_attempt; CREATE FUNCTION probe_delete_retry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('cleanup_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40P01'; END IF; RETURN OLD; END $$; CREATE TRIGGER probe_delete_retry BEFORE DELETE ON monitoring_probe_tokens FOR EACH ROW EXECUTE FUNCTION probe_delete_retry()")
        .execute(&state.pool).await.unwrap();
    save_result(&state, &work, result, &token_digest("fresh"))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM cleanup_attempt")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(row(&state).await, Value::Null);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxy_probe_samples WHERE status='ok'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM monitoring_probe_state WHERE successes=1"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn restored_counter_collision_does_not_authenticate_or_publish_the_old_probe() {
    let state = fixture().await;
    let old = install_fixture(&state, "a", "old").await;
    assert_eq!(auth(&state, "old").await["ok"], true);
    let epoch = Uuid::new_v4().to_string();
    sqlx::query("UPDATE cache_version_epoch SET epoch=$1 WHERE id")
        .bind(&epoch)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(auth(&state, "old").await["ok"], false);
    sqlx::query("UPDATE resource_leases SET epoch=$1 WHERE work_id=$2")
        .bind(&epoch)
        .bind(&old.id)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(auth(&state, "old").await["ok"], false);
    let mut current = old.clone();
    for lease in &mut current.leases {
        lease.epoch = epoch.clone();
    }
    install_token(&state, &current, &token_digest("fresh"))
        .await
        .unwrap();
    assert_eq!(auth(&state, "fresh").await["ok"], true);
    let before = row(&state).await;
    assert!(
        install_token(&state, &old, &token_digest("late"))
            .await
            .is_err()
    );
    assert!(
        save_result(
            &state,
            &old,
            ProbeResult {
                status: "ok".into(),
                ..Default::default()
            },
            &token_digest("fresh")
        )
        .await
        .is_err()
    );
    assert_eq!(before, row(&state).await);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxy_probe_samples")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    save_result(
        &state,
        &current,
        ProbeResult {
            status: "ok".into(),
            ..Default::default()
        },
        &token_digest("fresh"),
    )
    .await
    .unwrap();
    assert_eq!(auth(&state, "fresh").await["ok"], false);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxy_probe_samples")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
}
