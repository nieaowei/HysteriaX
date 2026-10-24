use super::*;
use crate::security::token_digest;
use chrono::{DateTime, Utc};
async fn fixture() -> PgPool {
    let pool = db::test_pool_with_max_connections(8).await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('one',$1,'one',clock_timestamp()),('two',$2,'two',clock_timestamp())")
        .bind(token_digest("one")).bind(token_digest("two")).execute(&pool).await.unwrap();
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&pool).await.unwrap();
    pool
}
async fn used(pool: &PgPool, id: &str) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("SELECT last_used_at FROM admin_tokens WHERE id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn token_usage_is_independent_of_the_legacy_lock_and_throttles_with_database_time() {
    let pool = fixture().await;
    let legacy = db::begin_write(&pool).await.unwrap();
    touch(&pool, "one", &token_digest("one")).await.unwrap();
    let first = used(&pool, "one").await.unwrap();
    touch(&pool, "one", &token_digest("one")).await.unwrap();
    assert_eq!(used(&pool, "one").await, Some(first));
    sqlx::query("UPDATE admin_tokens SET last_used_at=clock_timestamp()-interval '61 seconds' WHERE id='one'").execute(&pool).await.unwrap();
    touch(&pool, "one", &token_digest("one")).await.unwrap();
    assert!(used(&pool, "one").await.unwrap() > first);
    legacy.rollback().await.unwrap();
}

#[tokio::test]
async fn locked_token_is_skipped_while_another_token_can_be_touched() {
    let pool = fixture().await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM admin_tokens WHERE id='one' FOR UPDATE")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    touch(&pool, "one", &token_digest("one")).await.unwrap();
    assert!(used(&pool, "one").await.is_none());
    touch(&pool, "two", &token_digest("two")).await.unwrap();
    assert!(used(&pool, "two").await.is_some());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn revoked_replaced_or_deleted_token_is_never_touched_or_recreated() {
    for query in [
        "UPDATE admin_tokens SET revoked_at=clock_timestamp() WHERE id='one'",
        "UPDATE admin_tokens SET token_hash='replacement' WHERE id='one'",
        "DELETE FROM admin_tokens WHERE id='one'",
    ] {
        let pool = fixture().await;
        sqlx::query(query).execute(&pool).await.unwrap();
        touch(&pool, "one", &token_digest("one")).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM admin_tokens WHERE id='one' AND last_used_at IS NOT NULL"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn permanent_failure_rolls_back_and_native_conflict_retries_only_the_metadata_transaction() {
    let pool = fixture().await;
    sqlx::raw_sql("CREATE FUNCTION reject_touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'reject'; END $$; CREATE TRIGGER reject_touch BEFORE UPDATE ON admin_tokens FOR EACH ROW EXECUTE FUNCTION reject_touch()")
        .execute(&pool).await.unwrap();
    assert!(touch(&pool, "one", &token_digest("one")).await.is_err());
    assert!(used(&pool, "one").await.is_none());
    sqlx::raw_sql("DROP TRIGGER reject_touch ON admin_tokens; CREATE SEQUENCE touch_attempt; CREATE FUNCTION retry_touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('touch_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_touch BEFORE UPDATE ON admin_tokens FOR EACH ROW EXECUTE FUNCTION retry_touch()")
        .execute(&pool).await.unwrap();
    touch(&pool, "one", &token_digest("one")).await.unwrap();
    assert!(used(&pool, "one").await.is_some());
    assert!(used(&pool, "two").await.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM touch_attempt")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn actual_authentication_survives_locked_or_failed_metadata_and_observes_revocation() {
    let pool = fixture().await;
    let state = crate::state::AppState::new(
        pool.clone(),
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, super::super::router(state))
            .await
            .unwrap()
    });
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
        .unwrap();
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM admin_tokens WHERE id='one' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/api/v2/version"))
            .bearer_auth("one")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert!(used(&pool, "one").await.is_none());
    held.rollback().await.unwrap();
    sqlx::query("UPDATE admin_tokens SET revoked_at=clock_timestamp() WHERE id='one'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/api/v2/version"))
            .bearer_auth("one")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    sqlx::raw_sql("CREATE FUNCTION fail_usage() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'metadata rejected'; END $$; CREATE TRIGGER fail_usage BEFORE UPDATE ON admin_tokens FOR EACH ROW EXECUTE FUNCTION fail_usage()")
        .execute(&pool).await.unwrap();
    assert_eq!(
        client
            .get(format!("{base}/api/v2/version"))
            .bearer_auth("two")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert!(used(&pool, "two").await.is_none());
    server.abort();
}
