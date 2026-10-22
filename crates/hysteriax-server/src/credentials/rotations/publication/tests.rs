use super::*;
use crate::credentials::{
    ca::{Authority, Policy},
    identities,
};

async fn fixture(n: i32) -> (AppState, String, String) {
    let (state, ca, rotation) = super::super::trust::tests::fixture(0).await;
    users(&state, &ca, n).await;
    (state, ca, rotation)
}
async fn users(state: &AppState, ca: &str, n: i32) {
    sqlx::query("UPDATE mtls_rotations SET phase='identities'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) SELECT 'user-'||lpad(i::text,4,'0'),'User',false,now(),now() FROM generate_series(1,$1)i").bind(n).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_user_identities(id,user_id,ca_id,created_at,updated_at) SELECT gen_random_uuid()::text,id,$1,now(),now() FROM users").bind(ca).execute(&state.pool).await.unwrap();
}
async fn stage(state: &AppState, ca: &str, user: &str, version: i64) {
    sqlx::query("UPDATE users SET enabled=true WHERE id=$1")
        .bind(user)
        .execute(&state.pool)
        .await
        .unwrap();
    let p = identities::prepare_user(&state.pool, &state.secrets, user, ca, version, false)
        .await
        .unwrap();
    let secrets = state.secrets.clone();
    db::serializable::<_, anyhow::Error, _>(&state.pool, move |tx| {
        let (p, secrets) = (p.clone(), secrets.clone());
        Box::pin(async move { identities::commit_user(tx, &secrets, p).await })
    })
    .await
    .unwrap();
}
async fn publish(state: &AppState, ca: &str, rotation: &str) {
    super::super::publish_ready(state, rotation, ca, 2)
        .await
        .unwrap();
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

#[tokio::test]
async fn publication_releases_staged_material_atomically_without_locking_or_rewriting_all_users() {
    let (state, ca, rotation) = fixture(301).await;
    stage(&state, &ca, "user-0001", 2).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_version_publication WHERE released"
        )
        .await,
        0
    );
    let leaf_snapshot = "SELECT jsonb_agg(to_jsonb(v) ORDER BY identity_id,credential_version) FROM mtls_user_versions v";
    let before: Value = sqlx::query_scalar(leaf_snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let version_snapshot =
        "SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v";
    let bytes: Value = sqlx::query_scalar(version_snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users ORDER BY id FOR UPDATE")
        .fetch_all(&mut *held)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM mtls_user_identities ORDER BY id FOR UPDATE")
        .fetch_all(&mut *held)
        .await
        .unwrap();
    sqlx::query("SELECT identity_id FROM mtls_user_versions FOR UPDATE")
        .fetch_all(&mut *held)
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::super::publish_ready(&state, &rotation, &ca, 2),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_version_publication WHERE released AND NOT discarded"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE released"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version IS NOT NULL"
        )
        .await,
        0
    );
    assert!(
        before
            == sqlx::query_scalar::<_, Value>(leaf_snapshot)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
        "publication must not rewrite any leaf row"
    );
    assert!(
        bytes
            == sqlx::query_scalar::<_, Value>(version_snapshot)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
        "publication must preserve immutable encrypted versions"
    );
    held.rollback().await.unwrap();
}

#[tokio::test]
async fn three_hundred_one_identity_deadlines_are_scheduled_in_one_hundred_item_pages() {
    let (state, ca, rotation) = fixture(301).await;
    publish(&state, &ca, &rotation).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version=2"
        )
        .await,
        100
    );
    for expected in [200, 300, 301] {
        let work = claim(&state, "schedule").await;
        page(&state.pool, &work).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version=2"
            )
            .await,
            expected
        );
    }
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-identity-publication-schedule' AND state='complete'").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM resource_leases WHERE owner IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}

#[tokio::test]
async fn a_busy_identity_does_not_block_other_pages_and_is_revisited_after_cursor_wrap() {
    let (state, ca, rotation) = fixture(102).await;
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM mtls_user_identities WHERE user_id='user-0001' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    publish(&state, &ca, &rotation).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version=2"
        )
        .await,
        99
    );
    let work = claim(&state, "tail").await;
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version=2"
        )
        .await,
        101
    );
    held.rollback().await.unwrap();
    let work = claim(&state, "wrap").await;
    page(&state.pool, &work).await.unwrap();
    let work = claim(&state, "revisit").await;
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version=2"
        )
        .await,
        102
    );
}

