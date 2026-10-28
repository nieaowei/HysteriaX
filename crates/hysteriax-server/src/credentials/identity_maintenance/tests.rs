use super::*;
use crate::{
    api::{self, AdminActor},
    security::SecretBox,
};
use axum::{
    Json,
    extract::{Extension, State},
};

async fn fixture(n: i32) -> (AppState, String) {
    let pool = db::test_pool_with_max_connections(8).await;
    let state = AppState::new(
        pool,
        SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap(),
    );
    super::super::migration::migrate(&state.pool, &state.secrets)
        .await
        .unwrap();
    let (_, Json(ca)) = api::managed_mtls::create(
        State(state.clone()),
        Extension(AdminActor { id: "test".into() }),
        Json(
            serde_json::from_value(
                json!({"name":"CA","source":"generated","idempotency_key":"identity-ca"}),
            )
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    let ca = ca["id"].as_str().unwrap().to_owned();
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) SELECT 'u-'||lpad(i::text,4,'0'),'User',now(),now() FROM generate_series(1,$1)i").bind(n).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO mtls_user_identities(id,user_id,ca_id,created_at,updated_at) SELECT gen_random_uuid()::text,id,$1,now(),now() FROM users").bind(&ca).execute(&state.pool).await.unwrap();
    (state, ca)
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn admit(state: &AppState) {
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1")
        .bind(SCAN_KIND)
        .execute(&state.pool)
        .await
        .unwrap();
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN_KIND, json!({}))
        .await
        .unwrap();
    let work = maintenance::claim(&state.pool, SCAN_KIND, "admit")
        .await
        .unwrap()
        .unwrap();
    admit_work(&state.pool, &work).await.unwrap();
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
async fn admission_is_bounded_busy_early_identities_do_not_starve_other_work_and_existing_work_is_not_reset()
 {
    let (state, _) = fixture(301).await;
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM mtls_user_identities ORDER BY id LIMIT 1 FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    for expected in [100, 200, 300] {
        admit(&state).await;
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal'"
            )
            .await,
            expected
        );
    }
    let work = claim(&state, "live").await;
    admit(&state).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal'"
        )
        .await,
        300
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE owner='live' AND state='running'"
        )
        .await,
        1
    );
    held.rollback().await.unwrap();
    admit(&state).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal'"
        )
        .await,
        301
    );
    assert_eq!(work.leases[0].owner, "live");
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        0
    );
}

#[tokio::test]
async fn renewal_and_acknowledgement_roll_back_together_and_native_retry_publishes_one_signature() {
    let (state, _) = fixture(1).await;
    admit(&state).await;
    let work = claim(&state, "owner").await;
    sqlx::raw_sql("CREATE FUNCTION reject_identity_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-identity-renewal' AND NEW.state='complete' THEN RAISE EXCEPTION 'acknowledgement failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_identity_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_identity_ack();").execute(&state.pool).await.unwrap();
    assert!(renew_work(&state, &work).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credentials WHERE owner_user_id IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE credential_id IS NOT NULL"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_identity_ack ON background_work; CREATE SEQUENCE identity_attempts; CREATE FUNCTION retry_identity_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-identity-renewal' AND NEW.state='complete' AND nextval('identity_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_identity_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_identity_ack();").execute(&state.pool).await.unwrap();
    renew_work(&state, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT last_value FROM identity_attempts").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credentials WHERE owner_user_id IS NOT NULL"
        )
        .await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal' AND state='complete'").await,1);
    assert_eq!(count(&state, "SELECT revision FROM users").await, 1);
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}

