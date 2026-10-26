use super::*;

async fn retry_audit(state: &AppState, action: &str, code: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SEQUENCE preparation_attempts; CREATE FUNCTION retry_preparation_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='{action}' AND nextval('preparation_attempts')=1 THEN RAISE EXCEPTION 'retry' USING ERRCODE='{code}'; END IF; RETURN NEW; END $$; CREATE TRIGGER retry_preparation_audit BEFORE INSERT ON audit_records FOR EACH ROW EXECUTE FUNCTION retry_preparation_audit()")))
        .execute(&state.pool).await.unwrap();
}

#[tokio::test]
async fn creation_retries_reuse_the_prepared_identifiers_secrets_and_snapshot() {
    let (state, _) = transaction_tests::fixture().await;
    let ssh: String = sqlx::query_scalar("SELECT ssh_credential_id FROM nodes")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let input = serde_json::from_value(json!({"name":"Prepared","ssh_host":"8.8.8.8","ssh_port":22,"ssh_username":"root","ssh_credential_id":ssh,"ssh_credential_version":1,"public_host":"8.8.4.4","public_port":443,"listen_addr":":443"})).unwrap();
    let prepared = prepare_creation(&state, input).await.unwrap();
    retry_audit(&state, "node.created", "40001").await;
    let (app, creation) = (state.clone(), prepared.clone());
    let result = db::serializable(&state.pool, move |tx| {
        let (app, creation) = (app.clone(), creation.clone());
        Box::pin(async move { insert_node_on(tx, &app, &creation).await })
    })
    .await
    .unwrap();
    let NodeCreation::Created(receipt) = result else {
        panic!("unexpected legacy replay")
    };
    assert_eq!(receipt["node"]["id"], prepared.id);
    assert_eq!(receipt["node_auth_token"], prepared.token);
    let node = sqlx::query("SELECT * FROM nodes WHERE id=$1")
        .bind(&prepared.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        node.get::<String, _>("node_token_enc"),
        prepared.node_token_enc
    );
    assert_eq!(
        node.get::<String, _>("traffic_stats_secret_enc"),
        prepared.stats_secret_enc
    );
    assert_eq!(
        node.get::<String, _>("desired_config_enc"),
        prepared.config_enc
    );
    assert_eq!(
        node.get::<String, _>("node_token_hash"),
        token_digest(&prepared.token)
    );
    assert_eq!(
        node.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        prepared.timestamp
    );
    let history = sqlx::query("SELECT * FROM config_versions WHERE node_id=$1")
        .bind(&prepared.id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(history.get::<String, _>("id"), prepared.history_id);
    assert_eq!(
        history.get::<String, _>("config_enc"),
        prepared.deployment_snapshot_enc
    );
    assert_eq!(history.get::<String, _>("content_sha256"), prepared.digest);
    let attempts: i64 = sqlx::query_scalar("SELECT last_value FROM preparation_attempts")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(attempts, 2);
}

#[tokio::test]
async fn patch_retries_reuse_the_prepared_history_and_ciphertext() {
    let (state, node) = transaction_tests::fixture().await;
    let prepared = prepare_patch(
        &state,
        node.clone(),
        serde_json::from_value(json!({"expected_revision":1,"traffic_stats_port":12000})).unwrap(),
    )
    .await
    .unwrap();
    retry_audit(&state, "node.updated", "40P01").await;
    let (app, patch) = (state.clone(), prepared.clone());
    let Json(receipt) = db::serializable(&state.pool, move |tx| {
        let (app, patch) = (app.clone(), patch.clone());
        Box::pin(async move { patch_on(tx, &app, (*patch).clone()).await })
    })
    .await
    .unwrap();
    assert_eq!(receipt["revision"], 2);
    let history = sqlx::query("SELECT * FROM config_versions WHERE node_id=$1 AND revision=2")
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(history.get::<String, _>("id"), prepared.history_id);
    assert_eq!(
        history.get::<String, _>("config_enc"),
        prepared.deployment_snapshot_enc
    );
    assert_eq!(
        history.get::<String, _>("content_sha256"),
        prepared.deployment_digest
    );
    let cipher: String = sqlx::query_scalar("SELECT desired_config_enc FROM nodes WHERE id=$1")
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(cipher, prepared.config_enc);
    let attempts: i64 = sqlx::query_scalar("SELECT last_value FROM preparation_attempts")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(attempts, 2);
}

#[tokio::test]
async fn unchanged_business_revision_cannot_overwrite_changed_configuration_or_connection() {
    for field in ["desired_config_enc", "public_host"] {
        let (state, node) = transaction_tests::fixture().await;
        let prepared = prepare_patch(
            &state,
            node.clone(),
            serde_json::from_value(json!({"expected_revision":1,"name":"New name"})).unwrap(),
        )
        .await
        .unwrap();
        let changed = if field == "desired_config_enc" {
            state
                .secrets
                .encrypt("{\"bandwidth\":{\"up\":\"10 mbps\"}}")
                .unwrap()
        } else {
            "changed.example.test".into()
        };
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE nodes SET {field}=$1 WHERE id=$2"
        )))
        .bind(&changed)
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
        let before: Value = sqlx::query_scalar("SELECT to_jsonb(n) FROM nodes n WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let (app, patch) = (state.clone(), prepared.clone());
        let error = db::serializable(&state.pool, move |tx| {
            let (app, patch) = (app.clone(), patch.clone());
            Box::pin(async move { patch_on(tx, &app, (*patch).clone()).await })
        })
        .await
        .unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT);
        let after: Value = sqlx::query_scalar("SELECT to_jsonb(n) FROM nodes n WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(after, before);
        for (query, expected) in [
            ("SELECT count(*) FROM config_versions", 1_i64),
            ("SELECT count(*) FROM jobs", 0),
            (
                "SELECT count(*) FROM audit_records WHERE action='node.updated'",
                0,
            ),
        ] {
            let actual: i64 = sqlx::query_scalar(query)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(actual, expected);
        }
    }
}

#[tokio::test]
async fn independent_operational_observations_do_not_invalidate_a_prepared_edit() {
    let (state, node) = transaction_tests::fixture().await;
    let prepared = prepare_patch(
        &state,
        node.clone(),
        serde_json::from_value(json!({"expected_revision":1,"name":"New name"})).unwrap(),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE nodes SET last_seen_at=clock_timestamp(),updated_at=clock_timestamp(),state='ready' WHERE id=$1").bind(&node).execute(&state.pool).await.unwrap();
    let last_seen: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT last_seen_at FROM nodes WHERE id=$1")
            .bind(&node)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let (app, patch) = (state.clone(), prepared.clone());
    let Json(receipt) = db::serializable(&state.pool, move |tx| {
        let (app, patch) = (app.clone(), patch.clone());
        Box::pin(async move { patch_on(tx, &app, (*patch).clone()).await })
    })
    .await
    .unwrap();
    assert_eq!(receipt["revision"], 2);
    let row = sqlx::query("SELECT name,desired_revision,state,last_seen_at FROM nodes WHERE id=$1")
        .bind(&node)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("name"), "New name");
    assert_eq!(row.get::<i64, _>("desired_revision"), 2);
    assert_eq!(row.get::<String, _>("state"), "ready");
    assert_eq!(
        row.get::<chrono::DateTime<chrono::Utc>, _>("last_seen_at"),
        last_seen
    );
}
