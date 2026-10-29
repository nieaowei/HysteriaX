use super::*;
use crate::security::token_digest;

async fn fixture() -> AppState {
    let pool = crate::db::test_pool().await;
    let state = AppState::new(
        pool,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    sqlx::query("INSERT INTO authorization_groups(id,name,revision,created_at,updated_at) SELECT 'group-'||lpad(i::text,3,'0'),CASE WHEN i=235 THEN '历史%_组' ELSE 'Same' END,7,'2026-01-01'::timestamptz,'2026-01-01'::timestamptz FROM generate_series(1,235) i").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) SELECT 'user-'||lpad(i::text,3,'0'),CASE WHEN i=235 THEN '历史%_用户' ELSE 'Same' END,i%2=0,now(),now() FROM generate_series(1,235) i").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT 'node-'||lpad(i::text,3,'0'),'Same','127.0.0.1',22,'root','private_key','unused','example.test',443,':443','hash-'||i,'unused','unused','{}',now(),now() FROM generate_series(1,235) i").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT 'group-001',id,now() FROM users WHERE id<>'user-001'").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT 'group-001',id,now() FROM nodes WHERE id<>'node-001'").execute(&state.pool).await.unwrap();
    state
}
async fn page(state: &AppState, query: Value) -> Value {
    list(
        State(state.clone()),
        Query(serde_json::from_value(query).unwrap()),
    )
    .await
    .unwrap()
    .0
}
async fn relation(state: &AppState, users: bool, query: Value) -> Value {
    members(
        state,
        "group-001",
        serde_json::from_value(query).unwrap(),
        users,
    )
    .await
    .unwrap()
    .0
}

