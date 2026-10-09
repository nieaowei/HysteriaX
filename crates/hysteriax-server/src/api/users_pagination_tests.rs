use super::*;

async fn fixture() -> AppState {
    let pool = crate::db::test_pool().await;
    let state = AppState::new(
        pool,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    sqlx::query("INSERT INTO users(id,name,enabled,quota_bytes,usage_bytes,revision,created_at,updated_at) SELECT 'user-' || lpad(i::text,3,'0'), CASE WHEN i=235 THEN '历史%_用户' ELSE 'Same' END, i%2=0, 1000, i, i, '2026-01-01'::timestamptz, '2026-01-01'::timestamptz FROM generate_series(1,235) i")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT 'node-'||i, 'Node '||i, '127.0.0.1',22,'root','private_key','unused','example.test',443,':443','hash-'||i,'unused','unused','{}',now(),now() FROM generate_series(1,2) i")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('user-235','node-1','one','encrypted-secret',now()),('user-235','node-2','two','encrypted-secret',now())")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO authorization_groups(id,name,revision,created_at,updated_at) VALUES('group-1','Group',1,now(),now())")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO authorization_group_users(group_id,user_id,created_at) VALUES('group-1','user-235',now())")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO authorization_group_nodes(group_id,node_id,created_at) VALUES('group-1','node-1',now()),('group-1','node-2',now())")
        .execute(&state.pool).await.unwrap();
    state
}

async fn page(state: &AppState, query: Value) -> Value {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        list_page(
            State(state.clone()),
            Query(serde_json::from_value(query).unwrap()),
        ),
    )
    .await
    .expect(
        "pagination must hydrate on its existing connection, including with a one-connection pool",
    )
    .unwrap()
    .0
}

#[tokio::test]
async fn users_pagination_covers_all_users_with_stable_names_and_clamps() {
    let state = fixture().await;
    let first = page(&state, json!({})).await;
    assert_eq!(first["total"], 235);
    assert_eq!(first["page_size"], 50);
    assert_eq!(first["items"][0]["id"], "user-001");
    let mut ids = Vec::new();
    for n in 1..=5 {
        let result = page(&state, json!({"page":n})).await;
        ids.extend(
            result["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|user| user["id"].as_str().unwrap().to_owned()),
        );
    }
    assert_eq!(ids.len(), 235);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        235
    );
    assert_eq!(ids.last().unwrap(), "user-235");
    let last = page(&state, json!({"page":i64::MAX})).await;
    assert_eq!(last["page"], 5);
    assert_eq!(last["items"].as_array().unwrap().len(), 35);
    assert_eq!(
        page(&state, json!({"order":"desc","page_size":25})).await["items"][0]["id"],
        "user-235"
    );
    assert_eq!(
        page(&state, json!({"sort":"created_at","order":"desc"})).await["items"][0]["id"],
        "user-235"
    );
    let Json(catalog) = list(State(state.clone())).await.unwrap();
    assert_eq!(
        catalog.len(),
        235,
        "node/group catalogs and overview must still receive all users"
    );
}

#[tokio::test]
async fn users_pagination_preserves_authorization_and_searches_assignments_without_duplicates() {
    let state = fixture().await;
    for query in [" 历史%_ ", "USER-235", "node-", "node-1"] {
        let result = page(&state, json!({"q":query})).await;
        assert_eq!(result["total"], 1);
        let user = &result["items"][0];
        assert_eq!(user["id"], "user-235");
        assert_eq!(user["usage_bytes"], 235);
        assert_eq!(user["quota_bytes"], 1000);
        assert_eq!(user["revision"], 235);
        assert_eq!(user["enabled"], false);
        assert_eq!(user["assignments"].as_array().unwrap().len(), 2);
        assert_eq!(user["authorization_groups"][0]["id"], "group-1");
        assert_eq!(user["assignments"][0]["source_groups"][0]["id"], "group-1");
        assert!(user.get("credential_enc").is_none());
        let Json(detail) = get(State(state.clone()), Path("user-235".to_owned()))
            .await
            .unwrap();
        assert_eq!(user, &detail);
    }
    let empty = page(&state, json!({"q":"' OR true --","page":99})).await;
    assert_eq!(empty["total"], 0);
    assert_eq!(empty["page"], 1);
    assert_eq!(empty["items"], json!([]));
    sqlx::query("DELETE FROM users")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(page(&state, json!({"page":2})).await["total"], 0);
}

#[test]
fn users_pagination_rejects_invalid_parameters() {
    for value in [
        json!({"page":0}),
        json!({"page":-1}),
        json!({"page_size":0}),
        json!({"page_size":201}),
        json!({"sort":"name; DROP TABLE users"}),
        json!({"order":"ASC;--"}),
    ] {
        let query: UsersPageQuery = serde_json::from_value(value).unwrap();
        assert!(query.validate().is_err());
    }
}
