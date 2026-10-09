use super::*;

async fn fixture() -> AppState {
    let pool = crate::db::test_pool().await;
    let state = AppState::new(
        pool,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    let mut tx = state.pool.begin().await.unwrap();
    let credential = crate::credentials::insert(
        &mut tx,
        &state.secrets,
        "SSH",
        "ssh_password",
        None,
        &json!({"secret":"NODE-PASSWORD-MUST-NOT-LEAK"}),
        &json!({}),
        &json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let cipher = state.secrets.encrypt("{}").unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,ssh_credential_id,ssh_credential_version,desired_revision,deployed_revision,state,last_sample_at,created_at,updated_at) SELECT 'node-'||lpad(i::text,3,'0'),CASE WHEN i=235 THEN '历史%_节点' ELSE 'Same' END,'host-'||lpad(i::text,3,'0')||'.example.test',22,'root','password','unused','example.test',443,':443','hash-'||i,'unused','unused',$1,$2,1,i,i-1,CASE WHEN i=235 THEN 'unreachable' WHEN i=2 THEN 'sync_failed' ELSE 'deployed' END,'2000-01-01'::timestamptz,now(),now() FROM generate_series(1,235) i")
        .bind(cipher).bind(credential).execute(&state.pool).await.unwrap();
    state
}

async fn page(state: &AppState, query: Value) -> Value {
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        list_page(
            State(state.clone()),
            Query(serde_json::from_value(query).unwrap()),
        ),
    )
    .await
    .expect("node hydration must release the page transaction before acquiring connections")
    .unwrap()
    .0
}

#[tokio::test]
async fn nodes_pagination_covers_all_nodes_with_stable_names_and_sorting() {
    let state = fixture().await;
    let first = page(&state, json!({})).await;
    assert_eq!(first["total"], 235);
    assert_eq!(first["page_size"], 50);
    assert_eq!(first["items"][0]["id"], "node-001");
    let mut ids = Vec::new();
    for n in 1..=5 {
        let result = page(&state, json!({"page":n})).await;
        ids.extend(
            result["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["id"].as_str().unwrap().to_owned()),
        );
    }
    assert_eq!(ids.len(), 235);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        235
    );
    assert_eq!(ids.last().unwrap(), "node-235");
    let last = page(&state, json!({"page":i64::MAX})).await;
    assert_eq!(last["page"], 5);
    assert_eq!(last["items"].as_array().unwrap().len(), 35);
    let reversed = page(&state, json!({"order":"desc","page_size":25})).await;
    assert_eq!(
        reversed["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ids.iter()
            .rev()
            .take(25)
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        page(
            &state,
            json!({"sort":"ssh_host","order":"desc","page_size":1})
        )
        .await["items"][0]["id"],
        "node-235"
    );
    for sort in ["state", "created_at"] {
        assert_eq!(
            page(&state, json!({"sort":sort,"page_size":1})).await["total"],
            235
        );
    }
    let Json(catalog) = list(State(state.clone())).await.unwrap();
    assert_eq!(
        catalog.len(),
        235,
        "all node consumers keep the complete catalog"
    );
}

#[tokio::test]
async fn nodes_pagination_searches_labels_hosts_ids_and_preserves_details() {
    let state = fixture().await;
    for query in [
        json!({"q":" 历史%_ "}),
        json!({"q":"NODE-235"}),
        json!({"q":"HOST-235.EXAMPLE"}),
        json!({"q":"无法连接","state_matches":"unreachable"}),
    ] {
        let result = page(&state, query).await;
        assert_eq!(result["total"], 1);
        let node = &result["items"][0];
        assert_eq!(node["id"], "node-235");
        assert_eq!(node["revision"], 235);
        assert_eq!(node["deployed_revision"], 234);
        assert_eq!(node["ssh"]["host"], "host-235.example.test");
        assert_eq!(node["ssh"]["auth_type"], "password");
        assert_eq!(node["data_freshness"], "stale");
        assert!(node["package"].is_object() && node["package_usage"].is_object());
        assert!(node["yaml_preview"].is_string() && node["config"].is_object());
        assert!(!node.to_string().contains("NODE-PASSWORD-MUST-NOT-LEAK"));
        let Json(detail) = get(State(state.clone()), Path("node-235".into()))
            .await
            .unwrap();
        assert_eq!(node, &detail);
    }
    let status = page(
        &state,
        json!({"q":"同步失败","state_matches":"sync_failed"}),
    )
    .await;
    assert_eq!(status["total"], 1);
    assert_eq!(status["items"][0]["id"], "node-002");
    let empty = page(
        &state,
        json!({"q":"' OR true --","state_matches":"deployed'); DROP TABLE nodes;--","page":100}),
    )
    .await;
    assert_eq!(empty["total"], 0);
    assert_eq!(empty["page"], 1);
    assert_eq!(empty["items"], json!([]));
    sqlx::query("DELETE FROM nodes")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(page(&state, json!({"page":2})).await["total"], 0);
}

#[test]
fn nodes_pagination_rejects_invalid_parameters() {
    for value in [
        json!({"page":0}),
        json!({"page":-1}),
        json!({"page_size":0}),
        json!({"page_size":201}),
        json!({"sort":"ssh_secret_enc"}),
        json!({"order":"DESC;--"}),
    ] {
        let query: NodesPageQuery = serde_json::from_value(value).unwrap();
        assert!(query.validate().is_err());
    }
}
