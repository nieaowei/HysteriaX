use super::*;
use crate::api::{AdminActor, managed_mtls};
use axum::{
    Json,
    extract::{Extension, Path, State},
};

async fn fixture(leaves: i32) -> (AppState, String, String) {
    let (state, ca, rotation) = super::super::trust::tests::fixture(0).await;
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES('user','User',true,now(),now())").execute(&state.pool).await.unwrap();
    let p = identities::prepare_user(&state.pool, &state.secrets, "user", &ca, 2, false)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    identities::commit_user(&mut tx, &state.secrets, p)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    if leaves > 1 {
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) SELECT v.credential_id,i,v.payload_enc,v.metadata,v.created_at FROM credential_versions v JOIN mtls_user_versions m ON m.credential_id=v.credential_id AND m.credential_version=v.version CROSS JOIN generate_series(2,$1)i").bind(leaves).execute(&state.pool).await.unwrap();
        sqlx::query("UPDATE credentials SET latest_version=$1 WHERE owner_user_id='user'")
            .bind(i64::from(leaves))
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO mtls_user_versions(identity_id,credential_id,credential_version,ca_id,ca_version,not_before,expires_at,next_action_at,released,created_at) SELECT v.identity_id,v.credential_id,i,v.ca_id,v.ca_version,v.not_before,v.expires_at,v.next_action_at,false,v.created_at FROM mtls_user_versions v CROSS JOIN generate_series(2,$1)i WHERE v.credential_version=1").bind(leaves).execute(&state.pool).await.unwrap();
    }
    (state, ca, rotation)
}

fn request(rotation: &str) -> managed_mtls::PublishAuthority {
    managed_mtls::PublishAuthority {
        expected_revision: 2,
        certificate: None,
        private_key: None,
        replace_rotation_id: Some(rotation.into()),
        idempotency_key: "replace-target".into(),
    }
}
async fn replace(
    state: &AppState,
    ca: &str,
    rotation: &str,
) -> Result<Value, crate::error::ApiError> {
    managed_mtls::publish(
        State(state.clone()),
        Extension(AdminActor { id: "test".into() }),
        Path(ca.into()),
        Json(request(rotation)),
    )
    .await
    .map(|(_, Json(v))| v)
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(query)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn claim(state: &AppState, owner: &str) -> Work {
    sqlx::query(
        "UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1 AND state='queued'",
    )
    .bind(KIND)
    .execute(&state.pool)
    .await
    .unwrap();
    maintenance::claim(&state.pool, KIND, owner)
        .await
        .unwrap()
        .unwrap()
}
const HISTORY: &str = "SELECT jsonb_build_object('versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v JOIN credentials c ON c.id=v.credential_id WHERE c.owner_user_id='user'),'users',(SELECT jsonb_agg(to_jsonb(u) ORDER BY id) FROM users u),'identities',(SELECT jsonb_agg(to_jsonb(i) ORDER BY id) FROM mtls_user_identities i))";

#[tokio::test]
async fn replacement_retires_and_hides_all_staged_leaves_without_locking_or_rewriting_the_user_graph()
 {
    let (state, ca, rotation) = fixture(301).await;
    let before: Value = sqlx::query_scalar(HISTORY)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut held = state.pool.begin().await.unwrap();
    for query in [
        "SELECT id FROM users FOR UPDATE",
        "SELECT id FROM credentials WHERE owner_user_id='user' FOR UPDATE",
        "SELECT id FROM mtls_user_identities FOR UPDATE",
        "SELECT identity_id FROM mtls_user_versions FOR UPDATE",
        "SELECT credential_id FROM credential_versions WHERE credential_id IN (SELECT id FROM credentials WHERE owner_user_id='user') FOR UPDATE",
    ] {
        sqlx::query(query).fetch_all(&mut *held).await.unwrap();
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        replace(&state, &ca, &rotation),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["version"], 3);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_version_publication WHERE discarded AND NOT released"
        )
        .await,
        301
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE discarded"
        )
        .await,
        0
    );
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(HISTORY)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_trust_cleanup WHERE completed_at IS NULL"
        )
        .await,
        1
    );
    let deadline: DateTime<Utc> =
        sqlx::query_scalar("SELECT next_action_at FROM mtls_trust_cleanup")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let expected: DateTime<Utc> =
        sqlx::query_scalar("SELECT max(expires_at)+interval '5 minutes' FROM mtls_user_versions")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(deadline, expected);
    let w = claim(&state, "busy").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-superseded-identity-flags' AND state='queued'").await,1);
    held.rollback().await.unwrap();
    for expected in [100, 200, 300, 301] {
        let w = claim(&state, "materialize").await;
        page(&state.pool, &w).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_user_versions WHERE discarded"
            )
            .await,
            expected
        );
    }
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-superseded-identity-flags' AND state='complete'").await,1);
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>(HISTORY)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}

