use super::*;
use crate::security::SecretBox;

async fn fixture(expired: i64) -> AppState {
    let pool = db::test_pool_with_max_connections(8).await;
    sqlx::raw_sql("INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('ca','CA','ca_identity',now(),now()); INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('ca',1,'ca-preserved',now()); INSERT INTO mtls_authorities(credential_id,source) VALUES('ca','generated'); INSERT INTO mtls_ca_generations(ca_id,ca_version,state,expires_at) VALUES('ca',1,'published',now()+interval '1 year');").execute(&pool).await.unwrap();
    add_leaf(&pool, "leaf", expired).await;
    AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    )
}
async fn add_leaf(pool: &PgPool, id: &str, expired: i64) {
    sqlx::query("INSERT INTO credentials(id,name,kind,latest_version,created_at,updated_at) VALUES($1,$1,'tls_identity',$2,now(),now())")
        .bind(id).bind(expired+1).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) SELECT $1,i,'preserved-encrypted-'||i,jsonb_build_object('version',i),now() FROM generate_series(1,$2)i")
        .bind(id).bind(expired+1).execute(pool).await.unwrap();
    // Include an expired latest version: latest material is always retained.
    sqlx::query("INSERT INTO mtls_probe_versions(node_id,credential_id,credential_version,ca_id,ca_version,expires_at,next_action_at) SELECT 'retained-node',$1,i,'ca',1,clock_timestamp()-interval '1 day',clock_timestamp()-interval '2 days' FROM generate_series(1,$2)i")
        .bind(id).bind(expired+1).execute(pool).await.unwrap();
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn claim(state: &AppState, owner: &str) -> maintenance::Work {
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1")
        .bind(KIND)
        .execute(&state.pool)
        .await
        .unwrap();
    maintenance::ensure(
        &state.pool,
        KEY,
        KIND,
        json!({"after_id":null,"after_version":0}),
    )
    .await
    .unwrap();
    maintenance::claim(&state.pool, KIND, owner)
        .await
        .unwrap()
        .unwrap()
}
async fn page(state: &AppState) -> u64 {
    let work = claim(state, "test-owner").await;
    prune_work(&state.pool, &work).await.unwrap()
}

#[tokio::test]
async fn bounded_pages_preserve_latest_bytes_and_takeover_rejects_old_owner() {
    let state = fixture(301).await;
    let before: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(v) FROM credential_versions v WHERE credential_id='leaf' AND version=302",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(prune(&state).await.unwrap(), 100);
    let paused = claim(&state, "paused").await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&paused.id).execute(&state.pool).await.unwrap();
    let next = maintenance::claim(&state.pool, KIND, "new-owner")
        .await
        .unwrap()
        .unwrap();
    assert!(next.leases[0].generation > paused.leases[0].generation);
    assert!(prune_work(&state.pool, &paused).await.is_err());
    assert_eq!(prune_work(&state.pool, &next).await.unwrap(), 100);
    assert_eq!(page(&state).await, 100);
    assert_eq!(page(&state).await, 1);
    assert_eq!(page(&state).await, 0);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE credential_id='leaf'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_probe_versions WHERE credential_id='leaf'"
        )
        .await,
        1
    );
    let after: serde_json::Value = sqlx::query_scalar(
        "SELECT to_jsonb(v) FROM credential_versions v WHERE credential_id='leaf' AND version=302",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
}

#[tokio::test]
async fn locked_early_versions_do_not_starve_later_versions_and_are_revisited() {
    let state = fixture(301).await;
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT version FROM credential_versions WHERE credential_id='leaf' AND version<=100 FOR UPDATE").fetch_all(&mut *held).await.unwrap();
    assert_eq!(page(&state).await, 100);
    assert_eq!(page(&state).await, 100);
    assert_eq!(page(&state).await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE credential_id='leaf' AND version<=100"
        )
        .await,
        100
    );
    assert_eq!(page(&state).await, 0);
    held.rollback().await.unwrap();
    // Empty tail wraps the cursor; the following page revisits skipped rows.
    assert_eq!(page(&state).await, 0);
    assert_eq!(page(&state).await, 100);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE credential_id='leaf'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn locked_parent_does_not_starve_another_credential_and_healthy_owner_is_not_replaced() {
    let state = fixture(101).await;
    add_leaf(&state.pool, "other", 1).await;
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM credentials WHERE id='leaf' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let own = claim(&state, "live-owner").await;
    assert_eq!(prune(&state).await.unwrap(), 0);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE owner='live-owner' AND state='running'"
        )
        .await,
        1
    );
    assert_eq!(prune_work(&state.pool, &own).await.unwrap(), 0);
    assert_eq!(page(&state).await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE credential_id='other'"
        )
        .await,
        1
    );
    held.rollback().await.unwrap();
    assert_eq!(page(&state).await, 100);
    assert_eq!(page(&state).await, 1);
}