#[tokio::test]
async fn acknowledgement_failure_rolls_back_schedule_markers_revision_cursor_and_native_conflict_retries_once()
 {
    let (state, ca, rotation) = fixture(101).await;
    publish(&state, &ca, &rotation).await;
    let work = claim(&state, "owner").await;
    let snapshot = "SELECT jsonb_agg(to_jsonb(i) ORDER BY id) FROM mtls_user_identities i";
    let before: Value = sqlx::query_scalar(snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_identity_schedule() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-identity-publication-schedule' AND NEW.state<>'running' THEN RAISE EXCEPTION 'reject schedule confirmation'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_identity_schedule BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_identity_schedule();").execute(&state.pool).await.unwrap();
    assert!(page(&state.pool, &work).await.is_err());
    assert!(
        before
            == sqlx::query_scalar::<_, Value>(snapshot)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
        "failed confirmation must preserve all identity metadata"
    );
    sqlx::raw_sql("DROP TRIGGER reject_identity_schedule ON background_work; CREATE SEQUENCE schedule_attempts; CREATE FUNCTION retry_identity_schedule() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-identity-publication-schedule' AND NEW.state<>'running' AND nextval('schedule_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_identity_schedule BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_identity_schedule();").execute(&state.pool).await.unwrap();
    page(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT last_value FROM schedule_attempts").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version=2"
        )
        .await,
        101
    );
    assert_eq!(
        count(
            &state,
            "SELECT revision FROM mtls_user_identities WHERE user_id='user-0101'"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn takeover_preserves_the_schedule_cursor_and_rejects_old_identity_writes() {
    let (state, ca, rotation) = fixture(101).await;
    publish(&state, &ca, &rotation).await;
    let old = claim(&state, "old").await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = claim(&state, "next").await;
    assert_eq!(old.payload, next.payload);
    assert!(page(&state.pool, &old).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version=2"
        )
        .await,
        100
    );
    page(&state.pool, &next).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version=2"
        )
        .await,
        101
    );
}

#[tokio::test]
async fn a_pause_after_publication_keeps_authoritative_access_disabled() {
    let (state, ca, rotation) = fixture(101).await;
    stage(&state, &ca, "user-0101", 2).await;
    publish(&state, &ca, &rotation).await;
    // Pause after publication but before this identity's scheduling page.
    sqlx::query("UPDATE users SET enabled=false WHERE id='user-0101'")
        .execute(&state.pool)
        .await
        .unwrap();
    let work = claim(&state, "paused").await;
    page(&state.pool, &work).await.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_user_identities WHERE user_id='user-0101' AND scheduled_ca_version=2 AND next_action_at IS NULL").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM users WHERE id='user-0101' AND NOT enabled AND revision=1"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_version_publication WHERE released"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn publication_outbox_and_logical_visibility_roll_back_when_the_publication_audit_fails() {
    let (state, ca, rotation) = fixture(1).await;
    stage(&state, &ca, "user-0001", 2).await;
    sqlx::raw_sql("CREATE FUNCTION reject_ca_publication() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='ca.published' THEN RAISE EXCEPTION 'reject publication audit'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_ca_publication BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_ca_publication();").execute(&state.pool).await.unwrap();
    assert!(
        super::super::publish_ready(&state, &rotation, &ca, 2)
            .await
            .is_err()
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_version_publication WHERE released"
        )
        .await,
        0
    );
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-identity-publication-schedule'"
        )
        .await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_ca_generations WHERE ca_version=2 AND state='preparing' AND published_at IS NULL").await,1);
    sqlx::query("DROP TRIGGER reject_ca_publication ON audit_records")
        .execute(&state.pool)
        .await
        .unwrap();
    publish(&state, &ca, &rotation).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_version_publication WHERE released"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn an_archived_or_superseded_publication_does_not_overwrite_pending_identity_metadata() {
    for change in [
        "UPDATE credentials SET archived=true WHERE kind='ca_identity'",
        "UPDATE mtls_rotations SET outcome='superseded_expired_publication'",
    ] {
        let (state, ca, rotation) = fixture(101).await;
        publish(&state, &ca, &rotation).await;
        let work = claim(&state, "old-publication").await;
        sqlx::query(change).execute(&state.pool).await.unwrap();
        page(&state.pool, &work).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version=2"
            )
            .await,
            100
        );
        assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE error_code='identity_publication_superseded' AND state='complete'").await,1);
    }
}