#[tokio::test]
async fn busy_leaves_are_revisited_and_published_or_explicitly_released_history_stays_available() {
    let (state, ca, rotation) = fixture(102).await;
    // Explicit historical release flags retain their pre-migration meaning.
    sqlx::query("UPDATE mtls_user_versions SET released=true WHERE credential_version=102")
        .execute(&state.pool)
        .await
        .unwrap();
    replace(&state, &ca, &rotation).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT identity_id FROM mtls_user_versions WHERE credential_version=1 FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let w = claim(&state, "skip").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE discarded"
        )
        .await,
        100
    );
    let w = claim(&state, "still-busy").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-superseded-identity-flags' AND state='complete'").await,0);
    held.rollback().await.unwrap();
    let w = claim(&state, "revisit").await;
    page(&state.pool, &w).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE discarded"
        )
        .await,
        101
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_version_publication WHERE released AND NOT discarded"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn acknowledgement_failure_rolls_back_flags_and_ownership_then_native_retry_commits_one_page()
{
    let (state, ca, rotation) = fixture(301).await;
    replace(&state, &ca, &rotation).await.unwrap();
    let w = claim(&state, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_superseded_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-superseded-identity-flags' AND NEW.state='queued' THEN RAISE EXCEPTION 'reject acknowledgement'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_superseded_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_superseded_ack();").execute(&state.pool).await.unwrap();
    assert!(page(&state.pool, &w).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE discarded"
        )
        .await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-superseded-identity-flags' AND state='running'").await,1);
    sqlx::raw_sql("DROP TRIGGER reject_superseded_ack ON background_work; CREATE SEQUENCE superseded_attempts; CREATE FUNCTION retry_superseded_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-superseded-identity-flags' AND NEW.state='queued' AND nextval('superseded_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_superseded_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_superseded_ack();").execute(&state.pool).await.unwrap();
    page(&state.pool, &w).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE discarded"
        )
        .await,
        100
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM superseded_attempts").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn takeover_rejects_old_owner_and_exact_scope_mismatch_before_any_flag_write() {
    let (state, ca, rotation) = fixture(101).await;
    replace(&state, &ca, &rotation).await.unwrap();
    let old = claim(&state, "old").await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = claim(&state, "next").await;
    assert_eq!(old.payload, next.payload);
    assert!(page(&state.pool, &old).await.is_err());
    let mut wrong = next.clone();
    wrong.payload["rotation_id"] = json!("other");
    assert!(page(&state.pool, &wrong).await.is_err());
    wrong = next.clone();
    wrong.payload["ca_id"] = json!("other");
    assert!(page(&state.pool, &wrong).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE discarded"
        )
        .await,
        0
    );
    page(&state.pool, &next).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE discarded"
        )
        .await,
        100
    );
}

#[tokio::test]
async fn changed_or_published_sources_never_discard_compatible_history() {
    for change in [
        "UPDATE mtls_rotations SET outcome=NULL WHERE ca_version=2",
        "UPDATE mtls_ca_generations SET published_at=clock_timestamp() WHERE ca_version=2",
        "UPDATE mtls_rotations SET phase='trust' WHERE ca_version=2",
    ] {
        let (state, ca, rotation) = fixture(1).await;
        replace(&state, &ca, &rotation).await.unwrap();
        let w = claim(&state, "source").await;
        // The phase fixture must first close the new active rotation's unique slot.
        sqlx::query("UPDATE mtls_rotations SET phase='complete' WHERE ca_version=3")
            .execute(&state.pool)
            .await
            .unwrap();
        sqlx::query(change).execute(&state.pool).await.unwrap();
        page(&state.pool, &w).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_user_versions WHERE discarded"
            )
            .await,
            0
        );
        assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE error_code='superseded_identity_source_changed' AND state='complete'").await,1);
    }
}