#[tokio::test]
async fn failed_acknowledgement_rolls_back_private_material_metadata_and_cursor_then_native_conflict_retries()
 {
    let state = fixture(2).await;
    let work = claim(&state, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_retention_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='credential-retention' AND NEW.state='complete' THEN RAISE EXCEPTION 'retention acknowledgement failed'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_retention_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_retention_ack();").execute(&state.pool).await.unwrap();
    assert!(prune_work(&state.pool, &work).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE credential_id='leaf'"
        )
        .await,
        3
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_probe_versions WHERE credential_id='leaf'"
        )
        .await,
        3
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE state='running' AND payload->'after_id'='null'").await,1);
    sqlx::raw_sql("DROP TRIGGER reject_retention_ack ON background_work; CREATE SEQUENCE retention_attempts; CREATE FUNCTION retry_retention_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='credential-retention' AND NEW.state='complete' AND nextval('retention_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_retention_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_retention_ack();").execute(&state.pool).await.unwrap();
    assert_eq!(prune_work(&state.pool, &work).await.unwrap(), 2);
    assert_eq!(
        count(&state, "SELECT last_value FROM retention_attempts").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credential_versions WHERE credential_id='leaf'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn grace_future_leaf_history_and_batch_pins_survive_bounded_retention() {
    let state = fixture(6).await;
    sqlx::raw_sql("UPDATE mtls_probe_versions SET expires_at=clock_timestamp()-interval '1 minute' WHERE credential_version=2; UPDATE mtls_probe_versions SET expires_at=clock_timestamp()+interval '1 day' WHERE credential_version=3; INSERT INTO credential_batches(id,credential_id,version,created_at) VALUES('retained-batch','leaf',4,now()); INSERT INTO mtls_user_identities(id,user_id,ca_id,credential_id,created_at,updated_at) VALUES('identity','deleted-user','ca','leaf',now(),now()); INSERT INTO mtls_user_versions(identity_id,credential_id,credential_version,ca_id,ca_version,not_before,expires_at,next_action_at,created_at) VALUES('identity','leaf',5,'ca',1,now()-interval '1 day',now()+interval '1 day',now(),now()),('identity','leaf',6,'ca',1,now()-interval '2 days',now()-interval '1 day',now()-interval '1 day',now());").execute(&state.pool).await.unwrap();
    assert_eq!(page(&state).await, 2);
    let retained: Vec<i64> = sqlx::query_scalar(
        "SELECT version FROM credential_versions WHERE credential_id='leaf' ORDER BY version",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(retained, vec![2, 3, 4, 5, 7]);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_ca_generations").await,
        1
    );
}

#[tokio::test]
async fn concurrent_history_pin_protects_its_version_while_other_material_is_collected() {
    let state = fixture(2).await;
    let mut pin = state.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO credential_batches(id,credential_id,version,created_at) VALUES('concurrent-pin','leaf',1,now())")
        .execute(&mut *pin).await.unwrap();
    // The FK's KEY SHARE protects version 1 before its uncommitted history is
    // visible to the collector. SKIP LOCKED still permits deleting version 2.
    assert_eq!(page(&state).await, 1);
    pin.commit().await.unwrap();
    assert_eq!(page(&state).await, 0);
    let kept: Vec<i64> = sqlx::query_scalar(
        "SELECT version FROM credential_versions WHERE credential_id='leaf' ORDER BY version",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(kept, vec![1, 3]);
    let late=sqlx::query("INSERT INTO credential_batches(id,credential_id,version,created_at) VALUES('too-late','leaf',2,now())")
        .execute(&state.pool).await.unwrap_err();
    assert_eq!(
        late.as_database_error().unwrap().code().as_deref(),
        Some("23503")
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        1
    );
}
