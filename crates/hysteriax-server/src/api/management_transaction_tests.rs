use super::*;

async fn state() -> AppState {
    AppState::new(
        crate::db::test_pool_with_max_connections(4).await,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    )
}

#[tokio::test]
async fn token_creation_and_audit_roll_back_together() {
    let state = state().await;
    sqlx::raw_sql("CREATE FUNCTION reject_token_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='admin_token.created' THEN RAISE EXCEPTION 'injected audit failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_token_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_token_audit();")
        .execute(&state.pool).await.unwrap();
    assert!(
        create_admin_token(
            State(state.clone()),
            Json(CreateAdminToken {
                label: "Atomic".into()
            })
        )
        .await
        .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM admin_tokens")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_records")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn token_creation_retries_the_transaction_with_one_valid_secret_and_audit() {
    let state = state().await;
    sqlx::raw_sql("CREATE SEQUENCE token_audit_attempt; CREATE FUNCTION retry_token_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='admin_token.created' AND nextval('token_audit_attempt')=1 THEN RAISE EXCEPTION 'injected serialization failure' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_token_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_token_audit();")
        .execute(&state.pool).await.unwrap();
    let (_, Json(response)) = AUDIT_ACTOR_ID
        .scope(
            "test-actor".to_owned(),
            create_admin_token(
                State(state.clone()),
                Json(CreateAdminToken {
                    label: "Retried".into(),
                }),
            ),
        )
        .await
        .unwrap();
    let token = response["token"].as_str().unwrap();
    let row = sqlx::query("SELECT id,token_hash FROM admin_tokens")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("id"), response["id"].as_str().unwrap());
    assert_eq!(row.get::<String, _>("token_hash"), token_digest(token));
    let audits: Vec<(String, String)> = sqlx::query_as(
        "SELECT actor,entity_id FROM audit_records WHERE action='admin_token.created'",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        audits,
        vec![("test-actor".into(), response["id"].as_str().unwrap().into())]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM admin_tokens")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM token_audit_attempt")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn concurrent_cross_revocations_preserve_one_administrator() {
    let state = state().await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('a','hash-a','A',now()),('b','hash-b','B',now())").execute(&state.pool).await.unwrap();
    // Synchronize both revocations before either can commit.
    sqlx::raw_sql("CREATE SEQUENCE cross_revoke_arrival; CREATE FUNCTION pause_token_revoke() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE arrivals bigint; deadline timestamptz:=clock_timestamp()+interval '3 seconds'; BEGIN PERFORM nextval('cross_revoke_arrival'); LOOP SELECT last_value INTO arrivals FROM cross_revoke_arrival; EXIT WHEN arrivals>=2; IF clock_timestamp()>deadline THEN RAISE EXCEPTION 'concurrent revoke barrier timed out'; END IF; PERFORM pg_sleep(0.01); END LOOP; RETURN NEW; END $$; CREATE TRIGGER pause_token_revoke BEFORE UPDATE ON admin_tokens FOR EACH ROW EXECUTE FUNCTION pause_token_revoke();")
        .execute(&state.pool).await.unwrap();
    let (a, b) = tokio::join!(
        revoke_admin_token(
            State(state.clone()),
            Extension(AdminActor { id: "a".into() }),
            Path("b".into())
        ),
        revoke_admin_token(
            State(state.clone()),
            Extension(AdminActor { id: "b".into() }),
            Path("a".into())
        )
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
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM cross_revoke_arrival")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2,
        "both transactions must read two active tokens before either update commits"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM admin_tokens WHERE revoked_at IS NULL")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE action='admin_token.revoked'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        1
    );
}