#[tokio::test]
async fn replacement_audit_failure_rolls_back_retirement_visibility_outbox_and_receipt() {
    let (state, ca, rotation) = fixture(1).await;
    sqlx::raw_sql("CREATE FUNCTION reject_supersession() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='ca.target_superseded' THEN RAISE EXCEPTION 'reject audit'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_supersession BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_supersession();").execute(&state.pool).await.unwrap();
    assert!(replace(&state, &ca, &rotation).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_version_publication WHERE discarded"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_ca_generations WHERE ca_version=2 AND state='preparing'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-superseded-identity-flags'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_trust_cleanup").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_operations WHERE idempotency_key='replace-target'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_supersession ON audit_records")
        .execute(&state.pool)
        .await
        .unwrap();
    let receipt = replace(&state, &ca, &rotation).await.unwrap();
    assert_eq!(replace(&state, &ca, &rotation).await.unwrap(), receipt);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-superseded-identity-flags'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn retirement_rejects_prepared_old_signatures_and_native_stale_snapshot_commits() {
    let (state, ca, rotation) = fixture(1).await;
    let p = identities::prepare_user(&state.pool, &state.secrets, "user", &ca, 2, true)
        .await
        .unwrap();
    let mut stale = state.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .execute(&mut *stale)
        .await
        .unwrap();
    sqlx::query("SELECT ca_id FROM mtls_ca_generations")
        .fetch_all(&mut *stale)
        .await
        .unwrap();
    replace(&state, &ca, &rotation).await.unwrap();
    let error = identities::commit_user(&mut stale, &state.secrets, p.clone())
        .await
        .unwrap_err();
    assert!(
        error.downcast_ref::<sqlx::Error>().is_some_and(|e| e
            .as_database_error()
            .is_some_and(|e| e.code().as_deref() == Some("40001"))),
        "{error:?}"
    );
    stale.rollback().await.unwrap();
    let mut fresh = state.pool.begin().await.unwrap();
    assert!(
        identities::commit_user(&mut fresh, &state.secrets, p)
            .await
            .is_err()
    );
    fresh.rollback().await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        1
    );
}

#[tokio::test]
async fn archived_or_deleted_identity_owners_do_not_prevent_bounded_compatibility_flags() {
    let (state, ca, rotation) = fixture(101).await;
    replace(&state, &ca, &rotation).await.unwrap();
    sqlx::query("UPDATE mtls_user_identities SET archived=true")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    let legacy = db::begin_write(&state.pool).await.unwrap();
    for expected in [100, 101] {
        let work = claim(&state, "materializer").await;
        tokio::time::timeout(std::time::Duration::from_secs(2), page(&state.pool, &work))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_user_versions WHERE discarded"
            )
            .await,
            expected
        );
    }
    legacy.rollback().await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE archived"
        )
        .await,
        2
    );
    assert_eq!(count(&state, "SELECT count(*) FROM users").await, 0);
}

