use super::*;
use axum::{Json, extract::Path};

async fn fixture() -> AppState {
    let state = crate::kick_requests::tests::fixture().await;
    crate::connection_credentials::migrate(&state.pool, &state.secrets)
        .await
        .unwrap();
    state
}
async fn queued(state: &AppState, ty: &str, target: &str, action: &str, input: Value) -> Value {
    let mut tx = state.pool.begin().await.unwrap();
    let receipt = admit(&mut tx, ty, target, action, input, "test-admin")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    receipt
}
async fn apply(state: &AppState, ty: &str, target: &str, action: &str, input: Value) -> Value {
    let receipt = queued(state, ty, target, action, input).await;
    finish_for_test(state, receipt["job_id"].as_str().unwrap()).await;
    receipt
}
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn allowed(state: &AppState) -> bool {
    sqlx::query_scalar("SELECT hysteriax_authorized('user','node')")
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
#[tokio::test]
async fn all_changes_are_queued_and_shared_password_is_unchanged_by_grants() {
    let state = fixture().await;
    let before: (String, String) = sqlx::query_as(
        "SELECT connection_credential_hash,connection_credential_enc FROM users WHERE id='user'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let receipt = queued(
        &state,
        "group",
        "group",
        "create",
        json!({"name":"A","add_user_ids":["user"],"add_node_ids":["node"]}),
    )
    .await;
    assert_eq!(receipt["status"], "queued");
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        0
    );
    finish_for_test(&state, receipt["job_id"].as_str().unwrap()).await;
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        1
    );
    assert!(allowed(&state).await);
    let after: (String, String) = sqlx::query_as(
        "SELECT connection_credential_hash,connection_credential_enc FROM users WHERE id='user'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    let receipt = queued(
        &state,
        "group",
        "group",
        "update",
        json!({"expected_revision":1,"name":"B"}),
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT name FROM authorization_groups")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "A"
    );
    finish_for_test(&state, receipt["job_id"].as_str().unwrap()).await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT name FROM authorization_groups")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        "B"
    );
}
#[tokio::test]
async fn exclusions_revoke_immediately_but_preserve_other_sources() {
    let state = fixture().await;
    for id in ["one", "two"] {
        apply(
            &state,
            "group",
            id,
            "create",
            json!({"name":id,"add_user_ids":["user"],"add_node_ids":["node"]}),
        )
        .await;
    }
    let one = queued(
        &state,
        "group",
        "one",
        "update",
        json!({"expected_revision":1,"remove_user_ids":["user"]}),
    )
    .await;
    assert!(allowed(&state).await);
    finish_for_test(&state, one["job_id"].as_str().unwrap()).await;
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        1
    );
    let two = queued(
        &state,
        "group",
        "two",
        "delete",
        json!({"expected_revision":1}),
    )
    .await;
    assert!(!allowed(&state).await);
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        1
    );
    finish_for_test(&state, two["job_id"].as_str().unwrap()).await;
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_groups WHERE id='two'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM kick_requests WHERE state='active'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn membership_replacement_uses_the_same_queue_and_revocation_barrier() {
    let state = fixture().await;
    apply(
        &state,
        "group",
        "one",
        "create",
        json!({"name":"A","add_user_ids":["user"],"add_node_ids":["node"]}),
    )
    .await;
    let revision = count(&state, "SELECT revision FROM users WHERE id='user'").await;
    let receipt = queued(
        &state,
        "user",
        "user",
        "memberships",
        json!({"expected_revision":revision,"group_ids":[]}),
    )
    .await;
    assert!(!allowed(&state).await);
    finish_for_test(&state, receipt["job_id"].as_str().unwrap()).await;
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_group_users").await,
        0
    );
}
#[tokio::test]
async fn http_replays_one_task_and_rejects_overlapping_target_changes() {
    let state = fixture().await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('admin',$1,'Admin',now(),now())")
        .bind(crate::security::token_digest("admin-token")).execute(&state.pool).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v2", listener.local_addr().unwrap());
    let app = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, crate::api::router(app))
            .await
            .unwrap()
    });
    let client = reqwest::Client::new();
    let mut receipt = None;
    for _ in 0..2 {
        let response = client
            .post(format!("{base}/authorization-groups"))
            .bearer_auth("admin-token")
            .header("Idempotency-Key", "create")
            .json(&json!({"name":"HTTP"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let value: Value = response.json().await.unwrap();
        if let Some(previous) = &receipt {
            assert_eq!(previous, &value);
        }
        receipt = Some(value);
    }
    let receipt = receipt.unwrap();
    let id = receipt["resource_id"].as_str().unwrap();
    let response = client
        .patch(format!("{base}/authorization-groups/{id}"))
        .bearer_auth("admin-token")
        .header("Idempotency-Key", "patch")
        .json(&json!({"expected_revision":1,"add_user_ids":["user"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_changes").await,
        1
    );
    let resumed = AppState::new(state.pool.clone(), (*state.secrets).clone());
    finish_for_test(&resumed, receipt["job_id"].as_str().unwrap()).await;
    server.abort();
}
#[tokio::test]
async fn credential_rotation_updates_one_user_password_and_leaves_assignments_intact() {
    let state = fixture().await;
    apply(
        &state,
        "group",
        "one",
        "create",
        json!({"name":"A","add_user_ids":["user"],"add_node_ids":["node"]}),
    )
    .await;
    let revision = count(&state, "SELECT revision FROM users WHERE id='user'").await;
    let Json(result) = crate::api::users::rotate_credentials(
        axum::extract::State(state.clone()),
        Path("user".into()),
        Json(serde_json::from_value(json!({"expected_revision":revision})).unwrap()),
    )
    .await
    .unwrap();
    let hash: String =
        sqlx::query_scalar("SELECT connection_credential_hash FROM users WHERE id='user'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        hash,
        crate::security::token_digest(result["credential"].as_str().unwrap())
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        1
    );
    assert_eq!(result["revocation_job_ids"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn bounded_pages_commit_progress_and_a_linked_retry_resumes_after_failure() {
    let state = fixture().await;
    sqlx::query("INSERT INTO nodes SELECT (jsonb_populate_record(NULL::nodes,to_jsonb(n)||jsonb_build_object('id','page-'||lpad(i::text,4,'0'),'name','Page '||i::text,'node_token_hash','page-hash-'||i::text))).* FROM nodes n CROSS JOIN generate_series(1,260)i WHERE n.id='node'")
        .execute(&state.pool).await.unwrap();
    let nodes: Vec<String> =
        sqlx::query_scalar("SELECT id FROM nodes WHERE id LIKE 'page-%' ORDER BY id")
            .fetch_all(&state.pool)
            .await
            .unwrap();
    let receipt = queued(
        &state,
        "group",
        "paged",
        "create",
        json!({"name":"Paged","add_user_ids":["user"],"add_node_ids":nodes}),
    )
    .await;
    let id = receipt["job_id"].as_str().unwrap();
    sqlx::raw_sql("CREATE SEQUENCE page_attempt; CREATE FUNCTION reject_page() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('page_attempt')=257 THEN RAISE EXCEPTION 'injected page failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_page BEFORE INSERT ON node_assignments FOR EACH ROW EXECUTE FUNCTION reject_page();")
        .execute(&state.pool).await.unwrap();
    assert_eq!(execute_change_for_test(&state, id).await, "failed");
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        256
    );
    assert_eq!(
        count(&state, "SELECT additions FROM authorization_changes").await,
        256
    );
    sqlx::query("DROP TRIGGER reject_page ON node_assignments")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('retry-admin',$1,'Admin',now(),now())")
        .bind(crate::security::token_digest("retry-token")).execute(&state.pool).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v2", listener.local_addr().unwrap());
    let app = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, crate::api::router(app))
            .await
            .unwrap()
    });
    let response = reqwest::Client::new()
        .post(format!("{base}/jobs/{id}/retry"))
        .bearer_auth("retry-token")
        .header("Idempotency-Key", "retry")
        .json(&json!({"expected_revision":1}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let retry: Value = response.json().await.unwrap();
    let resumed = AppState::new(state.pool.clone(), (*state.secrets).clone());
    finish_for_test(&resumed, retry["job_id"].as_str().unwrap()).await;
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        260
    );
    assert_eq!(
        count(&state, "SELECT additions FROM authorization_changes").await,
        260
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_changes WHERE complete"
        )
        .await,
        1
    );
    server.abort();
}

#[tokio::test]
async fn migration_selects_one_existing_password_and_removes_assignment_password_columns() {
    let state = fixture().await;
    let mut tx = state.pool.begin().await.unwrap();
    let original: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let schema = format!("credential_upgrade_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SET LOCAL search_path TO {schema}"
    )))
    .execute(&mut *tx)
    .await
    .unwrap();
    for migration in [
        include_str!("../../../../migrations/0001_baseline.sql"),
        include_str!("../../../../migrations/0002_node_kick_batches.sql"),
        include_str!("../../../../migrations/0003_remove_authorization_previews.sql"),
        include_str!("../../../../migrations/0004_separate_job_cache_roots.sql"),
    ] {
        sqlx::raw_sql(migration).execute(&mut *tx).await.unwrap();
    }
    sqlx::query(
        "INSERT INTO users(id,name,created_at,updated_at) VALUES('user','User',now(),now())",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO nodes SELECT * FROM {original}.nodes"
    )))
    .execute(&mut *tx)
    .await
    .unwrap();
    let cipher = state.secrets.encrypt("preserved-password").unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('user','node',$1,$2,now())")
        .bind(crate::security::token_digest("preserved-password")).bind(&cipher).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../../migrations/0005_async_authorization.sql"
    ))
    .execute(&mut *tx)
    .await
    .unwrap();
    let kept: String =
        sqlx::query_scalar("SELECT connection_credential_enc FROM users WHERE id='user'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(kept, cipher);
    let columns:i64=sqlx::query_scalar("SELECT count(*) FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='node_assignments' AND column_name IN ('credential_hash','credential_enc')").fetch_one(&mut *tx).await.unwrap();
    assert_eq!(columns, 0);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn one_user_delta_in_a_hundred_thousand_member_ten_thousand_node_group_is_incremental() {
    let state = fixture().await;
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) SELECT 'capacity-'||lpad(i::text,6,'0'),'Existing',now(),now() FROM generate_series(1,100000)i")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO nodes SELECT (jsonb_populate_record(NULL::nodes,to_jsonb(n)||jsonb_build_object('id','capacity-node-'||lpad(i::text,5,'0'),'name','Capacity','node_token_hash','capacity-token-'||i::text))).* FROM nodes n CROSS JOIN generate_series(1,10000)i WHERE n.id='node'")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO authorization_groups(id,name,created_at,updated_at) VALUES('capacity','Capacity',now(),now())").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT 'capacity',id,now() FROM users WHERE id LIKE 'capacity-%'")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT 'capacity',id,now() FROM nodes WHERE id LIKE 'capacity-node-%'")
        .execute(&state.pool).await.unwrap();
    let receipt = queued(
        &state,
        "group",
        "capacity",
        "update",
        json!({"expected_revision":1,"add_user_ids":["user"]}),
    )
    .await;
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_change_items").await,
        1
    );
    assert!(
        count(
            &state,
            "SELECT pg_column_size(input)::bigint FROM authorization_changes"
        )
        .await
            < 1024
    );
    finish_for_test(&state, receipt["job_id"].as_str().unwrap()).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM node_assignments WHERE user_id='user'"
        )
        .await,
        10000
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM node_assignments WHERE user_id<>'user'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM users WHERE id LIKE 'capacity-%' AND revision<>1"
        )
        .await,
        0
    );
    let noop = queued(
        &state,
        "group",
        "capacity",
        "update",
        json!({"expected_revision":2,"add_node_ids":["capacity-node-00001"]}),
    )
    .await;
    let items: i64 =
        sqlx::query_scalar("SELECT count(*) FROM authorization_change_items WHERE job_id=$1")
            .bind(noop["job_id"].as_str())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(items, 0);
    finish_for_test(&state, noop["job_id"].as_str().unwrap()).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM users WHERE id LIKE 'capacity-%' AND revision<>1"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn concurrent_group_tasks_for_one_user_preserve_the_union() {
    let state = fixture().await;
    sqlx::query("INSERT INTO nodes SELECT (jsonb_populate_record(NULL::nodes,to_jsonb(n)||jsonb_build_object('id','other','name','Other','node_token_hash','other-token'))).* FROM nodes n WHERE n.id='node'")
        .execute(&state.pool).await.unwrap();
    let first = queued(
        &state,
        "group",
        "first",
        "create",
        json!({"name":"First","add_user_ids":["user"],"add_node_ids":["node"]}),
    )
    .await;
    let second = queued(
        &state,
        "group",
        "second",
        "create",
        json!({"name":"Second","add_user_ids":["user"],"add_node_ids":["other"]}),
    )
    .await;
    tokio::join!(
        Box::pin(finish_for_test(&state, first["job_id"].as_str().unwrap())),
        Box::pin(finish_for_test(&state, second["job_id"].as_str().unwrap()))
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM node_assignments WHERE user_id='user'"
        )
        .await,
        2
    );
    assert_eq!(
        count(&state, "SELECT revision FROM users WHERE id='user'").await,
        3
    );
}
