use super::*;

pub(super) async fn fixture() -> (AppState, String) {
    let pool = db::test_pool_with_max_connections(4).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    crate::credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    crate::credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    let state = AppState::new(pool, secrets);
    let request = crate::credentials::test_node_request(&state, json!({"name":"Node","ssh_host":"8.8.8.8","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"test","public_host":"8.8.8.8","public_port":443,"listen_addr":":443"})).await;
    let (_, Json(receipt)) = create(State(state.clone()), Json(request)).await.unwrap();
    let node = receipt["node"]["id"].as_str().unwrap().to_owned();
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    (state, node)
}
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn waiting(state: &AppState) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'SELECT desired_revision,deployed_revision,state,mtls_state,mtls_recovery_configured FROM nodes%' AND wait_event_type='Lock')").fetch_one(&state.pool).await.unwrap() {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
}
pub(super) async fn history(state: &AppState, node: &str) {
    let mut tx = state.pool.begin().await.unwrap();
    for revision in 2..=4 {
        sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,deployed_success,created_at) SELECT $1,node_id,$2,config_enc,content_sha256,TRUE,created_at FROM config_versions WHERE node_id=$3 AND revision=1").bind(Uuid::new_v4().to_string()).bind(revision as i64).bind(node).execute(&mut *tx).await.unwrap();
    }
    sqlx::query("UPDATE config_versions SET deployed_success=TRUE WHERE node_id=$1")
        .bind(node)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET desired_revision=5,deployed_revision=3 WHERE id=$1")
        .bind(node)
        .execute(&mut *tx)
        .await
        .unwrap();
    crate::credentials::reference_index::flush_node_on(&mut tx, &state.secrets, node)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn action_audit_failure_rolls_back_jobs_and_outbox_events() {
    let (state, node) = fixture().await;
    history(&state, &node).await;
    sqlx::raw_sql("CREATE FUNCTION reject_action_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action IN ('node.deploy','node.sync','node.ssh-test','node.rollback') THEN RAISE EXCEPTION 'injected audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_action_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_action_audit();").execute(&state.pool).await.unwrap();
    for kind in ["deploy", "sync", "ssh-test", "rollback"] {
        assert!(action(&state, &node, 5, kind).await.is_err());
        assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
        assert_eq!(count(&state, "SELECT count(*) FROM job_events").await, 0);
    }
}
#[tokio::test]
async fn retried_action_commits_one_job_event_and_actual_actor() {
    let (state, node) = fixture().await;
    sqlx::raw_sql("CREATE SEQUENCE action_attempts; CREATE FUNCTION retry_action_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='node.deploy' AND nextval('action_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_action_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_action_audit();").execute(&state.pool).await.unwrap();
    // A converted entry point must not wait on the legacy application lock.
    let legacy = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    let (_, Json(receipt)) = super::super::AUDIT_ACTOR_ID
        .scope("node-operator".into(), action(&state, &node, 1, "deploy"))
        .await
        .unwrap();
    legacy.rollback().await.unwrap();
    let (job, revision): (String, i64) = sqlx::query_as("SELECT id,target_revision FROM jobs")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(job, receipt["job_id"].as_str().unwrap());
    assert_eq!(revision, 1);
    assert_eq!(count(&state, "SELECT count(*) FROM job_events").await, 1);
    assert_eq!(count(&state,"SELECT count(*) FROM audit_records WHERE action='node.deploy' AND actor='node-operator'").await,1);
    assert_eq!(
        count(&state, "SELECT last_value FROM action_attempts").await,
        2
    );
}
#[tokio::test]
async fn action_waiting_for_revision_change_rejects_the_old_revision() {
    let (state, node) = fixture().await;
    let mut editing = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE nodes SET desired_revision=2 WHERE id=$1")
        .bind(&node)
        .execute(&mut *editing)
        .await
        .unwrap();
    let (app, id) = (state.clone(), node.clone());
    let request = tokio::spawn(async move { action(&app, &id, 1, "deploy").await });
    waiting(&state).await;
    editing.commit().await.unwrap();
    assert_eq!(
        request.await.unwrap().unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}
#[tokio::test]
async fn action_waiting_for_deletion_does_not_queue_remote_work() {
    let (state, node) = fixture().await;
    let mut deleting = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE nodes SET state='deleting' WHERE id=$1")
        .bind(&node)
        .execute(&mut *deleting)
        .await
        .unwrap();
    let (app, id) = (state.clone(), node.clone());
    let request = tokio::spawn(async move { action(&app, &id, 1, "sync").await });
    waiting(&state).await;
    deleting.commit().await.unwrap();
    assert_eq!(
        request.await.unwrap().unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}
#[tokio::test]
async fn rollback_selects_history_from_the_deployment_seen_after_waiting() {
    let (state, node) = fixture().await;
    history(&state, &node).await;
    let mut deploying = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE nodes SET deployed_revision=4 WHERE id=$1")
        .bind(&node)
        .execute(&mut *deploying)
        .await
        .unwrap();
    let (app, id) = (state.clone(), node.clone());
    let request = tokio::spawn(async move {
        rollback(
            State(app),
            Path(id),
            Json(NodeAction {
                expected_revision: 5,
            }),
        )
        .await
    });
    waiting(&state).await;
    deploying.commit().await.unwrap();
    let (_, Json(receipt)) = request.await.unwrap().unwrap();
    let target: i64 = sqlx::query_scalar("SELECT target_revision FROM jobs WHERE id=$1")
        .bind(receipt["job_id"].as_str().unwrap())
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(target, 3);
}

async fn delete_node(state: &AppState, node: &str) -> Result<(StatusCode, Json<Value>), ApiError> {
    delete(
        State(state.clone()),
        Path(node.into()),
        Query(RevisionQuery {
            expected_revision: Some(1),
        }),
    )
    .await
}
async fn forget_node(state: &AppState, node: &str) -> Result<StatusCode, ApiError> {
    remove_record(
        State(state.clone()),
        Path(node.into()),
        Query(RevisionQuery {
            expected_revision: Some(1),
        }),
    )
    .await
}
async fn waiting_removal(state: &AppState) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'SELECT id FROM nodes WHERE id=$1 FOR UPDATE%' AND wait_event_type='Lock')").fetch_one(&state.pool).await.unwrap() {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
}
#[tokio::test]
async fn deletion_and_forgetting_audit_failures_preserve_the_node_and_send_no_notification() {
    let (state, node) = fixture().await;
    let mut notifications = state.removed_nodes.subscribe();
    sqlx::raw_sql("CREATE FUNCTION reject_removal_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action IN ('node.deleted','node.record_removed','node.uninstall_requested') THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_removal_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_removal_audit();").execute(&state.pool).await.unwrap();
    assert!(delete_node(&state, &node).await.is_err());
    assert!(forget_node(&state, &node).await.is_err());
    assert_eq!(count(&state, "SELECT count(*) FROM nodes").await, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        1
    );
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(delete_node(&state, &node).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM nodes WHERE state='new'").await,
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    assert!(matches!(
        notifications.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}
#[tokio::test]
async fn retried_deletion_notifies_only_after_the_one_committed_removal() {
    let (state, node) = fixture().await;
    let mut notifications = state.removed_nodes.subscribe();
    sqlx::raw_sql("CREATE SEQUENCE deletion_attempts; CREATE FUNCTION retry_deletion_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='node.deleted' AND nextval('deletion_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_deletion_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_deletion_audit();").execute(&state.pool).await.unwrap();
    let legacy = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        delete_node(&state, &node).await.unwrap().0,
        StatusCode::NO_CONTENT
    );
    legacy.rollback().await.unwrap();
    assert_eq!(notifications.try_recv().unwrap(), node);
    assert!(matches!(
        notifications.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(count(&state, "SELECT count(*) FROM nodes").await, 0);
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_reference_edges").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='node.deleted'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM deletion_attempts").await,
        2
    );
}
#[tokio::test]
async fn deletion_waiting_for_a_deployment_preserves_the_node_and_queues_uninstall() {
    let (state, node) = fixture().await;
    let mut deploying = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&mut *deploying)
        .await
        .unwrap();
    let (app, id) = (state.clone(), node.clone());
    let request = tokio::spawn(async move { delete_node(&app, &id).await });
    waiting_removal(&state).await;
    deploying.commit().await.unwrap();
    assert_eq!(request.await.unwrap().unwrap().0, StatusCode::ACCEPTED);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM nodes WHERE state='deleting' AND deployed_revision=1"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='uninstall'").await,
        1
    );
}
#[tokio::test]
async fn concurrent_deletion_requests_share_the_uninstall_job() {
    let (state, node) = fixture().await;
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let (first, second) = tokio::join!(delete_node(&state, &node), delete_node(&state, &node));
    let (_, Json(first)) = first.unwrap();
    let (_, Json(second)) = second.unwrap();
    assert_eq!(first, second);
    assert_eq!(
        count(&state, "SELECT count(*) FROM jobs WHERE kind='uninstall'").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='node.uninstall_requested'"
        )
        .await,
        1
    );
}

