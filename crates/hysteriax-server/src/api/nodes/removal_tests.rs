use super::*;

async fn remove(state: &AppState, revision: Option<i64>) -> Result<StatusCode, ApiError> {
    remove_record(
        State(state.clone()),
        Path("node".into()),
        Query(RevisionQuery {
            expected_revision: revision,
        }),
    )
    .await
}

#[tokio::test]
async fn record_removal_cancels_work_and_keeps_history_without_ssh() {
    let state = crate::kick_requests::tests::fixture().await;
    sqlx::query("UPDATE nodes SET deployed_revision=1,state='unreachable' WHERE id='node'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("WITH assignment_fixture(user_id,node_id,credential_hash,credential_enc,created_at) AS MATERIALIZED (VALUES('user','node','hash','unused',now())), password_fixture AS (INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at) SELECT 'connection:'||user_id,user_id,'connection',credential_hash,credential_enc,now() FROM (SELECT DISTINCT ON(user_id) user_id,credential_hash,credential_enc FROM assignment_fixture ORDER BY user_id) c ON CONFLICT(user_id,kind) WHERE revoked_at IS NULL DO UPDATE SET secret_hash=EXCLUDED.secret_hash,secret_enc=EXCLUDED.secret_enc), fixture_groups AS (INSERT INTO authorization_groups(id,name,created_at,updated_at) SELECT 'fixture-'||md5(user_id||chr(31)||node_id),'Fixture authorization',now(),now() FROM assignment_fixture ON CONFLICT(id) DO UPDATE SET name=EXCLUDED.name RETURNING id), fixture_members AS (INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT g.id,a.user_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING), fixture_nodes AS (INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT g.id,a.node_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING) INSERT INTO node_assignments(user_id,node_id,created_at) SELECT user_id,node_id,created_at FROM assignment_fixture").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,created_at) VALUES('config','node',1,'unused','hash',now())").execute(&state.pool).await.unwrap();
    let id = crate::kick_requests::tests::enqueue(&state, "user_deleted").await;
    sqlx::query("UPDATE jobs SET status='running' WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let queued = enqueue_job_in_tx(&mut tx, "sync", Some("node"), Some(1))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut receiver = state.removed_nodes.subscribe();
    assert_eq!(
        remove(&state, Some(1)).await.unwrap(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(receiver.try_recv().unwrap(), "node");
    for table in [
        "nodes",
        "node_assignments",
        "config_versions",
        "kick_requests",
    ] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM users")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    for job in [&id, &queued] {
        let row = sqlx::query("SELECT status,stage,node_id,node_name FROM jobs WHERE id=$1")
            .bind(job)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>("status"), "cancelled");
        assert_eq!(row.get::<String, _>("stage"), "node_removed");
        assert_eq!(row.get::<Option<String>, _>("node_id"), Some("node".into()));
        assert_eq!(row.get::<String, _>("node_name"), "Node");
    }
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM job_events WHERE event_type='job.cancelled'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(events, 2);
    let detail: Value = sqlx::query_scalar(
        "SELECT detail_json FROM audit_records WHERE action='node.record_removed'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(detail["remote_uninstall"], false);
    assert_eq!(detail["removed_pending_kicks"], 1);
    assert_eq!(detail["removed_assignments"], 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE kind='uninstall'")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    let mut tx = state.pool.begin().await.unwrap();
    crate::kick_requests::schedule(&mut tx, None, true)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn record_removal_rejects_missing_stale_revision_and_running_remote_mutation() {
    let state = crate::kick_requests::tests::fixture().await;
    for revision in [None, Some(0), Some(-1)] {
        assert_eq!(
            remove(&state, revision).await.unwrap_err().status,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        remove(&state, Some(99)).await.unwrap_err().status,
        StatusCode::CONFLICT
    );
    let mut tx = state.pool.begin().await.unwrap();
    let id = enqueue_job_in_tx(&mut tx, "uninstall", Some("node"), None)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    for kind in [
        "deploy",
        "sync",
        "rollback",
        "uninstall",
        "credential-apply",
    ] {
        sqlx::query("UPDATE jobs SET kind=$2,status='running' WHERE id=$1")
            .bind(&id)
            .bind(kind)
            .execute(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            remove(&state, Some(1)).await.unwrap_err().status,
            StatusCode::CONFLICT
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id=$1")
                .bind(&id)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            "running"
        );
    }
    sqlx::query("UPDATE jobs SET status='failed' WHERE id=$1")
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET state='delete_failed',deployed_revision=1")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        remove(&state, Some(1)).await.unwrap(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        remove(&state, Some(1)).await.unwrap_err().status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn record_removal_event_failure_rolls_back_everything_and_sends_no_signal() {
    let state = crate::kick_requests::tests::fixture().await;
    let id = crate::kick_requests::tests::enqueue(&state, "credentials_revoked").await;
    sqlx::raw_sql("CREATE FUNCTION reject_remove_event() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.event_type='job.cancelled' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_remove_event BEFORE INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION reject_remove_event();").execute(&state.pool).await.unwrap();
    let mut receiver = state.removed_nodes.subscribe();
    assert!(remove(&state, Some(1)).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nodes")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT status FROM jobs WHERE id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "queued"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM kick_requests")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn regular_delete_still_queues_uninstall_for_deployed_node() {
    let state = crate::kick_requests::tests::fixture().await;
    sqlx::query("UPDATE nodes SET deployed_revision=1,state='unreachable'")
        .execute(&state.pool)
        .await
        .unwrap();
    let (status, Json(reply)) = delete(
        State(state.clone()),
        Path("node".into()),
        Query(RevisionQuery {
            expected_revision: Some(1),
        }),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT kind FROM jobs WHERE id=$1")
            .bind(reply["job_id"].as_str().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "uninstall"
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM nodes")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "deleting"
    );
}

#[tokio::test]
async fn http_record_removal_requires_auth_and_advertises_capability() {
    let state = crate::kick_requests::tests::fixture().await;
    let token = "record-removal-test-token-with-256-bits-of-entropy";
    sqlx::query(
        "INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('admin',$1,'test',now())",
    )
    .bind(crate::security::token_digest(token))
    .execute(&state.pool)
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = crate::api::router(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    let endpoint = format!("http://{address}/api/v2/nodes/node/record");
    assert_eq!(
        client
            .delete(format!("{endpoint}?expected_revision=1"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let version: Value = client
        .get(format!("http://{address}/api/v2/version"))
        .bearer_auth(token)
        .header("Idempotency-Key", Uuid::new_v4().to_string())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        version["features"]
            .as_array()
            .unwrap()
            .contains(&json!("node_record_removal"))
    );
    assert_eq!(
        client
            .delete(&endpoint)
            .bearer_auth(token)
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        client
            .delete(format!("{endpoint}?expected_revision=99"))
            .bearer_auth(token)
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let response = client
        .delete(format!("{endpoint}?expected_revision=1"))
        .bearer_auth(token)
        .header("Idempotency-Key", Uuid::new_v4().to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(response.bytes().await.unwrap().is_empty());
    assert_eq!(
        client
            .delete(format!("{endpoint}?expected_revision=1"))
            .bearer_auth(token)
            .header("Idempotency-Key", Uuid::new_v4().to_string())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    task.abort();
}
