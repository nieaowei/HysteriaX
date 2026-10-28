use super::*;
async fn fixture() -> AppState {
    let pool = db::test_pool_with_max_connections(4).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    let state = AppState::new(pool, secrets);
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES('user','User',true,now(),now())").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO subscription_credentials(id,user_id,token_hash,token_enc,created_at) VALUES('old','user',$1,$2,now())")
        .bind(token_digest("old-token")).bind(state.secrets.encrypt("old-token").unwrap()).execute(&state.pool).await.unwrap();
    state
}
async fn rotate_once(state: &AppState) -> Result<Json<Value>, ApiError> {
    rotate(
        State(state.clone()),
        Path("user".into()),
        Json(RotateSubscription {
            expected_revision: 1,
        }),
    )
    .await
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
#[tokio::test]
async fn rotation_and_revocation_roll_back_when_the_audit_fails() {
    let state = fixture().await;
    sqlx::raw_sql("CREATE FUNCTION reject_subscription_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='user.subscription_rotated' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_subscription_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_subscription_audit();").execute(&state.pool).await.unwrap();
    assert!(rotate_once(&state).await.is_err());
    assert_eq!(
        count(&state, "SELECT revision FROM users WHERE id='user'").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM subscription_credentials").await,
        1
    );
    assert_eq!(
        validate_subscription(&state, "old-token").await.unwrap(),
        "user"
    );
}
#[tokio::test]
async fn retry_returns_the_committed_token_and_revokes_only_the_old_url() {
    let state = fixture().await;
    sqlx::raw_sql("CREATE SEQUENCE subscription_attempt; CREATE FUNCTION retry_subscription_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='user.subscription_rotated' AND nextval('subscription_attempt')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_subscription_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_subscription_audit();").execute(&state.pool).await.unwrap();
    let Json(response) = rotate_once(&state).await.unwrap();
    let token = response["token"].as_str().unwrap();
    assert_eq!(response["revision"], 2);
    assert!(validate_subscription(&state, "old-token").await.is_err());
    assert_eq!(validate_subscription(&state, token).await.unwrap(), "user");
    let (stored, hash): (String, String) = sqlx::query_as(
        "SELECT token_enc,token_hash FROM subscription_credentials WHERE revoked_at IS NULL",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(state.secrets.decrypt(&stored).unwrap(), token);
    assert_eq!(hash, token_digest(token));
    let Json(read) = get_user(State(state.clone()), Path("user".into()))
        .await
        .unwrap();
    assert_eq!(read["active"]["token"], token);
    assert_eq!(
        count(&state, "SELECT count(*) FROM subscription_credentials").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='user.subscription_rotated'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM subscription_attempt").await,
        2
    );
}
#[tokio::test]
async fn concurrent_rotations_preserve_one_active_token_and_one_revision() {
    let state = fixture().await;
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(rotate_once(&state), rotate_once(&state))
    })
    .await
    .unwrap();
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
        count(
            &state,
            "SELECT count(*) FROM subscription_credentials WHERE revoked_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT revision FROM users WHERE id='user'").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='user.subscription_rotated'"
        )
        .await,
        1
    );
}