#[tokio::test]
async fn repeated_admission_preserves_healthy_work_and_empty_pending_range_uses_existing_index() {
    let (state, ca, rotation) = fixture(1001).await;
    replace(&state, &ca, &rotation).await.unwrap();
    let work = claim(&state, "healthy").await;
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(w) FROM background_work w WHERE id=$1")
        .bind(&work.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    enqueue_on(&mut tx, &rotation, &ca, 2).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        before,
        sqlx::query_scalar::<_, Value>("SELECT to_jsonb(w) FROM background_work w WHERE id=$1")
            .bind(&work.id)
            .fetch_one(&state.pool)
            .await
            .unwrap()
    );
    assert!(
        maintenance::claim(&state.pool, KIND, "other-instance")
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE mtls_user_versions SET discarded=true")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("ANALYZE mtls_user_versions")
        .execute(&state.pool)
        .await
        .unwrap();
    let plan:Value=sqlx::query_scalar("EXPLAIN (ANALYZE,FORMAT JSON) SELECT identity_id,credential_version FROM mtls_user_versions WHERE ca_id=$1 AND ca_version=$2 AND NOT released AND NOT discarded ORDER BY identity_id,credential_version LIMIT 100")
        .bind(&ca).bind(2_i64).fetch_one(&state.pool).await.unwrap();
    assert!(
        plan.to_string()
            .contains("mtls_user_versions_release_pending"),
        "{plan}"
    );
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
    page(&state.pool, &work).await.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-superseded-identity-flags' AND state='complete'").await,1);
}

#[tokio::test]
async fn migration_0059_to_0060_preserves_all_history_and_only_changes_logical_staged_visibility() {
    let pool = db::test_pool_through_version(8, 59).await;
    let (state, ca, _) = super::super::trust::tests::fixture_with_pool(pool, 0).await;
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES('user','User',true,now(),now())").execute(&state.pool).await.unwrap();
    for version in [1, 2] {
        let p = identities::prepare_user(&state.pool, &state.secrets, "user", &ca, version, false)
            .await
            .unwrap();
        let mut tx = state.pool.begin().await.unwrap();
        identities::commit_user(&mut tx, &state.secrets, p)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    // Simulate a persisted unpublished retirement before the new migration.
    sqlx::query("UPDATE mtls_ca_generations SET state='retired' WHERE ca_version=2")
        .execute(&state.pool)
        .await
        .unwrap();
    let snapshot = "SELECT jsonb_build_object('history',(";
    let snapshot = format!(
        "{snapshot}{HISTORY}),'leaves',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_version) FROM mtls_user_versions v),'generations',(SELECT jsonb_agg(to_jsonb(g) ORDER BY ca_id,ca_version) FROM mtls_ca_generations g),'rotations',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM mtls_rotations r))"
    );
    let before: Value = sqlx::query_scalar(&snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_version_publication WHERE discarded"
        )
        .await,
        0
    );
    for _ in 0..2 {
        db::run_migrations(&state.pool).await.unwrap();
        assert_eq!(
            before,
            sqlx::query_scalar::<_, Value>(&snapshot)
                .fetch_one(&state.pool)
                .await
                .unwrap()
        );
        assert_eq!(count(&state,"SELECT count(*) FROM mtls_user_version_publication WHERE discarded AND NOT released").await,1);
        assert_eq!(count(&state,"SELECT count(*) FROM mtls_user_version_publication WHERE released AND NOT discarded").await,1);
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_user_versions WHERE discarded"
            )
            .await,
            0
        );
    }
}

#[tokio::test]
async fn an_inflight_signature_pins_its_generation_before_retirement_and_is_included_in_retention()
{
    let (state, ca, rotation) = fixture(1).await;
    let p = identities::prepare_user(&state.pool, &state.secrets, "user", &ca, 2, true)
        .await
        .unwrap();
    let mut signer = state.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .execute(&mut *signer)
        .await
        .unwrap();
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *signer)
        .await
        .unwrap();
    identities::commit_user(&mut signer, &state.secrets, p)
        .await
        .unwrap();
    let app = state.clone();
    let target = ca.clone();
    let old = rotation.clone();
    let mut publisher = tokio::spawn(async move { replace(&app, &target, &old).await });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let waits: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
        )
        .bind(pid)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        if waits {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "replacement must wait for the pinned generation"
        );
        tokio::select! {result=&mut publisher=>panic!("replacement completed before the signature commit: {result:?}"),_=tokio::time::sleep(std::time::Duration::from_millis(10))=>{}}
    }
    signer.commit().await.unwrap();
    publisher.await.unwrap().unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_version_publication WHERE discarded AND NOT released"
        )
        .await,
        2
    );
    let actual: DateTime<Utc> =
        sqlx::query_scalar("SELECT next_action_at FROM mtls_trust_cleanup WHERE ca_version=2")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let expected: DateTime<Utc> = sqlx::query_scalar(
        "SELECT max(expires_at)+interval '5 minutes' FROM mtls_user_versions WHERE ca_version=2",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(actual, expected);
}
