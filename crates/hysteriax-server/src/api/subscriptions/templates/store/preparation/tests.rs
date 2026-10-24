use super::*;

async fn state() -> AppState {
    let pool = db::test_pool_with_max_connections(5).await;
    let secrets = SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap();
    initialize(&pool, &secrets).await.unwrap();
    AppState::new(pool, secrets)
}

#[tokio::test]
async fn native_conflict_retries_reuse_the_prepared_identifier_and_ciphertext() {
    for code in ["40001", "40P01"] {
        let state = state().await;
        let prepared = PreparedCreation::new(
            &state,
            Create {
                expected_revision: 0,
                name: "Prepared".into(),
                format: "mihomo".into(),
                document: None,
            },
        )
        .unwrap();
        sqlx::raw_sql(&format!("CREATE SEQUENCE creation_attempt; CREATE FUNCTION retry_template_creation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='template.created' AND nextval('creation_attempt')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_template_creation BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_template_creation()"))
            .execute(&state.pool).await.unwrap();
        let (status, Json(result)) = db::serializable::<_, ApiError, _>(&state.pool, |tx| {
            let prepared = prepared.clone();
            Box::pin(async move { prepared.commit_on(tx).await })
        })
        .await
        .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(result["id"], prepared.id);
        let ciphertext: String =
            sqlx::query_scalar("SELECT draft_enc FROM subscription_templates WHERE id=$1")
                .bind(&prepared.id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(ciphertext, prepared.ciphertext);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT last_value FROM creation_attempt")
                .fetch_one(&state.pool)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM audit_records WHERE action='template.created'"
            )
            .fetch_one(&state.pool)
            .await
            .unwrap(),
            1
        );
    }
}

#[tokio::test]
async fn publication_preparation_does_not_wait_for_a_writer_and_checks_the_exact_source_at_commit()
{
    let state = state().await;
    let mut blocker = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM subscription_templates WHERE id='default-mihomo' FOR UPDATE")
        .fetch_one(&mut *blocker)
        .await
        .unwrap();
    let prepared = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        PreparedPublication::new(&state, "default-mihomo", 1, None),
    )
    .await
    .expect("validation must not acquire the template write lock")
    .unwrap();
    blocker.rollback().await.unwrap();
    let mut document = render::single_seed("mihomo");
    document.common["allow-lan"] = json!(true);
    let ciphertext = state
        .secrets
        .encrypt(&serde_json::to_string(&document).unwrap())
        .unwrap();
    sqlx::query("UPDATE subscription_templates SET draft_enc=$1 WHERE id='default-mihomo'")
        .bind(&ciphertext)
        .execute(&state.pool)
        .await
        .unwrap();
    let error = db::serializable::<_, ApiError, _>(&state.pool, |tx| {
        let prepared = prepared.clone();
        Box::pin(async move { prepared.commit_on(tx).await })
    })
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    let row = sqlx::query("SELECT revision,published_version,draft_enc FROM subscription_templates WHERE id='default-mihomo'")
        .fetch_one(&state.pool).await.unwrap();
    assert_eq!(row.get::<i64, _>("revision"), 1);
    assert_eq!(row.get::<i64, _>("published_version"), 1);
    assert_eq!(row.get::<String, _>("draft_enc"), ciphertext);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM subscription_template_versions WHERE template_id='default-mihomo'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_records")
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        0
    );
}
