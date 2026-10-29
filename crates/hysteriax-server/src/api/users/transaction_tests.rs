use super::*;
use crate::security::token_digest;

async fn fixture() -> (AppState, String) {
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
    let (_, Json(user)) = create(
        State(state.clone()),
        Json(CreateUser {
            name: "User".into(),
            enabled: true,
            expires_at: None,
            quota_bytes: None,
        }),
    )
    .await
    .unwrap();
    (state, user["id"].as_str().unwrap().into())
}
async fn assign(state: &AppState, user: &str) -> String {
    let input=crate::credentials::test_node_request(state,json!({"name":"Node","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"test","public_host":"example.test","public_port":443,"listen_addr":":443"})).await;
    let (_, Json(node)) = crate::api::nodes::create(State(state.clone()), Json(input))
        .await
        .unwrap();
    let node = node["node"]["id"].as_str().unwrap().to_owned();
    sqlx::query("WITH assignment_fixture(user_id,node_id,credential_hash,credential_enc,created_at) AS MATERIALIZED (VALUES($1,$2,'old-hash',$3,now())), password_fixture AS (UPDATE users u SET connection_credential_hash=c.credential_hash,connection_credential_enc=c.credential_enc FROM (SELECT DISTINCT ON(user_id) user_id,credential_hash,credential_enc FROM assignment_fixture ORDER BY user_id) c WHERE u.id=c.user_id), fixture_groups AS (INSERT INTO authorization_groups(id,name,created_at,updated_at) SELECT 'fixture-'||md5(user_id||chr(31)||node_id),'Fixture authorization',now(),now() FROM assignment_fixture ON CONFLICT(id) DO UPDATE SET name=EXCLUDED.name RETURNING id), fixture_members AS (INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT g.id,a.user_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING), fixture_nodes AS (INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT g.id,a.node_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING) INSERT INTO node_assignments(user_id,node_id,created_at) SELECT user_id,node_id,created_at FROM assignment_fixture").bind(user).bind(&node).bind(state.secrets.encrypt("old-secret").unwrap()).execute(&state.pool).await.unwrap();
    node
}
fn edit(name: &str) -> PatchUser {
    PatchUser {
        expected_revision: 1,
        name: Some(name.into()),
        enabled: None,
        expires_at: None,
        quota_bytes: None,
    }
}
async fn reject_audit(state: &AppState, action: &str) {
    assert!(
        action
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'_')
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE FUNCTION reject_user_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='{action}' THEN RAISE EXCEPTION 'injected audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_user_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_user_audit();"))).execute(&state.pool).await.unwrap();
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn failed_creation_audit_does_not_leave_an_unacknowledged_user() {
    let (state, user) = fixture().await;
    reject_audit(&state, "user.created").await;
    assert!(
        create(
            State(state.clone()),
            Json(CreateUser {
                name: "Failed".into(),
                enabled: true,
                expires_at: None,
                quota_bytes: None
            })
        )
        .await
        .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT id FROM users")
            .fetch_all(&state.pool)
            .await
            .unwrap(),
        vec![user]
    );
}
#[tokio::test]
async fn concurrent_user_edits_allow_only_one_revision() {
    let (state, user) = fixture().await;
    assign(&state, &user).await;
    let (a, b) = tokio::join!(
        patch(State(state.clone()), Path(user.clone()), Json(edit("A"))),
        patch(State(state.clone()), Path(user.clone()), Json(edit("B")))
    );
    let results = [a, b];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| r
                .as_ref()
                .err()
                .is_some_and(|e| e.status == StatusCode::CONFLICT))
            .count(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM users WHERE id=$1")
            .bind(user)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='user.updated'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn a_patch_waiting_on_billing_retries_with_current_usage_and_keeps_the_revocation() {
    let (state, user) = fixture().await;
    let node = assign(&state, &user).await;
    let mut billing = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id=$1 FOR NO KEY UPDATE")
        .bind(&node)
        .fetch_one(&mut *billing)
        .await
        .unwrap();
    let mut input = edit("Quota");
    input.quota_bytes = Some(Some(100));
    let target = state.clone();
    let target_user = user.clone();
    let work =
        tokio::spawn(async move { patch(State(target), Path(target_user), Json(input)).await });
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        loop {
            let waiting:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE query LIKE 'SELECT n.id FROM nodes n JOIN node_assignments%' AND wait_event_type='Lock')").fetch_one(&state.pool).await.unwrap();
            if waiting {break;}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    sqlx::query("UPDATE users SET usage_bytes=150 WHERE id=$1")
        .bind(&user)
        .execute(&mut *billing)
        .await
        .unwrap();
    billing.commit().await.unwrap();
    let Json(response) = work.await.unwrap().unwrap();
    assert_eq!(response["revision"], 2);
    let row = sqlx::query("SELECT usage_bytes,access_kick_enqueued_at FROM users WHERE id=$1")
        .bind(&user)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("usage_bytes"), 150);
    assert!(
        row.get::<Option<DateTime<Utc>>, _>("access_kick_enqueued_at")
            .is_some()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM kick_requests WHERE node_id=$1 AND user_id=$2"
        )
        .bind(node)
        .bind(user)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        1
    );
}
#[tokio::test]
async fn audit_failure_rolls_back_credential_rotation_secrets_revision_and_kicks() {
    let (state, user) = fixture().await;
    let node = assign(&state, &user).await;
    reject_audit(&state, "user.credentials_rotated").await;
    assert!(
        rotate_credentials(
            State(state.clone()),
            Path(user.clone()),
            Json(RevisionRequest {
                expected_revision: 1
            })
        )
        .await
        .is_err()
    );
    let row=sqlx::query("SELECT connection_credential_hash AS credential_hash,connection_credential_enc AS credential_enc FROM users WHERE id=$1 AND EXISTS(SELECT 1 FROM node_assignments WHERE user_id=$1 AND node_id=$2)").bind(&user).bind(node).fetch_one(&state.pool).await.unwrap();
    assert_eq!(row.get::<String, _>("credential_hash"), "old-hash");
    assert_eq!(
        state
            .secrets
            .decrypt(&row.get::<String, _>("credential_enc"))
            .unwrap(),
        "old-secret"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM users WHERE id=$1")
            .bind(user)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM kick_requests").await, 0);
}
#[tokio::test]
async fn rotation_retries_without_returning_rolled_back_credentials_or_duplicating_kicks() {
    let (state, user) = fixture().await;
    assign(&state, &user).await;
    sqlx::raw_sql("CREATE SEQUENCE user_rotation_attempt; CREATE FUNCTION retry_rotation_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='user.credentials_rotated' AND nextval('user_rotation_attempt')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_rotation_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_rotation_audit();").execute(&state.pool).await.unwrap();
    let Json(response) = rotate_credentials(
        State(state.clone()),
        Path(user.clone()),
        Json(RevisionRequest {
            expected_revision: 1,
        }),
    )
    .await
    .unwrap();
    let credential = response["credential"].as_str().unwrap();
    let row =
        sqlx::query("SELECT connection_credential_hash AS credential_hash,connection_credential_enc AS credential_enc FROM users WHERE id=$1")
            .bind(&user)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        row.get::<String, _>("credential_hash"),
        token_digest(credential)
    );
    assert_eq!(
        state
            .secrets
            .decrypt(&row.get::<String, _>("credential_enc"))
            .unwrap(),
        credential
    );
    assert_eq!(response["revision"], 2);
    assert_eq!(count(&state, "SELECT count(*) FROM kick_requests").await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='user.credentials_rotated'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM user_rotation_attempt").await,
        2
    );
}
#[tokio::test]
async fn deleting_user_and_archiving_owned_credentials_roll_back_with_failed_audit() {
    let (state, user) = fixture().await;
    assign(&state, &user).await;
    let mut tx = state.pool.begin().await.unwrap();
    let credential = crate::credentials::insert(
        &mut tx,
        &state.secrets,
        "Owned",
        "api_token",
        Some(&user),
        &json!({"token":"retained"}),
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    reject_audit(&state, "user.deleted").await;
    assert!(
        delete(
            State(state.clone()),
            Path(user.clone()),
            Query(super::super::nodes::RevisionQuery {
                expected_revision: Some(1)
            })
        )
        .await
        .is_err()
    );
    assert!(
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users WHERE id=$1)")
            .bind(&user)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    assert!(
        !sqlx::query_scalar::<_, bool>("SELECT archived FROM credentials WHERE id=$1")
            .bind(credential)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM node_assignments WHERE user_id=$1")
            .bind(user)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM kick_requests").await, 0);
}
