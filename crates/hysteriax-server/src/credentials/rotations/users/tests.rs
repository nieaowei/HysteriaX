use super::*;

async fn fixture(n: i32) -> (AppState, String, String) {
    let (state, ca, rotation) = super::super::trust::tests::fixture(1).await;
    initialize(&state, n).await;
    (state, ca, rotation)
}
async fn initialize(state: &AppState, n: i32) {
    sqlx::query("UPDATE mtls_rotations SET phase='identities'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) SELECT 'user-'||lpad(i::text,4,'0'),'User',now(),now() FROM generate_series(1,$1)i").bind(n).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(node_id,user_id,credential_hash,credential_enc,created_at) SELECT 'member-0001',id,'assignment-'||id,$1,now() FROM users").bind(state.secrets.encrypt("fixture-only").unwrap()).execute(&state.pool).await.unwrap();
}
async fn count(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn admission(state: &AppState, ca: &str, rotation: &str) -> Work {
    let key = format!("rotation-users:{rotation}");
    maintenance::ensure(&state.pool, &key, ADMIT, admission_payload(rotation, ca, 2))
        .await
        .unwrap();
    maintenance::claim_id(&state.pool, ADMIT, "admit", &format!("maintenance:{key}"))
        .await
        .unwrap()
        .unwrap()
}
async fn admit(state: &AppState, ca: &str, rotation: &str) {
    let work = admission(state, ca, rotation).await;
    admit_users(&state.pool, &work).await.unwrap();
}
async fn claim(state: &AppState, owner: &str) -> Work {
    maintenance::claim(&state.pool, ISSUE, owner)
        .await
        .unwrap()
        .unwrap()
}
async fn prepare(state: &AppState, work: &Work) -> PreparedIdentity {
    let t = Target::from_work(work).unwrap();
    identities::prepare_user(
        &state.pool,
        &state.secrets,
        &t.user,
        &t.ca,
        t.version,
        false,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn three_hundred_one_missing_identities_and_signatures_are_admitted_in_one_hundred_item_pages()
 {
    let (state, ca, rotation) = fixture(301).await;
    for expected in [100, 200, 300, 301] {
        admit(&state, &ca, &rotation).await;
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM mtls_user_identities WHERE user_id<>'deleted-owner'"
            )
            .await,
            expected
        );
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-issuance'"
            )
            .await,
            expected
        );
        assert_eq!(
            count(&state, "SELECT count(*) FROM mtls_user_versions").await,
            0
        );
        assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    }
    let live = claim(&state, "live").await;
    let key = format!("rotation-users:{rotation}");
    maintenance::ensure(
        &state.pool,
        &key,
        ADMIT,
        admission_payload(&rotation, &ca, 2),
    )
    .await
    .unwrap();
    assert!(
        maintenance::claim_id(&state.pool, ADMIT, "other", &format!("maintenance:{key}"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE owner='live' AND state='running'"
        )
        .await,
        1
    );
    assert_eq!(live.leases.len(), 3);
    let mut tx = state.pool.begin().await.unwrap();
    assert!(!missing_identity_on(&mut tx, &ca).await.unwrap());
    assert!(missing_leaf_on(&mut tx, &ca, 2).await.unwrap());
}

#[tokio::test]
async fn a_busy_user_is_skipped_then_revisited_and_inactive_users_are_admitted_without_signing() {
    let (state, ca, rotation) = fixture(102).await;
    sqlx::query("UPDATE users SET enabled=false WHERE id='user-0002'")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id='user-0001' FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    admit(&state, &ca, &rotation).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE user_id<>'deleted-owner'"
        )
        .await,
        100
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-issuance'"
        )
        .await,
        99
    );
    held.rollback().await.unwrap();
    admit(&state, &ca, &rotation).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE user_id<>'deleted-owner'"
        )
        .await,
        102
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-issuance'"
        )
        .await,
        101
    );
    sqlx::query("UPDATE users SET enabled=true WHERE id='user-0002'")
        .execute(&state.pool)
        .await
        .unwrap();
    admit(&state, &ca, &rotation).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-issuance'"
        )
        .await,
        102
    );
}

