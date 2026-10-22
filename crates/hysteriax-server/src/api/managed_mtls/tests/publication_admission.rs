use super::*;

async fn unlock(state: &AppState) {
    sqlx::raw_sql("DO $$ DECLARE r record; BEGIN FOR r IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid WHERE c.relnamespace=current_schema()::regnamespace AND p.proname='hysteriax_serialize_writes' LOOP EXECUTE format('ALTER TABLE %I DISABLE TRIGGER %I',r.relname,r.tgname); END LOOP; END $$").execute(&state.pool).await.unwrap();
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn barrier_timeouts(state: &AppState) {
    let mut connections = Vec::new();
    for _ in 0..4 {
        let mut c = state.pool.acquire().await.unwrap();
        sqlx::query("SET lock_timeout='5s'")
            .execute(&mut *c)
            .await
            .unwrap();
        connections.push(c);
    }
}
async fn blocked_by(state: &AppState, blocker: i32) -> i32 {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let pid: Option<i32> = sqlx::query_scalar(
            "SELECT pid FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)) LIMIT 1",
        )
        .bind(blocker)
        .fetch_optional(&state.pool)
        .await
        .unwrap();
        if let Some(pid) = pid {
            return pid;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected a real PostgreSQL lock wait"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
async fn prepared(state: &AppState, ca: &str, key: &str) -> PreparedAuthorityPublication {
    prepare_authority_publication(state, actor().0, ca.into(), publish_request(1, key))
        .await
        .unwrap()
}

#[tokio::test]
async fn ordinary_publication_does_not_lock_or_rewrite_a_large_bound_node_and_user_graph() {
    let state = state().await;
    let ca = generated(&state).await;
    unlock(&state).await;
    // Every parent is locked to expose any hidden whole-graph lock or fan-out.
    for i in 0..301 {
        node(&state, &format!("member-{i:04}")).await;
    }
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) SELECT 'u-'||i,'User',now(),now() FROM generate_series(1,301)i").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_user_identities(id,user_id,ca_id,created_at,updated_at) SELECT gen_random_uuid()::text,id,$1,now(),now() FROM users").bind(&ca).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,desired,deployed) SELECT id,$1,1,true,true FROM nodes").bind(&ca).execute(&state.pool).await.unwrap();
    let snapshot = "SELECT jsonb_build_object('nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'users',(SELECT jsonb_agg(to_jsonb(u) ORDER BY id) FROM users u),'identities',(SELECT jsonb_agg(to_jsonb(i) ORDER BY id) FROM mtls_user_identities i),'trust',(SELECT jsonb_agg(to_jsonb(t) ORDER BY node_id,ca_id,ca_version) FROM mtls_node_trust t))";
    let before: Value = sqlx::query_scalar(snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes ORDER BY id FOR UPDATE")
        .fetch_all(&mut *held)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM users ORDER BY id FOR UPDATE")
        .fetch_all(&mut *held)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM mtls_user_identities ORDER BY id FOR UPDATE")
        .fetch_all(&mut *held)
        .await
        .unwrap();
    let (_, Json(receipt)) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        publish(
            State(state.clone()),
            actor(),
            Path(ca),
            Json(publish_request(1, "bounded-publication")),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(receipt["published"], false);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_rotation_nodes").await,
        0
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_ca_generations WHERE state='preparing'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
    let after: Value = sqlx::query_scalar(snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(before, after);
    held.rollback().await.unwrap();
}

#[tokio::test]
async fn a_binding_waiting_behind_publication_cannot_commit_the_now_retired_ca_version() {
    let state = state().await;
    let ca = generated(&state).await;
    let input = node_flow::node_request(&state, &ca, 1).await;
    let p = prepared(&state, &ca, "publish-before-binding").await;
    unlock(&state).await;
    barrier_timeouts(&state).await;
    let mut publisher = state.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .execute(&mut *publisher)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *publisher)
        .await
        .unwrap();
    sqlx::query("SELECT ca_id FROM mtls_ca_generations WHERE ca_id=$1 AND ca_version=1 FOR UPDATE")
        .bind(&ca)
        .fetch_one(&mut *publisher)
        .await
        .unwrap();
    let app = state.clone();
    let mut binding =
        tokio::spawn(async move { crate::api::nodes::create(State(app), Json(input)).await });
    tokio::select! {
        _ = blocked_by(&state, pid) => {},
        _ = &mut binding => panic!("binding finished before generation lock wait"),
    }
    let receipt = publish_authority_on(&mut publisher, p).await.unwrap();
    assert_eq!(receipt["published"], true);
    publisher.commit().await.unwrap();
    let error = binding.await.unwrap().unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(count(&state, "SELECT count(*) FROM nodes").await, 0);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_node_trust").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        2
    );
    let input = node_flow::node_request(&state, &ca, 2).await;
    let _ = crate::api::nodes::create(State(state.clone()), Json(input))
        .await
        .unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_node_trust WHERE ca_version=2 AND desired"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn a_publication_with_an_old_snapshot_retries_into_the_committed_binding_trust_barrier() {
    let state = state().await;
    let ca = generated(&state).await;
    let input = node_flow::node_request(&state, &ca, 1).await;
    let p = prepared(&state, &ca, "binding-before-publication").await;
    unlock(&state).await;
    barrier_timeouts(&state).await;
    let mut publisher = state.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .execute(&mut *publisher)
        .await
        .unwrap();
    // Establish the old CA/trust snapshot before the binding begins.
    sqlx::query("SELECT credential_id FROM mtls_authorities WHERE credential_id=$1")
        .bind(&ca)
        .fetch_one(&mut *publisher)
        .await
        .unwrap();
    // Commit an actual binding after the publisher has taken its snapshot.
    // The binding does not lock the CA credential parent: its native foreign
    // keys pin the generation/version instead. SSI must still reject a stale
    // publisher's decision to publish without coordinating this member.
    let _ = crate::api::nodes::create(State(state.clone()), Json(input))
        .await
        .unwrap();
    let conflict = match publish_authority_on(&mut publisher, p.clone()).await {
        Ok(_) => ApiError::from(publisher.commit().await.unwrap_err()),
        Err(error) => {
            publisher.rollback().await.unwrap();
            error
        }
    };
    assert!(
        crate::db::RetryableTransactionError::retryable_transaction(&conflict),
        "{}",
        conflict.message
    );
    let (actor, key, digest) = (
        p.actor.clone(),
        p.input.idempotency_key.clone(),
        p.digest.clone(),
    );
    let receipt = managed_transaction(
        &state.pool,
        &actor,
        &key,
        "ca.publish",
        &digest,
        move |tx| {
            let p = p.clone();
            Box::pin(async move { publish_authority_on(tx, p).await })
        },
    )
    .await
    .unwrap();
    assert_eq!(receipt["published"], false);
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_ca_generations WHERE state='preparing'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_node_trust WHERE ca_version=1 AND desired"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_operations WHERE operation='ca.publish'"
        )
        .await,
        1
    );
}
