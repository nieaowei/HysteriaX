use super::*;

#[tokio::test]
async fn direct_membership_changes_keep_last_source_and_reject_stale_revision() {
    let state = tests::fixture().await;
    let first = tests::create_group(&state, "First").await;
    let second = tests::create_group(&state, "Second").await;
    let first_id = first["id"].as_str().unwrap();
    let second_id = second["id"].as_str().unwrap();
    let revision: i64 = sqlx::query_scalar("SELECT revision FROM users WHERE id='user'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let Json(receipt) = update_memberships(
        State(state.clone()),
        Path("user".into()),
        Json(MembershipUpdateRequest {
            expected_revision: revision,
            group_ids: vec![second_id.into()],
        }),
    )
    .await
    .unwrap();
    assert_eq!(receipt["removals_count"], 0);
    assert_eq!(receipt["assignments"].as_array().unwrap().len(), 1);
    assert_eq!(
        receipt["assignments"][0]["source_groups"][0]["id"],
        second_id
    );
    assert!(
        update_memberships(
            State(state.clone()),
            Path("user".into()),
            Json(MembershipUpdateRequest {
                expected_revision: revision,
                group_ids: vec![first_id.into()],
            })
        )
        .await
        .is_err()
    );
    let Json(receipt) = update_memberships(
        State(state.clone()),
        Path("user".into()),
        Json(MembershipUpdateRequest {
            expected_revision: receipt["revision"].as_i64().unwrap(),
            group_ids: vec![],
        }),
    )
    .await
    .unwrap();
    assert_eq!(receipt["removals_count"], 1);
    assert_eq!(receipt["assignments"], json!([]));
    assert_eq!(receipt["revocation_job_ids"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn preview_table_migration_drops_existing_tokens_and_preserves_authorization() {
    let state = tests::fixture().await;
    let created = tests::create_group(&state, "Keep").await;
    let baseline = include_str!("../../../migrations/0001_baseline.sql");
    let start = baseline
        .find("CREATE TABLE authorization_group_previews (")
        .unwrap();
    let end = baseline[start..].find("\n);").unwrap() + start + 3;
    sqlx::raw_sql(sqlx::AssertSqlSafe(&baseline[start..end]))
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::raw_sql("ALTER TABLE authorization_group_previews ADD PRIMARY KEY(token); CREATE INDEX authorization_group_previews_expiry_cursor_idx ON authorization_group_previews(expires_at,token); ALTER TABLE authorization_group_previews ADD FOREIGN KEY(group_id) REFERENCES authorization_groups(id) ON DELETE CASCADE; ALTER TABLE authorization_group_previews ADD FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE;")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO authorization_group_previews(token,action,group_id,request_json,guard_json,result_json,created_at,expires_at) VALUES('obsolete','delete',$1,'{}','{}','{}',now(),now()+interval '10 minutes')")
        .bind(created["id"].as_str().unwrap()).execute(&state.pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../migrations/0003_remove_authorization_previews.sql"
    ))
    .execute(&state.pool)
    .await
    .unwrap();
    let absent: bool =
        sqlx::query_scalar("SELECT to_regclass('authorization_group_previews') IS NULL")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(absent);
    let counts: (i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM authorization_groups),(SELECT count(*) FROM authorization_group_users),(SELECT count(*) FROM authorization_group_nodes),(SELECT count(*) FROM node_assignments)")
        .fetch_one(&state.pool).await.unwrap();
    assert_eq!(counts, (1, 1, 1, 1));
}

#[tokio::test]
async fn thousand_member_grant_batches_statements_and_preserves_each_password() {
    let state = tests::fixture().await;
    let users: Vec<String> = (0..1000).map(|n| format!("batch-{n:04}")).collect();
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) SELECT id,id,now(),now() FROM unnest($1::text[]) AS u(id)")
        .bind(&users).execute(&state.pool).await.unwrap();
    sqlx::raw_sql("CREATE TABLE authorization_statement_counts(kind text PRIMARY KEY,calls int NOT NULL); CREATE FUNCTION count_authorization_statements() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO authorization_statement_counts(kind,calls) VALUES(TG_TABLE_NAME||':'||TG_OP,1) ON CONFLICT(kind) DO UPDATE SET calls=authorization_statement_counts.calls+1; RETURN NULL; END $$; CREATE TRIGGER count_grant_members AFTER INSERT ON authorization_group_users FOR EACH STATEMENT EXECUTE FUNCTION count_authorization_statements(); CREATE TRIGGER count_grant_assignments AFTER INSERT ON node_assignments FOR EACH STATEMENT EXECUTE FUNCTION count_authorization_statements(); CREATE TRIGGER count_user_revisions AFTER UPDATE ON users FOR EACH STATEMENT EXECUTE FUNCTION count_authorization_statements();")
        .execute(&state.pool).await.unwrap();
    let (_, Json(receipt)) = create(
        State(state.clone()),
        Json(FixtureGroupCreate {
            name: "Batch grant".into(),
            user_ids: users.clone(),
            node_ids: vec!["node".into()],
        }),
    )
    .await
    .unwrap();
    assert_eq!(receipt, json!({"revocation_job_ids":[]}));
    let statements: Vec<(String, i32)> =
        sqlx::query_as("SELECT kind,calls FROM authorization_statement_counts ORDER BY kind")
            .fetch_all(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        statements,
        vec![
            ("authorization_group_users:INSERT".into(), 1),
            ("node_assignments:INSERT".into(), 1),
            ("users:UPDATE".into(), 1)
        ]
    );
    let rows: Vec<(String,String,String,i64)> = sqlx::query_as("SELECT a.user_id,a.credential_hash,a.credential_enc,u.revision FROM node_assignments a JOIN users u ON u.id=a.user_id WHERE a.user_id=ANY($1) ORDER BY a.user_id")
        .bind(users).fetch_all(&state.pool).await.unwrap();
    assert_eq!(rows.len(), 1000);
    let mut passwords = BTreeSet::new();
    for (_user, hash, cipher, revision) in rows {
        let password = state.secrets.decrypt(&cipher).unwrap();
        assert_eq!(hash, token_digest(&password));
        passwords.insert(password);
        assert_eq!(revision, 2);
    }
    assert_eq!(passwords.len(), 1000);
}

#[tokio::test]
async fn adding_group_nodes_does_not_wait_for_monitoring_non_key_node_lock() {
    let state = tests::fixture().await;
    sqlx::query("DELETE FROM node_assignments")
        .execute(&state.pool)
        .await
        .unwrap();
    let (_, Json(initial)) = create(
        State(state.clone()),
        Json(FixtureGroupCreate {
            name: "Concurrent monitoring".into(),
            user_ids: vec!["user".into()],
            node_ids: vec![],
        }),
    )
    .await
    .unwrap();
    assert_eq!(initial, json!({"revocation_job_ids":[]}));
    let initial = group_named(&state, "Concurrent monitoring").await;
    let id = initial["id"].as_str().unwrap().to_owned();
    let mut monitoring = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM nodes WHERE id='node' FOR NO KEY UPDATE")
        .fetch_one(&mut *monitoring)
        .await
        .unwrap();
    let saved = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        update(
            State(state.clone()),
            Path(id.clone()),
            Json(FixtureGroupReplace {
                expected_revision: 1,
                name: "Concurrent monitoring".into(),
                user_ids: vec!["user".into()],
                node_ids: vec!["node".into()],
            }),
        ),
    )
    .await;
    monitoring.rollback().await.unwrap();
    let Json(receipt) = saved
        .expect("node authorization must not wait for monitoring's non-key lock")
        .unwrap();
    assert_eq!(receipt, json!({"revocation_job_ids":[]}));
    assert_eq!(
        group_named(&state, "Concurrent monitoring").await["revision"],
        2
    );
    let assignments: i64 = sqlx::query_scalar("SELECT count(*) FROM node_assignments")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(assignments, 1);
    let Json(removed) = update(
        State(state.clone()),
        Path(id),
        Json(FixtureGroupReplace {
            expected_revision: 2,
            name: "Concurrent monitoring".into(),
            user_ids: vec!["user".into()],
            node_ids: vec![],
        }),
    )
    .await
    .unwrap();
    assert_eq!(removed.as_object().unwrap().len(), 1);
    assert_eq!(removed["revocation_job_ids"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn authorization_node_key_lock_still_prevents_concurrent_deletion() {
    let state = tests::fixture().await;
    let proposal = GroupProposal {
        name: "Protected".into(),
        user_ids: vec![],
        node_ids: vec!["node".into()],
    };
    let (managed, _) = prepare_group_change(&state, "create", None, &proposal)
        .await
        .unwrap();
    let mut authorization = state.pool.begin().await.unwrap();
    lock_authorization_graph_on(&mut authorization, &[], &[], &proposal.node_ids, &managed)
        .await
        .unwrap();
    let mut deletion = state.pool.begin().await.unwrap();
    let mut request =
        Box::pin(sqlx::query("DELETE FROM nodes WHERE id='node'").execute(&mut *deletion));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut request)
            .await
            .is_err()
    );
    authorization.rollback().await.unwrap();
    request.await.unwrap();
    deletion.rollback().await.unwrap();
}
