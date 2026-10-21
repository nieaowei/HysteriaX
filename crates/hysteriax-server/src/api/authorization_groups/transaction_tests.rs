use super::*;
async fn fixture() -> AppState {
    let state = super::tests::fixture().await;
    sqlx::raw_sql("DO $$ DECLARE entry record; BEGIN FOR entry IN SELECT c.relname,tr.tgname FROM pg_trigger tr JOIN pg_class c ON c.oid=tr.tgrelid JOIN pg_proc p ON p.oid=tr.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',entry.relname,entry.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
    state
}
async fn preview(state: &AppState) -> Result<Json<Value>, ApiError> {
    preview_create(
        State(state.clone()),
        Json(
            serde_json::from_value(
                json!({"action":"create","name":"Group","user_ids":["user"],"node_ids":["node"]}),
            )
            .unwrap(),
        ),
    )
    .await
}
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
#[tokio::test]
async fn concurrent_previews_are_independent_and_do_not_change_authorization() {
    let state = fixture().await;
    let assignments = count(&state, "SELECT count(*) FROM node_assignments").await;
    let legacy = db::begin_write(&state.pool).await.unwrap();
    let (a, b) = tokio::join!(preview(&state), preview(&state));
    let Json(a) = a.unwrap();
    let Json(b) = b.unwrap();
    legacy.rollback().await.unwrap();
    assert_ne!(a["preview_token"], b["preview_token"]);
    assert_eq!(a["additions"], b["additions"]);
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_group_previews").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_groups").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM node_assignments").await,
        assignments
    );
}
#[tokio::test]
async fn retried_preview_returns_only_the_one_committed_token() {
    let state = fixture().await;
    sqlx::raw_sql("CREATE SEQUENCE preview_attempts; CREATE FUNCTION retry_preview_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('preview_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_preview_insert AFTER INSERT ON authorization_group_previews FOR EACH ROW EXECUTE FUNCTION retry_preview_insert();").execute(&state.pool).await.unwrap();
    let Json(receipt) = preview(&state).await.unwrap();
    let token: String = sqlx::query_scalar("SELECT token FROM authorization_group_previews")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(token, receipt["preview_token"].as_str().unwrap());
    assert_eq!(
        count(&state, "SELECT last_value FROM preview_attempts").await,
        2
    );
}
#[tokio::test]
async fn failed_preview_insert_rolls_back_expired_preview_collection() {
    let state = fixture().await;
    let Json(old) = preview(&state).await.unwrap();
    sqlx::query("UPDATE authorization_group_previews SET expires_at=now()-interval '1 hour'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_preview_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected failure'; END $$; CREATE TRIGGER reject_preview_insert AFTER INSERT ON authorization_group_previews FOR EACH ROW EXECUTE FUNCTION reject_preview_insert();").execute(&state.pool).await.unwrap();
    assert!(preview(&state).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM authorization_group_previews").await,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT token FROM authorization_group_previews")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        old["preview_token"].as_str().unwrap()
    );
}
#[tokio::test]
async fn preview_collection_is_bounded_and_skips_claimed_rows() {
    let state = fixture().await;
    let Json(receipt) = preview(&state).await.unwrap();
    let token = receipt["preview_token"].as_str().unwrap();
    sqlx::query("INSERT INTO authorization_group_previews(token,action,request_json,guard_json,result_json,created_at,expires_at) SELECT 'expired-'||i,action,request_json,guard_json,result_json,created_at,now()-interval '1 hour' FROM authorization_group_previews CROSS JOIN generate_series(1,300) i WHERE token=$1").bind(token).execute(&state.pool).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query(
        "SELECT token FROM authorization_group_previews WHERE token='expired-1' FOR UPDATE",
    )
    .fetch_one(&mut *held)
    .await
    .unwrap();
    let _ = preview(&state).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE expires_at<now()"
        )
        .await,
        44
    );
    held.rollback().await.unwrap();
    let _ = preview(&state).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE expires_at<now()"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn database_expiry_is_shared_by_preparation_and_commit_validation() {
    let state = fixture().await;
    let Json(receipt) = preview(&state).await.unwrap();
    let token = receipt["preview_token"].as_str().unwrap();
    assert!(
        crate::credentials::authorization::prepare_preview(&state.pool, &state.secrets, token)
            .await
            .is_ok()
    );
    assert!(sqlx::query_scalar::<_,bool>("SELECT expires_at-created_at=interval '10 minutes' FROM authorization_group_previews WHERE token=$1").bind(token).fetch_one(&state.pool).await.unwrap());
    sqlx::query("UPDATE authorization_group_previews SET expires_at=clock_timestamp()-interval '1 second' WHERE token=$1").bind(token).execute(&state.pool).await.unwrap();
    assert!(
        crate::credentials::authorization::prepare_preview(&state.pool, &state.secrets, token)
            .await
            .is_err()
    );
    let mut tx = state.pool.begin().await.unwrap();
    assert_eq!(
        load_preview(&mut tx, token).await.unwrap_err().status,
        StatusCode::CONFLICT
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM authorization_group_previews WHERE consumed_at IS NOT NULL"
        )
        .await,
        0
    );
}