#[tokio::test]
async fn takeover_rejects_prepared_old_owner_and_current_source_can_pause_and_resume_without_reissuing()
 {
    let (state, ca) = fixture(1).await;
    admit(&state).await;
    let old = claim(&state, "old").await;
    let prepared = identities::prepare_user(&state.pool, &state.secrets, "u-0001", &ca, 1, false)
        .await
        .unwrap();
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = claim(&state, "next").await;
    assert!(
        commit_work(&state, &old, Preparation::Signed(prepared))
            .await
            .is_err()
    );
    sqlx::query("UPDATE users SET enabled=false")
        .execute(&state.pool)
        .await
        .unwrap();
    renew_work(&state, &next).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE next_action_at IS NULL"
        )
        .await,
        1
    );
    sqlx::query("UPDATE users SET enabled=true")
        .execute(&state.pool)
        .await
        .unwrap();
    admit(&state).await;
    let work = claim(&state, "resumed").await;
    renew_work(&state, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        1
    );
    admit(&state).await;
    assert!(
        maintenance::claim(&state.pool, KIND, "again")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn obsolete_preparation_does_not_unarchive_or_publish_after_permission_removal() {
    let (state, ca) = fixture(1).await;
    admit(&state).await;
    let work = claim(&state, "owner").await;
    let prepared = identities::prepare_user(&state.pool, &state.secrets, "u-0001", &ca, 1, false)
        .await
        .unwrap();
    sqlx::query("UPDATE mtls_user_identities SET archived=true,authorization_revision=authorization_revision+1").execute(&state.pool).await.unwrap();
    commit_work(&state, &work, Preparation::Signed(prepared))
        .await
        .unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE archived"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE error_code='identity_removed'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn failed_preparation_is_sanitized_and_cannot_overwrite_a_newer_identity_revision() {
    let (state, _) = fixture(1).await;
    admit(&state).await;
    let work = claim(&state, "owner").await;
    sqlx::query("UPDATE mtls_user_identities SET revision=revision+1")
        .execute(&state.pool)
        .await
        .unwrap();
    commit_work(
        &state,
        &work,
        Preparation::Failed {
            revision: 1,
            version: 1,
            code: "stale error".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE maintenance_error IS NOT NULL"
        )
        .await,
        0
    );
    let work = claim(&state, "next").await;
    // A bad local decryption key exercises the real preparation failure branch;
    // its low-level error must never be published to identity state.
    let bad = AppState::new(
        state.pool.clone(),
        SecretBox::from_base64("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE").unwrap(),
    );
    renew_work(&bad, &work).await.unwrap();
    let error: String = sqlx::query_scalar("SELECT maintenance_error FROM mtls_user_identities")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(error, "Managed identity renewal failed; retry scheduled");
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_user_identities WHERE next_action_at>clock_timestamp()+interval '59 minutes'").await,1);
}

#[tokio::test]
async fn quota_crossing_after_signing_discards_prepared_material_without_changing_user_revision() {
    let (state, ca) = fixture(1).await;
    admit(&state).await;
    let work = claim(&state, "owner").await;
    let prepared = identities::prepare_user(&state.pool, &state.secrets, "u-0001", &ca, 1, false)
        .await
        .unwrap();
    assert!(prepared.will_issue());
    sqlx::query("UPDATE users SET quota_bytes=1,usage_bytes=1")
        .execute(&state.pool)
        .await
        .unwrap();
    commit_work(&state, &work, Preparation::Signed(prepared))
        .await
        .unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credentials WHERE owner_user_id IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(count(&state, "SELECT revision FROM users").await, 1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE next_action_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal' AND state='complete'").await,1);
}

#[tokio::test]
async fn admission_ack_failure_rolls_back_every_new_identity_work() {
    let (state, _) = fixture(3).await;
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN_KIND, json!({}))
        .await
        .unwrap();
    let work = maintenance::claim(&state.pool, SCAN_KIND, "admit")
        .await
        .unwrap()
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_identity_admission() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-identity-admission' AND NEW.state='complete' THEN RAISE EXCEPTION 'admission acknowledgement failed'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_identity_admission BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_identity_admission();").execute(&state.pool).await.unwrap();
    assert!(admit_work(&state.pool, &work).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal'"
        )
        .await,
        0
    );
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-identity-admission' AND state='running'").await,1);
    sqlx::query("DROP TRIGGER reject_identity_admission ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    admit_work(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal'"
        )
        .await,
        3
    );
}

#[tokio::test]
async fn due_inactive_identities_are_admitted_in_pages_then_paused_without_signing() {
    let (state, _) = fixture(301).await;
    sqlx::query("UPDATE users SET enabled=false")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE mtls_user_identities SET next_action_at=clock_timestamp()-interval '1 second'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    for expected in [100, 200, 300, 301] {
        admit(&state).await;
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal'"
            )
            .await,
            expected
        );
    }
    let work = claim(&state, "owner").await;
    renew_work(&state, &work).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE next_action_at IS NULL"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        0
    );
    admit(&state).await;
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-identity-renewal' AND state='complete'").await,1);
}
