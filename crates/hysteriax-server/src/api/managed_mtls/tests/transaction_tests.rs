use super::node_flow::managed_node;
use super::*;
async fn count(state: &AppState, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
#[tokio::test]
async fn ca_creation_audit_failure_leaves_no_authority_material_or_receipt() {
    let state = state().await;
    sqlx::raw_sql("CREATE FUNCTION reject_ca_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='ca.created' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_ca_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_ca_audit();").execute(&state.pool).await.unwrap();
    assert!(
        create(State(state.clone()), actor(), Json(request("failed")))
            .await
            .is_err()
    );
    assert_eq!(count(&state, "SELECT count(*) FROM credentials").await, 0);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_authorities").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_ca_generations").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_operations").await,
        0
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.create_request_failed'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn ca_creation_native_conflict_commits_one_material_and_receipt() {
    let state = state().await;
    sqlx::raw_sql("CREATE SEQUENCE create_ca_attempts; CREATE FUNCTION retry_ca_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='ca.created' AND nextval('create_ca_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_ca_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_ca_audit();").execute(&state.pool).await.unwrap();
    let (_, Json(receipt)) = create(State(state.clone()), actor(), Json(request("retry-ca")))
        .await
        .unwrap();
    let (id, cipher): (String, String) =
        sqlx::query_as("SELECT credential_id,payload_enc FROM credential_versions")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(id, receipt["id"].as_str().unwrap());
    let payload: Value = serde_json::from_str(&state.secrets.decrypt(&cipher).unwrap()).unwrap();
    Authority::from_payload(&payload, Utc::now()).unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_operations").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.created'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM create_ca_attempts").await,
        2
    );
}
#[tokio::test]
async fn concurrent_ca_creation_rolls_back_the_duplicate_authority() {
    let state = state().await;
    sqlx::raw_sql("CREATE SEQUENCE ca_arrivals; CREATE FUNCTION await_ca_peer() RETURNS trigger LANGUAGE plpgsql AS $$ DECLARE arrived bigint; deadline timestamptz:=clock_timestamp()+interval '3 seconds'; BEGIN IF NEW.kind='ca_identity' THEN PERFORM nextval('ca_arrivals'); LOOP SELECT last_value INTO arrived FROM ca_arrivals; EXIT WHEN arrived>=2; IF clock_timestamp()>deadline THEN RAISE EXCEPTION 'CA barrier timeout'; END IF; PERFORM pg_sleep(0.01); END LOOP; END IF; RETURN NEW; END $$; CREATE TRIGGER await_ca_peer BEFORE INSERT ON credentials FOR EACH ROW EXECUTE FUNCTION await_ca_peer();").execute(&state.pool).await.unwrap();
    let (a, b) = tokio::join!(
        create(State(state.clone()), actor(), Json(request("same-ca"))),
        create(State(state.clone()), actor(), Json(request("same-ca")))
    );
    let (_, Json(a)) = a.unwrap();
    let (_, Json(b)) = b.unwrap();
    assert_eq!(a, b);
    for table in [
        "credentials",
        "credential_versions",
        "mtls_authorities",
        "mtls_ca_generations",
        "mtls_operations",
    ] {
        assert_eq!(
            count(&state, &format!("SELECT count(*) FROM {table}")).await,
            1
        );
    }
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.created'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn native_receipt_collision_replays_after_rollback_without_reexecuting_work() {
    let state = state().await;
    let (_, Json(original)) = create(State(state.clone()), actor(), Json(request("native-ca")))
        .await
        .unwrap();
    let actor = actor().0;
    let request_digest = digest(&request("native-ca")).unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let error = receipt(
        &mut tx,
        &actor,
        "native-ca",
        "ca.create",
        &request_digest,
        &original,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "mtls_operation_committed");
    tx.rollback().await.unwrap();
    let mut error = Some(error);
    let mut attempts = 0;
    let replay = managed_transaction(
        &state.pool,
        &actor,
        "native-ca",
        "ca.create",
        &request_digest,
        |_| {
            attempts += 1;
            let error = error.take().unwrap();
            Box::pin(async move { Err(error) })
        },
    )
    .await
    .unwrap();
    assert_eq!(replay, original);
    assert_eq!(attempts, 1);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_operations").await,
        1
    );
}
#[tokio::test]
async fn policy_conflict_changes_one_revision() {
    let state = state().await;
    let ca = generated(&state).await;
    let a = UpdatePolicy {
        expected_revision: 1,
        policy: Policy {
            client_validity_days: 60,
            renew_before_hours: 24,
        },
        idempotency_key: "policy-a".into(),
    };
    let b = UpdatePolicy {
        expected_revision: 1,
        policy: Policy {
            client_validity_days: 90,
            renew_before_hours: 24,
        },
        idempotency_key: "policy-b".into(),
    };
    let (a, b) = tokio::join!(
        update_policy(State(state.clone()), actor(), Path(ca.clone()), Json(a)),
        update_policy(State(state.clone()), actor(), Path(ca.clone()), Json(b))
    );
    assert_eq!([&a, &b].iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        [&a, &b]
            .iter()
            .filter(|r| r
                .as_ref()
                .err()
                .is_some_and(|e| e.status == StatusCode::CONFLICT))
            .count(),
        1
    );
    assert_eq!(
        count(&state, "SELECT policy_revision FROM mtls_authorities").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.policy_updated'"
        )
        .await,
        1
    );
}
#[tokio::test]
async fn referenced_publication_native_conflict_commits_one_rotation_and_batch() {
    let state = state().await;
    let ca = generated(&state).await;
    let _ = managed_node(&state, &ca).await;
    sqlx::raw_sql("CREATE SEQUENCE publication_attempts; CREATE FUNCTION retry_publication_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='ca.version_prepared' AND nextval('publication_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_publication_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_publication_audit();").execute(&state.pool).await.unwrap();
    let (_, Json(receipt)) = publish(
        State(state.clone()),
        actor(),
        Path(ca.clone()),
        Json(publish_request(1, "retry-publish")),
    )
    .await
    .unwrap();
    assert_eq!(receipt["version"], 2);
    assert_eq!(receipt["published"], false);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_rotation_nodes").await,
        0
    );
    crate::credentials::rotations::advance(&state, receipt["rotation_id"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_rotations").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_rotation_nodes").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='ca.version_prepared'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM publication_attempts").await,
        2
    );
}
#[tokio::test]
async fn reissue_audit_failure_rolls_back_then_native_conflict_keeps_one_new_version() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let (identity, _) = issue(&state, "alice", &ca, false).await;
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM mtls_user_identities WHERE id=$1")
        .bind(identity)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let input = ReissueUser {
        ca_credential_id: Some(ca.clone()),
        expected_revisions: [(ca, revision)].into(),
        idempotency_key: "retry-reissue".into(),
    };
    sqlx::raw_sql("CREATE FUNCTION reject_reissue_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='mtls.user_reissued' THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_reissue_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_reissue_audit();").execute(&state.pool).await.unwrap();
    assert!(
        reissue_user(
            State(state.clone()),
            actor(),
            Path("alice".into()),
            Json(input.clone())
        )
        .await
        .is_err()
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_operations WHERE operation='mtls.reissue'"
        )
        .await,
        0
    );
    sqlx::raw_sql("DROP TRIGGER reject_reissue_audit ON audit_records; CREATE SEQUENCE reissue_attempts; CREATE FUNCTION retry_reissue_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='mtls.user_reissued' AND nextval('reissue_attempts')=1 THEN RAISE EXCEPTION 'injected conflict' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_reissue_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_reissue_audit();").execute(&state.pool).await.unwrap();
    let Json(receipt) = reissue_user(
        State(state.clone()),
        actor(),
        Path("alice".into()),
        Json(input),
    )
    .await
    .unwrap();
    assert_eq!(receipt["identities"][0]["credential_version"], 2);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        2
    );
    assert_eq!(
        count(&state, "SELECT revision FROM users WHERE id='alice'").await,
        1
    );
    assert_eq!(count(&state, "SELECT count(*) FROM jobs").await, 0);
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='mtls.user_reissued'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&state, "SELECT last_value FROM reissue_attempts").await,
        2
    );
}