#[tokio::test]
async fn admission_confirmation_failure_rolls_back_identities_and_all_signature_work() {
    let (state, ca, rotation) = fixture(101).await;
    let work = admission(&state, &ca, &rotation).await;
    sqlx::raw_sql("CREATE FUNCTION reject_user_admission() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-user-admission' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject confirmation'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_user_admission BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_user_admission();").execute(&state.pool).await.unwrap();
    assert!(admit_users(&state.pool, &work).await.is_err());
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_identities").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-issuance'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_user_admission ON background_work; CREATE SEQUENCE admission_attempts; CREATE FUNCTION retry_user_admission() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-user-admission' AND NEW.state='complete' AND nextval('admission_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_user_admission BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_user_admission();").execute(&state.pool).await.unwrap();
    admit_users(&state.pool, &work).await.unwrap();
    assert_eq!(
        count(&state, "SELECT last_value FROM admission_attempts").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_identities").await,
        101
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-issuance'"
        )
        .await,
        100
    );
}

#[tokio::test]
async fn signature_count_and_confirmation_are_atomic_and_native_retry_reuses_the_prepared_cipher() {
    let (state, ca, rotation) = fixture(1).await;
    admit(&state, &ca, &rotation).await;
    let work = claim(&state, "owner").await;
    let p = prepare(&state, &work).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM credentials WHERE owner_user_id IS NOT NULL"
        )
        .await,
        0
    );
    sqlx::raw_sql("CREATE FUNCTION reject_user_signature() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-user-issuance' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject confirmation'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_user_signature BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_user_signature();").execute(&state.pool).await.unwrap();
    assert!(
        commit_work(&state, &work, Preparation::Signed(p.clone()))
            .await
            .is_err()
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
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT prepared_count FROM mtls_rotations").await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_user_signature ON background_work; CREATE SEQUENCE signature_attempts; CREATE SEQUENCE cipher_checks; CREATE SEQUENCE cipher_hash MINVALUE 0 START 0; CREATE FUNCTION remember_staged_cipher() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE h bigint; BEGIN IF NEW.metadata->>'identity_id' IS NOT NULL THEN h:=hashtextextended(NEW.payload_enc,0) & 9223372036854775807; IF nextval('cipher_checks')=1 THEN PERFORM setval('cipher_hash',h,true); ELSE IF h<>(SELECT last_value FROM cipher_hash) THEN RAISE EXCEPTION 'retry changed immutable cipher'; END IF; END IF; END IF; RETURN NEW; END $$; CREATE TRIGGER remember_staged_cipher BEFORE INSERT ON credential_versions FOR EACH ROW EXECUTE FUNCTION remember_staged_cipher(); CREATE FUNCTION retry_user_signature() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-user-issuance' AND NEW.state='complete' AND nextval('signature_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_user_signature BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_user_signature();").execute(&state.pool).await.unwrap();
    commit_work(&state, &work, Preparation::Signed(p))
        .await
        .unwrap();
    assert_eq!(
        count(&state, "SELECT last_value FROM signature_attempts").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM cipher_checks").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT prepared_count FROM mtls_rotations").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE NOT released AND NOT discarded"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
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
async fn takeover_rejects_old_signatures_and_shares_identity_resources_with_regular_renewal() {
    let (state, ca, rotation) = fixture(1).await;
    admit(&state, &ca, &rotation).await;
    let old = claim(&state, "old").await;
    let p = prepare(&state, &old).await;
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second' WHERE work_id=$1").bind(&old.id).execute(&state.pool).await.unwrap();
    let next = claim(&state, "next").await;
    assert!(
        commit_work(&state, &old, Preparation::Signed(p))
            .await
            .is_err()
    );
    let t = Target::from_work(&next).unwrap();
    maintenance::ensure(
        &state.pool,
        "regular",
        "test-renewal",
        json!({"lease_resources":[format!("identity:{}",t.identity)]}),
    )
    .await
    .unwrap();
    assert!(
        maintenance::claim(&state.pool, "test-renewal", "renewal")
            .await
            .unwrap()
            .is_none()
    );
    issue_work(&state, &next).await.unwrap();
    assert_eq!(
        count(&state, "SELECT prepared_count FROM mtls_rotations").await,
        1
    );
    assert!(
        maintenance::claim(&state.pool, "test-renewal", "renewal")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn revoked_ineligible_deleted_or_superseded_sources_cannot_publish_an_old_signature() {
    for change in [
        "UPDATE mtls_user_identities SET archived=true,revision=revision+1,authorization_revision=authorization_revision+1 WHERE user_id<>'deleted-owner'",
        "UPDATE users SET enabled=false",
        "UPDATE users SET expires_at=clock_timestamp()-interval '1 second'",
        "UPDATE users SET quota_bytes=10,usage_bytes=10",
        "DELETE FROM node_assignments; DELETE FROM users",
        "UPDATE credentials SET archived=true WHERE kind='ca_identity'",
        "UPDATE mtls_rotations SET phase='complete',outcome='superseded_unpublished_target'",
        "UPDATE mtls_ca_generations SET expires_at=clock_timestamp()-interval '1 second' WHERE ca_version=2",
    ] {
        let (state, ca, rotation) = fixture(1).await;
        admit(&state, &ca, &rotation).await;
        let work = claim(&state, "owner").await;
        let p = prepare(&state, &work).await;
        sqlx::raw_sql(change).execute(&state.pool).await.unwrap();
        commit_work(&state, &work, Preparation::Signed(p))
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
        assert_eq!(
            count(&state, "SELECT prepared_count FROM mtls_rotations").await,
            0
        );
    }
}

#[tokio::test]
async fn old_user_or_policy_revision_is_rejected_and_another_identitys_candidate_cannot_be_substituted()
 {
    for change in [
        "UPDATE users SET revision=revision+1",
        "UPDATE mtls_authorities SET policy_revision=policy_revision+1",
    ] {
        let (state, ca, rotation) = fixture(1).await;
        admit(&state, &ca, &rotation).await;
        let work = claim(&state, "owner").await;
        let p = prepare(&state, &work).await;
        sqlx::query(change).execute(&state.pool).await.unwrap();
        assert!(
            commit_work(&state, &work, Preparation::Signed(p))
                .await
                .is_err()
        );
        issue_work(&state, &work).await.unwrap();
        assert_eq!(
            count(&state, "SELECT prepared_count FROM mtls_rotations").await,
            1
        );
    }
    let (state, ca, rotation) = fixture(2).await;
    admit(&state, &ca, &rotation).await;
    let work = claim(&state, "owner").await;
    let t = Target::from_work(&work).unwrap();
    let other: String = sqlx::query_scalar("SELECT id FROM users WHERE id<>$1")
        .bind(&t.user)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let p = identities::prepare_user(&state.pool, &state.secrets, &other, &ca, 2, false)
        .await
        .unwrap();
    assert!(
        commit_work(&state, &work, Preparation::Signed(p))
            .await
            .is_err()
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        0
    );
}

#[tokio::test]
async fn preparation_errors_and_backoff_are_atomic_sanitized_and_do_not_overwrite_new_source() {
    let (state, ca, rotation) = fixture(1).await;
    admit(&state, &ca, &rotation).await;
    let work = claim(&state, "owner").await;
    let t = Target::from_work(&work).unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let source = source_on(&mut tx, &t).await.unwrap().unwrap();
    tx.rollback().await.unwrap();
    let failed = Preparation::Failed {
        user_revision: source.user_revision,
        policy_revision: source.policy_revision,
    };
    sqlx::raw_sql("CREATE FUNCTION reject_user_error() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-user-issuance' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject confirmation'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_user_error BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_user_error();").execute(&state.pool).await.unwrap();
    assert!(commit_work(&state, &work, failed.clone()).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotations WHERE error_message IS NOT NULL"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.rotation_failed'"
        )
        .await,
        0
    );
    sqlx::query("DROP TRIGGER reject_user_error ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    // A changed policy invalidates the old preparation error. Retry promptly
    // without publishing a one-hour error about that superseded snapshot.
    sqlx::query("UPDATE mtls_authorities SET policy_revision=policy_revision+1")
        .execute(&state.pool)
        .await
        .unwrap();
    commit_work(&state, &work, failed).await.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-issuance' AND state='queued' AND error_code='rotation_identity_preparation_required' AND available_at<clock_timestamp()+interval '2 seconds'").await,1);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_rotations WHERE error_message IS NOT NULL"
        )
        .await,
        0
    );
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1")
        .bind(ISSUE)
        .execute(&state.pool)
        .await
        .unwrap();
    let current = claim(&state, "current").await;
    // Exercise an actual decryption failure with a mismatched fixture key,
    // preserving the immutable stored CA material and its database protection.
    let mut wrong_key_state = state.clone();
    wrong_key_state.secrets = SecretBox::from_base64("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE")
        .unwrap()
        .into();
    issue_work(&wrong_key_state, &current).await.unwrap();
    assert_eq!(count(&state,"SELECT count(*) FROM mtls_rotations WHERE error_message='Shared identity preparation failed; retry scheduled'").await,1);
    assert_eq!(count(&state,"SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-issuance' AND state='complete' AND error_code='rotation_identity_preparation_failed' AND available_at>clock_timestamp()+interval '59 minutes'").await,1);
    assert_eq!(count(&state,"SELECT count(*) FROM audit_records WHERE action='ca.rotation_failed' AND detail_json->>'reason'='Shared identity preparation failed; retry scheduled'").await,1);
    admit(&state, &ca, &rotation).await;
    assert!(
        maintenance::claim(&state.pool, ISSUE, "too-early")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        0
    );
}