#[tokio::test]
async fn directory_pages_are_stable_bounded_and_only_contain_metadata() {
    let state = fixture().await;
    let mut ids = Vec::new();
    for index in 1..=5 {
        let result = page(&state, json!({"page":index})).await;
        assert_eq!(result["total"], 235);
        assert_eq!(result["page_size"], 50);
        for item in result["items"].as_array().unwrap() {
            assert!(item.get("user_ids").is_none());
            assert!(item.get("node_ids").is_none());
            ids.push(item["id"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(ids.len(), 235);
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 235);
    assert_eq!(ids[0], "group-001");
    assert_eq!(ids[234], "group-235");
    let last = page(&state, json!({"page":i64::MAX})).await;
    assert_eq!(last["page"], 5);
    assert_eq!(last["items"].as_array().unwrap().len(), 35);
    for sort in ["name", "updated_at", "created_at"] {
        assert_eq!(
            page(&state, json!({"sort":sort,"order":"desc"})).await["items"][0]["id"],
            "group-235"
        );
    }
    for q in [" 历史%_ ", "GROUP-235"] {
        let result = page(&state, json!({"q":q})).await;
        assert_eq!(result["total"], 1);
    }
    let empty = page(&state, json!({"q":"' OR TRUE --","page":99})).await;
    assert_eq!(empty["page"], 1);
    assert_eq!(empty["total"], 0);
    let detail = get(State(state.clone()), Path("group-001".into()))
        .await
        .unwrap()
        .0;
    assert_eq!(detail["user_count"], 234);
    assert_eq!(detail["node_count"], 234);
    assert_eq!(
        detail,
        page(&state, json!({"q":"group-001"})).await["items"][0]
    );
    assert!(get(State(state), Path("missing".into())).await.is_err());
}

#[tokio::test]
async fn relation_pages_cover_members_and_candidates_without_hydrating_authorization_graphs() {
    let state = fixture().await;
    for users in [true, false] {
        let mut ids = Vec::new();
        for index in 1..=3 {
            let result = relation(&state, users, json!({"page":index,"page_size":100})).await;
            assert_eq!(result["revision"], 7);
            assert_eq!(result["total"], 234);
            for item in result["items"].as_array().unwrap() {
                assert!(item.get("assignments").is_none());
                assert!(item.get("ssh").is_none());
                assert!(item.get("node_ids").is_none());
                ids.push(item["id"].as_str().unwrap().to_owned());
            }
        }
        assert_eq!(ids.len(), 234);
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 234);
        let candidates = relation(&state, users, json!({"member":false})).await;
        assert_eq!(candidates["total"], 1);
        assert!(
            candidates["items"][0]["id"]
                .as_str()
                .unwrap()
                .ends_with("001")
        );
        let last = relation(&state, users, json!({"page":i64::MAX,"page_size":200})).await;
        assert_eq!(last["page"], 2);
        assert_eq!(last["items"].as_array().unwrap().len(), 34);
        assert_eq!(
            relation(&state, users, json!({"q":"' OR true --","page":99})).await["page"],
            1
        );
    }
    assert_eq!(
        relation(&state, true, json!({"enabled":true})).await["total"],
        117
    );
    assert_eq!(
        relation(&state, true, json!({"enabled":false})).await["total"],
        117
    );
    assert_eq!(
        relation(&state, true, json!({"q":" 历史%_ "})).await["total"],
        1
    );
    for (users, sort) in [(true, "id"), (true, "expires_at"), (false, "state")] {
        let result = relation(&state, users, json!({"sort":sort,"order":"desc"})).await;
        assert!(result["items"][0]["id"].as_str().unwrap().ends_with("235"));
    }
    assert!(
        members(&state, "missing", MembersQuery::default(), true)
            .await
            .is_err()
    );
    // Every page reads revision and entries from one snapshot on a one-connection pool.
    sqlx::query("UPDATE authorization_groups SET revision=8 WHERE id='group-001'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(relation(&state, true, json!({})).await["revision"], 8);
}

#[test]
fn invalid_parameters_cannot_reach_sql() {
    for value in [
        json!({"page":0}),
        json!({"page":-1}),
        json!({"page_size":0}),
        json!({"page_size":201}),
        json!({"sort":"name; DROP TABLE users"}),
        json!({"order":"ASC;--"}),
    ] {
        let query: ListQuery = serde_json::from_value(value).unwrap();
        assert!(query.validate(&["name"]).is_err());
    }
}

#[tokio::test]
async fn relation_query_is_decoded_over_http() {
    let state = fixture().await;
    let app = axum::Router::new()
        .route("/groups/{id}/users", axum::routing::get(list_users))
        .route("/groups/{id}/nodes", axum::routing::get(list_nodes))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .get(format!(
            "{base}/groups/group-001/users?member=false&enabled=false&page_size=1"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["items"][0]["id"],
        "user-001"
    );
    for path in [
        "users?member=no",
        "users?page_size=201",
        "nodes?enabled=true",
        "users?sort=state",
    ] {
        assert_eq!(
            client
                .get(format!("{base}/groups/group-001/{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    server.abort();
}

#[tokio::test]
async fn authenticated_pages_isolate_cache_keys_and_invalidate_member_metadata() {
    let state = fixture().await;
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('pagination-admin',$1,'Pagination',now())")
        .bind(token_digest("pagination-admin")).execute(&state.pool).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/api/v2/authorization-groups",
        listener.local_addr().unwrap()
    );
    let app = crate::api::router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let get = |suffix: &str| {
        client
            .get(format!("{base}{suffix}"))
            .bearer_auth("pagination-admin")
    };
    let first = get("?page_size=1").send().await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        first.json::<Value>().await.unwrap()["items"][0]["id"],
        "group-001"
    );
    assert_eq!(
        get("?page_size=1&page=2")
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["items"][0]["id"],
        "group-002"
    );
    let hits = state.caches.responses.stats()["hits"].as_u64().unwrap();
    assert_eq!(
        get("?page_size=1").send().await.unwrap().status(),
        StatusCode::OK
    );
    assert!(state.caches.responses.stats()["hits"].as_u64().unwrap() > hits);
    let path = "/group-001/users?q=user-002&enabled=true";
    assert_eq!(
        get(path)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["total"],
        1
    );
    sqlx::query("UPDATE users SET name='Renamed',enabled=false WHERE id='user-002'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        get(path)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["total"],
        0
    );
    assert_eq!(
        get("/group-001/users?q=user-002")
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["items"][0]["name"],
        "Renamed"
    );
    let nodes = "/group-001/nodes?q=node-002";
    get(nodes).send().await.unwrap();
    sqlx::query("UPDATE nodes SET state='deployed' WHERE id='node-002'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        get(nodes)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap()["items"][0]["state"],
        "deployed"
    );
    server.abort();
}

#[tokio::test]
async fn hundred_thousand_users_ten_thousand_nodes_still_return_bounded_pages() {
    let state = fixture().await;
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) SELECT 'scale-user-'||i,'Scale '||i,now(),now() FROM generate_series(1,99765) i")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT 'scale-node-'||i,'Scale '||i,'127.0.0.1',22,'root','private_key','unused','example.test',443,':443','scale-hash-'||i,'unused','unused','{}',now(),now() FROM generate_series(1,9765) i")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT 'group-002',id,now() FROM users")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT 'group-002',id,now() FROM nodes")
        .execute(&state.pool).await.unwrap();
    let detail = get(State(state.clone()), Path("group-002".into()))
        .await
        .unwrap()
        .0;
    assert_eq!(detail["user_count"], 100000);
    assert_eq!(detail["node_count"], 10000);
    assert!(serde_json::to_vec(&detail).unwrap().len() < 1024);
    for (users, total) in [(true, 100000), (false, 10000)] {
        let value = members(
            &state,
            "group-002",
            serde_json::from_value(json!({"page_size":200,"page":i64::MAX})).unwrap(),
            users,
        )
        .await
        .unwrap()
        .0;
        assert_eq!(value["total"], total);
        assert_eq!(value["items"].as_array().unwrap().len(), 200);
        assert!(serde_json::to_vec(&value).unwrap().len() < 64000);
    }
}