#[tokio::test]
async fn policy_and_publication_audit_failures_preserve_authority_history_and_receipts() {
    let state = state().await;
    let ca = generated(&state).await;
    let query = "SELECT jsonb_build_object('credential',(SELECT to_jsonb(c) FROM credentials c WHERE id=$1),'authority',(SELECT to_jsonb(a) FROM mtls_authorities a WHERE credential_id=$1),'versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY version) FROM credential_versions v WHERE credential_id=$1),'generations',(SELECT jsonb_agg(to_jsonb(g) ORDER BY ca_version) FROM mtls_ca_generations g WHERE ca_id=$1))";
    let before: Value = sqlx::query_scalar(query)
        .bind(&ca)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_authority_change_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action IN ('ca.policy_updated','ca.version_prepared') THEN RAISE EXCEPTION 'injected failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_authority_change_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION reject_authority_change_audit();").execute(&state.pool).await.unwrap();
    assert!(
        update_policy(
            State(state.clone()),
            actor(),
            Path(ca.clone()),
            Json(UpdatePolicy {
                expected_revision: 1,
                policy: Policy {
                    client_validity_days: 60,
                    renew_before_hours: 24
                },
                idempotency_key: "failed-policy".into()
            })
        )
        .await
        .is_err()
    );
    assert!(
        publish(
            State(state.clone()),
            actor(),
            Path(ca.clone()),
            Json(publish_request(1, "failed-publication"))
        )
        .await
        .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, Value>(query)
            .bind(&ca)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        before
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_operations").await,
        1
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM credential_batches").await,
        0
    );
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_rotations").await,
        0
    );
}
#[tokio::test]
async fn concurrent_same_key_reissues_return_one_version_and_receipt() {
    let state = state().await;
    let ca = generated(&state).await;
    user(&state, "alice", true).await;
    let (identity, _) = issue(&state, "alice", &ca, false).await;
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM mtls_user_identities WHERE id=$1")
        .bind(identity)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let input = ReissueUser {
        ca_credential_id: Some(ca.clone()),
        expected_revisions: [(ca, revision)].into(),
        idempotency_key: "same-reissue".into(),
    };
    let (a, b) = tokio::join!(
        reissue_user(
            State(state.clone()),
            actor(),
            Path("alice".into()),
            Json(input.clone())
        ),
        reissue_user(
            State(state.clone()),
            actor(),
            Path("alice".into()),
            Json(input)
        )
    );
    let Json(a) = a.unwrap();
    let Json(b) = b.unwrap();
    assert_eq!(a, b);
    assert_eq!(
        count(&state, "SELECT count(*) FROM mtls_user_versions").await,
        2
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM mtls_operations WHERE operation='mtls.reissue'"
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &state,
            "SELECT count(*) FROM audit_records WHERE action='mtls.user_reissued'"
        )
        .await,
        1
    );
}