#[tokio::test]
async fn manually_staged_material_supersedes_queued_work_without_duplicate_issuance_or_count() {
    let (state, ca, rotation) = fixture(1).await;
    admit(&state, &ca, &rotation).await;
    let work = claim(&state, "owner").await;
    let p = prepare(&state, &work).await;
    let secrets = state.secrets.clone();
    let manual = p.clone();
    db::serializable::<_, anyhow::Error, _>(&state.pool, move |tx| {
        let (p, secrets) = (manual.clone(), secrets.clone());
        Box::pin(async move { identities::commit_user(tx, &secrets, p).await })
    })
    .await
    .unwrap();
    commit_work(&state, &work, Preparation::Signed(p))
        .await
        .unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT prepared_count FROM mtls_rotations").await,
        0
    );
    admit(&state, &ca, &rotation).await;
    assert!(
        maintenance::claim(&state.pool, ISSUE, "duplicate")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn new_assignments_without_identity_rows_cannot_be_hidden_by_a_completed_admission_page() {
    let (state, ca, rotation) = fixture(1).await;
    sqlx::query("UPDATE users SET enabled=false")
        .execute(&state.pool)
        .await
        .unwrap();
    admit(&state, &ca, &rotation).await;
    let mut tx = state.pool.begin().await.unwrap();
    assert!(!missing_identity_on(&mut tx, &ca).await.unwrap());
    assert!(!missing_leaf_on(&mut tx, &ca, 2).await.unwrap());
    tx.rollback().await.unwrap();
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES('late','Late',false,now(),now())").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(node_id,user_id,credential_hash,credential_enc,created_at) VALUES('member-0001','late','late-hash',$1,now())").bind(state.secrets.encrypt("fixture-only").unwrap()).execute(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    assert!(missing_identity_on(&mut tx, &ca).await.unwrap());
    tx.rollback().await.unwrap();
    admit(&state, &ca, &rotation).await;
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_identities WHERE user_id='late'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        0
    );
}