async fn edit(state: &AppState, node: &str, value: Value) -> Result<Json<Value>, ApiError> {
    patch(
        State(state.clone()),
        Path(node.into()),
        Json(serde_json::from_value(value).unwrap()),
    )
    .await
}
#[tokio::test]
async fn editing_audit_failure_rolls_back_revision_history_jobs_and_references() {
    let (state, node) = fixture().await;
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(n) FROM nodes n")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let edges:Value=sqlx::query_scalar("SELECT coalesce(jsonb_agg(to_jsonb(e) ORDER BY kind,source_id,source,field),'[]'::jsonb) FROM credential_reference_edges e").fetch_one(&state.pool).await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_edit_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='node.updated' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_edit_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_edit_audit();").execute(&state.pool).await.unwrap();
    assert!(
        edit(
            &state,
            &node,
            json!({"expected_revision":1,"name":"Changed","traffic_stats_port":12000})
        )
        .await
        .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, Value>("SELECT to_jsonb(n) FROM nodes n")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        before
    );
    assert_eq!(sqlx::query_scalar::<_,Value>("SELECT coalesce(jsonb_agg(to_jsonb(e) ORDER BY kind,source_id,source,field),'[]'::jsonb) FROM credential_reference_edges e").fetch_one(&state.pool).await.unwrap(),edges);
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}
#[tokio::test]
async fn concurrent_edits_of_one_revision_commit_one_history_and_sync() {
    let (state, node) = fixture().await;
    let legacy = db::hold_retired_global_write_lock(&state.pool)
        .await
        .unwrap();
    let input = json!({"expected_revision":1,"traffic_stats_port":12000});
    let (a, b) = tokio::join!(
        edit(&state, &node, input.clone()),
        edit(&state, &node, input)
    );
    legacy.rollback().await.unwrap();
    assert_eq!([&a, &b].iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        [&a, &b]
            .iter()
            .filter(|result| result
                .as_ref()
                .err()
                .is_some_and(|error| error.status == StatusCode::CONFLICT))
            .count(),
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM jobs WHERE kind='sync' AND target_revision=2"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='node.updated'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn retried_edit_returns_the_committed_revision_and_snapshot() {
    let (state, node) = fixture().await;
    sqlx::raw_sql("CREATE SEQUENCE edit_attempts; CREATE FUNCTION retry_edit_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='node.updated' AND nextval('edit_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_edit_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_edit_audit();").execute(&state.pool).await.unwrap();
    let Json(receipt) = super::super::AUDIT_ACTOR_ID
        .scope(
            "node-editor".into(),
            edit(
                &state,
                &node,
                json!({"expected_revision":1,"traffic_stats_port":12000}),
            ),
        )
        .await
        .unwrap();
    assert_eq!(receipt["revision"], 2);
    let cipher: String = sqlx::query_scalar(
        "SELECT config_enc FROM config_versions WHERE node_id=$1 AND revision=2",
    )
    .bind(&node)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let snapshot: Value = serde_json::from_str(&state.secrets.decrypt(&cipher).unwrap()).unwrap();
    assert_eq!(snapshot["traffic_stats_port"], 12000);
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        2
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='node.updated' AND actor='node-editor'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM edit_attempts").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_reference_dirty").await,
        0
    );
}
#[tokio::test]
async fn edit_waiting_for_deletion_rechecks_state_before_writing() {
    let (state, node) = fixture().await;
    let mut deleting = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE nodes SET state='deleting' WHERE id=$1")
        .bind(&node)
        .execute(&mut *deleting)
        .await
        .unwrap();
    let (app, id) = (state.clone(), node.clone());
    let request = tokio::spawn(async move {
        edit(&app, &id, json!({"expected_revision":1,"name":"Changed"})).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            if sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'SELECT * FROM nodes WHERE id=$1 FOR NO KEY UPDATE%' AND wait_event_type='Lock')").fetch_one(&state.pool).await.unwrap() {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    deleting.commit().await.unwrap();
    assert_eq!(
        request.await.unwrap().unwrap_err().status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM config_versions").await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM nodes WHERE desired_revision=1 AND name='Node' AND state='deleting'").await,1);
}
