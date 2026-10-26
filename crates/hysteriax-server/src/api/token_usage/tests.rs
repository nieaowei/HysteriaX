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
    let legacy = db::hold_retired_global_write_lock(&pool).await.unwrap();
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

#[tokio::test]
async fn scheduled_usage_is_bounded_and_outlives_the_callers_deadline() {
    let pool = fixture().await;
    let state = crate::state::AppState::new(
        pool.clone(),
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    // Delay the actual update beyond the old 50ms request deadline.
    sqlx::raw_sql("CREATE FUNCTION slow_touch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.15); RETURN NEW; END $$; CREATE TRIGGER slow_touch BEFORE UPDATE ON admin_tokens FOR EACH ROW EXECUTE FUNCTION slow_touch()")
        .execute(&pool).await.unwrap();
    for _ in 0..100 {
        schedule(&state, "one", &token_digest("one"));
    }
    assert_eq!(state.admin_token_usage_updates.available_permits(), 0);
    let waiting = tokio::time::timeout(
        std::time::Duration::from_millis(50),
        state.admin_token_usage_updates.acquire(),
    )
    .await;
    assert!(
        waiting.is_err(),
        "the caller's deadline must not cancel the update"
    );
    let completed = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        state.admin_token_usage_updates.acquire(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(used(&pool, "one").await.is_some());
    assert!(used(&pool, "two").await.is_none());
    drop(completed);
}

#[tokio::test]
async fn production_latency_cancellation_cleans_pool_and_background_usage_completes() {
    use sqlx::Connection;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let direct = fixture().await;
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&direct)
        .await
        .unwrap();
    let options = (*direct.connect_options()).clone();
    let target = (options.get_host().to_owned(), options.get_port());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let relay = tokio::spawn(async move {
        loop {
            let (downstream, _) = listener.accept().await.unwrap();
            let target = target.clone();
            tokio::spawn(async move {
                let upstream = tokio::net::TcpStream::connect(target).await.unwrap();
                let (mut dr, mut dw) = downstream.into_split();
                let (mut ur, mut uw) = upstream.into_split();
                let send = tokio::io::copy(&mut dr, &mut uw);
                let receive = async {
                    let mut bytes = [0_u8; 65536];
                    loop {
                        let count = ur.read(&mut bytes).await?;
                        if count == 0 {
                            return Ok::<(), std::io::Error>(());
                        }
                        tokio::time::sleep(Duration::from_millis(26)).await;
                        dw.write_all(&bytes[..count]).await?;
                    }
                };
                tokio::select! { _ = send => {}, _ = receive => {} }
            });
        }
    });
    let pool = db::pool_options()
        .max_connections(1)
        .after_connect(move |connection, _| {
            let schema = schema.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(format!("SET search_path TO {schema}")))
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(
            options
                .host("127.0.0.1")
                .port(port)
                .ssl_mode(sqlx::postgres::PgSslMode::Disable),
        )
        .await
        .unwrap();
    // Warm the single connection so its next acquisition includes the same
    // health-check round trip as production.
    let connection = pool.acquire().await.unwrap();
    drop(connection);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            touch(&pool, "one", &token_digest("one"))
        )
        .await
        .is_err()
    );
    let mut connection = pool.acquire().await.unwrap();
    assert!(!connection.is_in_transaction());
    let read_only: String = sqlx::query_scalar("SHOW transaction_read_only")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert_eq!(read_only, "off");
    // A different isolation level after a real query must be a fresh BEGIN,
    // rather than the 25001 -> 25P02 failure seen with the unpatched driver.
    let tx = connection
        .begin_with("BEGIN ISOLATION LEVEL SERIALIZABLE")
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    drop(connection);
    assert!(used(&direct, "one").await.is_none());
    let state = crate::state::AppState::new(
        pool.clone(),
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    for _ in 0..100 {
        schedule(&state, "one", &token_digest("one"));
    }
    let complete = tokio::time::timeout(
        Duration::from_secs(5),
        state.admin_token_usage_updates.acquire(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(used(&direct, "one").await.is_some());
    drop(complete);
    pool.close().await;
    relay.abort();
}