#[tokio::test]
async fn an_archived_identity_or_deleted_owner_is_not_restored_by_a_late_schedule_page() {
    for change in [
        "UPDATE mtls_user_identities SET archived=true,revision=revision+1 WHERE user_id='user-0101'",
        "DELETE FROM users WHERE id='user-0101'",
    ] {
        let (state, ca, rotation) = fixture(101).await;
        publish(&state, &ca, &rotation).await;
        let work = claim(&state, "late").await;
        sqlx::query(change).execute(&state.pool).await.unwrap();
        page(&state.pool, &work).await.unwrap();
        assert_eq!(count(&state,"SELECT count(*) FROM mtls_user_identities WHERE user_id='user-0101' AND scheduled_ca_version IS NOT NULL").await,0);
        assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-identity-publication-schedule' AND state='complete'").await,1);
    }
}

#[tokio::test]
async fn a_newer_ca_publication_supersedes_old_schedule_work_and_keeps_retired_material_logically_released()
 {
    use crate::api::{
        AdminActor,
        managed_mtls::{self, PublishAuthority},
    };
    use axum::{
        Json,
        extract::{Extension, Path, State},
    };
    let (state, ca, rotation) = fixture(101).await;
    stage(&state, &ca, "user-0001", 2).await;
    publish(&state, &ca, &rotation).await;
    let old = claim(&state, "old").await;
    old.retry(&state.pool).await.unwrap();
    sqlx::query("UPDATE mtls_rotations SET phase='complete'")
        .execute(&state.pool)
        .await
        .unwrap();
    let (_, Json(receipt)) = managed_mtls::publish(
        State(state.clone()),
        Extension(AdminActor { id: "test".into() }),
        Path(ca.clone()),
        Json(PublishAuthority {
            expected_revision: 2,
            certificate: None,
            private_key: None,
            replace_rotation_id: None,
            idempotency_key: "next-publication".into(),
        }),
    )
    .await
    .unwrap();
    let next = receipt["rotation_id"].as_str().unwrap();
    sqlx::query("UPDATE mtls_rotations SET phase='identities' WHERE id=$1")
        .bind(next)
        .execute(&state.pool)
        .await
        .unwrap();
    stage(&state, &ca, "user-0001", 3).await;
    super::super::publish_ready(&state, next, &ca, 3)
        .await
        .unwrap();
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        3
    );
    assert!(page(&state.pool, &old).await.is_err());
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE id=$1")
        .bind(&old.id)
        .execute(&state.pool)
        .await
        .unwrap();
    let obsolete = maintenance::claim_id(&state.pool, KIND, "obsolete", &old.id)
        .await
        .unwrap()
        .unwrap();
    page(&state.pool, &obsolete).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version=3"
        )
        .await,
        100
    );
    let current = claim(&state, "current").await;
    page(&state.pool, &current).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version=3"
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
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE released"
        )
        .await,
        2
    );
}