async fn scan(state: &AppState) -> Work {
    sqlx::query("UPDATE background_work SET available_at='2000-01-01' WHERE kind=$1")
        .bind(SCAN)
        .execute(&state.pool)
        .await
        .unwrap();
    maintenance::ensure(&state.pool, SCAN_KEY, SCAN, json!({}))
        .await
        .unwrap();
    maintenance::claim(&state.pool, SCAN, "scan")
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn rotation_scan_is_bounded_skips_busy_rows_and_confirms_all_child_work_atomically() {
    let (state, ca, rotation) = fixture(0).await;
    super::super::trust::tests::many_rotations(&state, &ca, 300).await;
    sqlx::query("UPDATE mtls_rotations SET phase='identities'")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM mtls_rotations WHERE id=$1 FOR UPDATE")
        .bind(&rotation)
        .fetch_one(&mut *held)
        .await
        .unwrap();
    let w = scan(&state).await;
    sqlx::raw_sql("CREATE FUNCTION reject_user_scan() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-user-scan' AND NEW.state='complete' THEN RAISE EXCEPTION 'reject scan confirmation'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_user_scan BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_user_scan();").execute(&state.pool).await.unwrap();
    assert!(admit_rotations(&state.pool, &w).await.is_err());
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-admission'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_user_scan ON background_work; CREATE SEQUENCE user_scan_attempts; CREATE FUNCTION retry_user_scan() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='mtls-rotation-user-scan' AND NEW.state='complete' AND nextval('user_scan_attempts')=1 THEN RAISE EXCEPTION USING ERRCODE='40001',MESSAGE='native conflict'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_user_scan BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION retry_user_scan();").execute(&state.pool).await.unwrap();
    admit_rotations(&state.pool, &w).await.unwrap();
    assert_eq!(
        count(&state, "SELECT last_value FROM user_scan_attempts").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-admission'"
        )
        .await,
        100
    );
    for expected in [200, 300] {
        let w = scan(&state).await;
        admit_rotations(&state.pool, &w).await.unwrap();
        assert_eq!(
            count(
                &state,
                "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-admission'"
            )
            .await,
            expected
        );
    }
    held.rollback().await.unwrap();
    let w = scan(&state).await;
    admit_rotations(&state.pool, &w).await.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM background_work WHERE kind='mtls-rotation-user-admission'"
        )
        .await,
        301
    );
}

