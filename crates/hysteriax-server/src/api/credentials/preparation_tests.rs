use super::*;

#[tokio::test]
async fn creation_and_publication_native_retries_preserve_prepared_ids_ciphertexts_and_batch() {
    let (state, _) = super::super::transaction_tests::fixture().await;
    let prepared = Creation::new(
        &state.secrets,
        CreateCredential {
            name: "Fixed".into(),
            kind: "api_token".into(),
            reminder_at: None,
            payload: json!({"token":"fixed-secret"}),
        },
    )
    .unwrap();
    let (id, cipher) = (prepared.id.clone(), prepared.ciphertext.clone());
    sqlx::raw_sql("CREATE SEQUENCE preparation_attempt; CREATE FUNCTION retry_preparation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('preparation_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40001'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_preparation BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_preparation()")
            .execute(&state.pool).await.unwrap();
    let created = crate::db::serializable(&state.pool, move |tx| {
        let prepared = prepared.clone();
        Box::pin(async move {
            prepared
                .commit_on(
                    tx,
                    &AdminActor {
                        id: "prepared-test".into(),
                    },
                )
                .await
        })
    })
    .await
    .unwrap();
    assert_eq!(created["id"], id);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT payload_enc FROM credential_versions WHERE credential_id=$1 AND version=1"
        )
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        cipher
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM preparation_attempt")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
    sqlx::raw_sql("ALTER SEQUENCE preparation_attempt RESTART WITH 1; CREATE OR REPLACE FUNCTION retry_preparation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF nextval('preparation_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='40P01'; END IF; RETURN NEW; END $$")
            .execute(&state.pool).await.unwrap();
    let prepared = Publication::new(
        &state,
        &id,
        &PublishVersion {
            expected_revision: 1,
            payload: json!({"token":"new-secret"}),
        },
    )
    .await
    .unwrap();
    let (cipher, batch) = (prepared.ciphertext.clone(), prepared.batch.clone());
    let current = id.clone();
    let published = crate::db::serializable(&state.pool, move |tx| {
        let (prepared, id) = (prepared.clone(), current.clone());
        Box::pin(async move {
            super::super::mutations::publish_on(
                tx,
                id,
                1,
                prepared,
                AdminActor {
                    id: "prepared-test".into(),
                },
            )
            .await
        })
    })
    .await
    .unwrap();
    assert_eq!(published["batch_id"], batch);
    assert_eq!(
        sqlx::query_scalar::<_, String>(
            "SELECT payload_enc FROM credential_versions WHERE credential_id=$1 AND version=2"
        )
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        cipher
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT last_value FROM preparation_attempt")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn changed_revision_archive_kind_or_owner_rejects_prepared_publication_without_writing() {
    for (change, status) in [
        ("revision=revision+1", axum::http::StatusCode::CONFLICT),
        ("archived=TRUE", axum::http::StatusCode::CONFLICT),
        ("kind='api_token'", axum::http::StatusCode::CONFLICT),
        ("owner_user_id='owner'", axum::http::StatusCode::BAD_REQUEST),
    ] {
        let (state, id) = super::super::transaction_tests::fixture().await;
        let prepared = Publication::new(
            &state,
            &id,
            &PublishVersion {
                expected_revision: 1,
                payload: json!({"secret":"candidate"}),
            },
        )
        .await
        .unwrap();
        sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('owner','Owner',clock_timestamp(),clock_timestamp())").execute(&state.pool).await.unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE credentials SET {change} WHERE id=$1"
        )))
        .bind(&id)
        .execute(&state.pool)
        .await
        .unwrap();
        let query = "SELECT jsonb_build_object('credentials',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM credentials t),'versions',(SELECT jsonb_agg(to_jsonb(t) ORDER BY credential_id,version) FROM credential_versions t),'batches',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM credential_batches t),'jobs',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM jobs t),'audits',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM audit_records t))";
        let before: Value = sqlx::query_scalar(query)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let error = crate::db::serializable(&state.pool, move |tx| {
            let (prepared, id) = (prepared.clone(), id.clone());
            Box::pin(async move {
                super::super::mutations::publish_on(
                    tx,
                    id,
                    1,
                    prepared,
                    AdminActor {
                        id: "prepared-test".into(),
                    },
                )
                .await
            })
        })
        .await
        .unwrap_err();
        assert_eq!(error.status, status);
        assert_eq!(
            sqlx::query_scalar::<_, Value>(query)
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            before
        );
    }
}