#[tokio::test]
async fn completed_schedule_markers_use_an_empty_index_range_instead_of_revisiting_all_identities()
{
    let (state, ca, rotation) = fixture(1001).await;
    publish(&state, &ca, &rotation).await;
    sqlx::query(
        "UPDATE mtls_user_identities SET scheduled_ca_version=2 WHERE user_id<>'deleted-owner'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::raw_sql("ANALYZE mtls_user_identities; ANALYZE users;")
        .execute(&state.pool)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar(&format!("EXPLAIN (ANALYZE,FORMAT JSON) {PENDING}"))
        .bind(&ca)
        .bind(2_i64)
        .bind(-1_i64)
        .bind("")
        .bind(PAGE)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert!(
        plan.to_string().contains("mtls_identity_schedule_pending"),
        "{plan}"
    );
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
}

#[tokio::test]
async fn migration_0057_to_0058_preserves_old_release_flags_encrypted_history_and_identity_state() {
    let pool = db::test_pool_through_version(8, 57).await;
    let (state, ca, _) = super::super::trust::tests::fixture_with_pool(pool, 0).await;
    users(&state, &ca, 1).await;
    sqlx::query("UPDATE users SET enabled=true")
        .execute(&state.pool)
        .await
        .unwrap();
    let identity: String =
        sqlx::query_scalar("SELECT id FROM mtls_user_identities WHERE user_id='user-0001'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO credentials(id,name,kind,owner_user_id,latest_version,created_at,updated_at) VALUES('retained-client','Client','tls_identity','user-0001',2,$1,$1)").bind(now).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE mtls_user_identities SET credential_id='retained-client' WHERE id=$1")
        .bind(&identity)
        .execute(&state.pool)
        .await
        .unwrap();
    for version in [1_i64, 2] {
        let (_, _, _, payload, _) =
            crate::credentials::load(&state.pool, &state.secrets, &ca, version)
                .await
                .unwrap();
        let authority = Authority::from_payload(&payload, now).unwrap();
        let issued = authority
            .sign_user(
                &uuid::Uuid::parse_str(&identity).unwrap(),
                Policy {
                    client_validity_days: 180,
                    renew_before_hours: 720,
                },
                now,
            )
            .unwrap();
        let payload = issued.material.payload();
        let metadata = crate::credentials::validate_payload("tls_identity", &payload).unwrap();
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) VALUES('retained-client',$1,$2,$3,$4)").bind(version).bind(state.secrets.encrypt(&payload.to_string()).unwrap()).bind(metadata).bind(now).execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO mtls_user_versions(identity_id,credential_id,credential_version,ca_id,ca_version,not_before,expires_at,next_action_at,released,created_at) VALUES($1,'retained-client',$2,$3,$2,$4,$5,$6,$7,$8)").bind(&identity).bind(version).bind(&ca).bind(issued.not_before).bind(issued.expires_at).bind(issued.next_action_at).bind(version==1).bind(now).execute(&state.pool).await.unwrap();
    }
    sqlx::query("UPDATE mtls_user_identities SET credential_id='retained-client' WHERE id=$1")
        .bind(identity)
        .execute(&state.pool)
        .await
        .unwrap();
    let snapshot = "SELECT jsonb_build_object('versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v),'leaves',(SELECT jsonb_agg(to_jsonb(v) ORDER BY identity_id,credential_version) FROM mtls_user_versions v),'identities',(SELECT jsonb_agg(to_jsonb(i)-'scheduled_ca_version' ORDER BY id) FROM mtls_user_identities i),'authorities',(SELECT jsonb_agg(to_jsonb(a) ORDER BY credential_id) FROM mtls_authorities a),'generations',(SELECT jsonb_agg(to_jsonb(g) ORDER BY ca_id,ca_version) FROM mtls_ca_generations g))";
    let before: Value = sqlx::query_scalar(snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    for _ in 0..2 {
        db::run_migrations(&state.pool).await.unwrap();
        assert!(
            before
                == sqlx::query_scalar::<_, Value>(snapshot)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap(),
            "publication migration must preserve historical source bytes and release flags"
        );
        let flags: Vec<bool> = sqlx::query_scalar(
            "SELECT released FROM mtls_user_version_publication ORDER BY credential_version",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(flags, vec![true, false]);
    }
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE scheduled_ca_version IS NOT NULL"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn compatibility_flags_are_materialized_in_one_hundred_row_pages_even_after_archival() {
    let (state, ca, rotation) = fixture(1).await;
    stage(&state, &ca, "user-0001", 2).await;
    let (identity, credential): (String, String) = sqlx::query_as(
        "SELECT id,credential_id FROM mtls_user_identities WHERE user_id='user-0001'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    // Retained versions can contain the same imported immutable material. Keep
    // its real identity, owner and issuer while expanding only version history.
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) SELECT v.credential_id,i,v.payload_enc,v.metadata,v.created_at FROM credential_versions v CROSS JOIN generate_series(2,301)i WHERE v.credential_id=$1 AND v.version=1").bind(&credential).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE credentials SET latest_version=301 WHERE id=$1")
        .bind(&credential)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO mtls_user_versions(identity_id,credential_id,credential_version,ca_id,ca_version,not_before,expires_at,next_action_at,released,created_at) SELECT v.identity_id,v.credential_id,i,v.ca_id,v.ca_version,v.not_before,v.expires_at,v.next_action_at,false,v.created_at FROM mtls_user_versions v CROSS JOIN generate_series(2,301)i WHERE v.identity_id=$1 AND v.credential_version=1").bind(&identity).execute(&state.pool).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT identity_id FROM mtls_user_versions FOR UPDATE")
        .fetch_all(&mut *held)
        .await
        .unwrap();
    publish(&state, &ca, &rotation).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_version_publication WHERE released"
        )
        .await,
        301
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE released"
        )
        .await,
        0
    );
    held.rollback().await.unwrap();
    sqlx::query("UPDATE credentials SET archived=true WHERE id=$1")
        .bind(&ca)
        .execute(&state.pool)
        .await
        .unwrap();
    let first = claim(&state, "compatibility").await;
    sqlx::raw_sql("CREATE FUNCTION reject_compatibility_flags() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-identity-publication-schedule' AND NEW.state<>'running' THEN RAISE EXCEPTION 'reject compatibility confirmation'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_compatibility_flags BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_compatibility_flags();").execute(&state.pool).await.unwrap();
    assert!(page(&state.pool, &first).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE released"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_compatibility_flags ON background_work; CREATE SEQUENCE compatibility_attempts; CREATE FUNCTION retry_compatibility_flags() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-identity-publication-schedule' AND NEW.state<>'running' AND nextval('compatibility_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_compatibility_flags BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_compatibility_flags();").execute(&state.pool).await.unwrap();
    page(&state.pool, &first).await.unwrap();
    assert_eq!(
        count(&state, "SELECT last_value FROM compatibility_attempts").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE released"
        )
        .await,
        100
    );
    for expected in [200, 300, 301] {
        let work = claim(&state, "compatibility").await;
        page(&state.pool, &work).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_user_versions WHERE released"
            )
            .await,
            expected
        );
    }
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-identity-publication-schedule' AND state='complete' AND error_code='identity_publication_superseded'").await,1);
}

