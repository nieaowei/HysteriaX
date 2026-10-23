use super::*;

async fn fixture() -> AppState {
    let pool = db::test_pool_with_max_connections(4).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    AppState::new(pool, secrets)
}
async fn prepare(state: &AppState) -> PreparedAuthorityCreation {
    prepare_authority_creation(
        state,
        AdminActor {
            id: "prepared".into(),
        },
        CreateAuthority {
            name: "Prepared CA".into(),
            source: Source::Generated,
            certificate: None,
            private_key: None,
            policy: Policy::default(),
            idempotency_key: "prepared".into(),
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn ca_creation_preparation_is_readonly_and_native_retries_preserve_exact_material() {
    for code in ["40001", "40P01"] {
        let state = fixture().await;
        let prepared = prepare(&state).await;
        let material = prepared.material.clone().unwrap();
        let before:i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM credentials)+(SELECT count(*) FROM mtls_operations)+(SELECT count(*) FROM audit_records)").fetch_one(&state.pool).await.unwrap();
        assert_eq!(before, 0);
        sqlx::raw_sql(&format!("CREATE SEQUENCE creation_attempt; CREATE FUNCTION retry_ca_creation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('creation_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_ca_creation BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_ca_creation()"))
            .execute(&state.pool).await.unwrap();
        let response = db::serializable(&state.pool, move |tx| {
            let prepared = prepared.clone();
            Box::pin(async move { create_authority_on(tx, prepared).await })
        })
        .await
        .unwrap();
        assert_eq!(response["id"], material.id);
        let row=sqlx::query("SELECT c.created_at,v.payload_enc,v.metadata FROM credentials c JOIN credential_versions v ON v.credential_id=c.id WHERE c.id=$1")
            .bind(&material.id).fetch_one(&state.pool).await.unwrap();
        assert_eq!(row.get::<String, _>("payload_enc"), material.ciphertext);
        assert_eq!(row.get::<Value, _>("metadata"), material.metadata);
        assert_eq!(
            row.get::<DateTime<Utc>, _>("created_at"),
            material.timestamp
        );
        let payload: Value =
            serde_json::from_str(&state.secrets.decrypt(&material.ciphertext).unwrap()).unwrap();
        Authority::from_payload(&payload, Utc::now()).unwrap();
        let attempts: i64 = sqlx::query_scalar("SELECT last_value FROM creation_attempt")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(attempts, 2);
    }
}

#[tokio::test]
async fn material_expired_during_preparation_cannot_commit_a_ca_or_success_receipt() {
    let state = fixture().await;
    let mut prepared = prepare(&state).await;
    prepared.material.as_mut().unwrap().expires_at =
        sqlx::query_scalar("SELECT clock_timestamp()-interval '1 second'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let error = db::serializable(&state.pool, move |tx| {
        let prepared = prepared.clone();
        Box::pin(async move { create_authority_on(tx, prepared).await })
    })
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    let count:i64=sqlx::query_scalar("SELECT (SELECT count(*) FROM credentials)+(SELECT count(*) FROM mtls_operations)+(SELECT count(*) FROM audit_records)").fetch_one(&state.pool).await.unwrap();
    assert_eq!(count, 0);
}