#[tokio::test]
async fn concurrent_schedulers_stage_one_signature_without_releasing_or_publishing_it() {
    let (state, _, _) = fixture(1).await;
    let (a, b) = tokio::join!(scheduled_pass(&state), scheduled_pass(&state));
    a.unwrap();
    b.unwrap();
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_user_versions WHERE NOT released AND NOT discarded"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT prepared_count FROM mtls_rotations").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT published_version FROM mtls_authorities").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
}

#[tokio::test]
async fn migration_0056_to_0057_preserves_source_material_and_pending_work_byte_for_byte() {
    let pool = db::test_pool_through_version(8, 56).await;
    let (state, ca, rotation) = super::super::trust::tests::fixture_with_pool(pool, 1).await;
    initialize(&state, 3).await;
    // Persist the historical issuance format without calling a worker compiled
    // against the new lease schema before the upgrade.
    sqlx::query("INSERT INTO mtls_user_identities(id,user_id,ca_id,created_at,updated_at) SELECT gen_random_uuid()::text,id,$1,now(),now() FROM users")
        .bind(&ca).execute(&state.pool).await.unwrap();
    let rows = sqlx::query("SELECT id,user_id,revision,authorization_revision FROM mtls_user_identities WHERE user_id LIKE 'user-%' ORDER BY user_id")
        .fetch_all(&state.pool).await.unwrap();
    let mut retained_work = String::new();
    for (index, row) in rows.into_iter().enumerate() {
        let identity: String = row.get("id");
        let id = format!("rotation-user:{rotation}:{identity}");
        let payload = json!({"rotation_id":rotation,"ca_id":ca,"ca_version":2,"identity_id":identity,"user_id":row.get::<String,_>("user_id"),"identity_revision":row.get::<i64,_>("revision"),"authorization_revision":row.get::<i64,_>("authorization_revision"),"lease_resources":[format!("ca:{ca}"),format!("identity:{identity}")]});
        sqlx::query("INSERT INTO background_work(id,kind,payload) VALUES($1,$2,$3)")
            .bind(&id)
            .bind(ISSUE)
            .bind(payload)
            .execute(&state.pool)
            .await
            .unwrap();
        if index == 0 {
            retained_work = id.clone();
            sqlx::query("UPDATE background_work SET state='running',owner='retained-owner',generation=1 WHERE id=$1")
                .bind(&id).execute(&state.pool).await.unwrap();
            for key in [
                format!("ca:{ca}"),
                format!("identity:{identity}"),
                format!("work:{id}"),
            ] {
                sqlx::query("INSERT INTO resource_leases(resource_key,owner,work_id,generation,expires_at) VALUES($1,'retained-owner',$2,1,clock_timestamp()+interval '1 minute')")
                    .bind(key).bind(&id).execute(&state.pool).await.unwrap();
            }
        }
    }
    let snapshot = "SELECT jsonb_build_object('credentials',(SELECT jsonb_agg(to_jsonb(c) ORDER BY id) FROM credentials c),'versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY credential_id,version) FROM credential_versions v),'authorities',(SELECT jsonb_agg(to_jsonb(a) ORDER BY credential_id) FROM mtls_authorities a),'generations',(SELECT jsonb_agg(to_jsonb(g) ORDER BY ca_id,ca_version) FROM mtls_ca_generations g),'nodes',(SELECT jsonb_agg(to_jsonb(n) ORDER BY id) FROM nodes n),'users',(SELECT jsonb_agg(to_jsonb(u) ORDER BY id) FROM users u),'assignments',(SELECT jsonb_agg(to_jsonb(a) ORDER BY node_id,user_id) FROM node_assignments a),'identities',(SELECT jsonb_agg(to_jsonb(i)-'scheduled_ca_version' ORDER BY id) FROM mtls_user_identities i),'rotations',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM mtls_rotations r),'work',(SELECT jsonb_agg(to_jsonb(w) ORDER BY id) FROM background_work w),'leases',(SELECT jsonb_agg(to_jsonb(l)-'epoch' ORDER BY resource_key) FROM resource_leases l))";
    let before: Value = sqlx::query_scalar(snapshot)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    for _ in 0..2 {
        db::run_migrations(&state.pool).await.unwrap();
        let after: Value = sqlx::query_scalar(snapshot)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert!(
            before == after,
            "index migration must preserve every retained business and execution row"
        );
    }
    assert_eq!(count(&state,"SELECT count(*) FROM pg_indexes WHERE schemaname=current_schema() AND indexname='background_work_rotation_identity_due'").await,1);
    let rows=sqlx::query("SELECT resource_key,owner,generation,epoch,work_id FROM resource_leases WHERE work_id=$1 ORDER BY resource_key")
        .bind(&retained_work).fetch_all(&state.pool).await.unwrap();
    assert_eq!(rows.len(), 3);
    for row in rows {
        let lease = crate::execution::Execution {
            resource: row.get("resource_key"),
            owner: row.get("owner"),
            generation: row.get("generation"),
            epoch: row.get("epoch"),
            work: row.get("work_id"),
        };
        assert!(lease.renew(&state.pool).await.unwrap());
    }
}

#[tokio::test]
async fn direct_rotation_advancement_uses_its_indexed_empty_due_range_despite_other_rotations_backlog()
 {
    let (state, ca, rotation) = fixture(1).await;
    admit(&state, &ca, &rotation).await;
    sqlx::query(
        "UPDATE background_work SET available_at=clock_timestamp()+interval '1 day' WHERE kind=$1",
    )
    .bind(ISSUE)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO background_work(id,kind,payload) SELECT 'unrelated-rotation-work-'||i,$1,jsonb_build_object('rotation_id','unrelated-'||i) FROM generate_series(1,1000)i").bind(ISSUE).execute(&state.pool).await.unwrap();
    sqlx::query("ANALYZE background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (ANALYZE,FORMAT JSON) {ISSUE_DUE}"
    )))
    .bind(ISSUE)
    .bind(&rotation)
    .bind(PAGE)
    .bind(now)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(
        plan.to_string()
            .contains("background_work_rotation_identity_due"),
        "{plan}"
    );
    assert_eq!(plan[0]["Plan"]["Actual Rows"].as_f64(), Some(0.0));
}