#[tokio::test]
async fn actual_certificate_selection_observes_publication_before_legacy_flags_are_materialized() {
    let (state, ca, rotation) = fixture(1).await;
    stage(&state, &ca, "user-0001", 2).await;
    super::super::trust::tests::nodes(&state, &ca, 1).await;
    let node = "00000000-0000-4000-8000-000000000001";
    sqlx::query("DELETE FROM mtls_node_trust WHERE node_id='member-0001'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET id=$1 WHERE id='member-0001'")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO mtls_node_trust(node_id,ca_id,ca_version,desired,deployed) VALUES($1,$2,1,true,true)").bind(node).bind(&ca).execute(&state.pool).await.unwrap();
    super::super::trust::tests::prove(&state, &ca, node).await;
    assert!(
        identities::for_node(&state.pool, &state.secrets, "user-0001", node, &ca)
            .await
            .unwrap()
            .is_none()
    );
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT identity_id FROM mtls_user_versions FOR UPDATE")
        .fetch_all(&mut *held)
        .await
        .unwrap();
    publish(&state, &ca, &rotation).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE released"
        )
        .await,
        0
    );
    let material = identities::for_node(&state.pool, &state.secrets, "user-0001", node, &ca)
        .await
        .unwrap()
        .unwrap();
    assert!(material.0.contains("BEGIN CERTIFICATE") && material.1.contains("PRIVATE KEY"));
    held.rollback().await.unwrap();
}
