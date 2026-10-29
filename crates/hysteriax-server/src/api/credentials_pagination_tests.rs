use super::*;

async fn fixture() -> AppState {
    fixture_with_connections(1).await
}

async fn fixture_with_connections(connections: u32) -> AppState {
    let pool = crate::db::test_pool_with_max_connections(connections).await;
    let state = AppState::new(
        pool,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) VALUES('user','User',false,now(),now())").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO credentials(id,name,kind,owner_user_id,archived,created_at,updated_at) SELECT CASE WHEN i=1 THEN '00000000-0000-4000-8000-000000000001' ELSE 'credential-'||lpad(i::text,3,'0') END,CASE WHEN i=235 THEN '历史%_凭据' ELSE 'Same' END,'ssh_password',CASE WHEN i>200 THEN 'user' ELSE NULL END,i=1,now(),now() FROM generate_series(1,235) i")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) SELECT id,1,'PRIVATE-PAYLOAD-MUST-NOT-LEAK','{}',now() FROM credentials").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,revoked_at) VALUES('token','PRIVATE-TOKEN-HASH','Admin',now(),now())").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO subscription_credentials(id,user_id,token_hash,token_enc,created_at) VALUES('subscription','user','PRIVATE-SUBSCRIPTION-HASH','PRIVATE-SUBSCRIPTION-MUST-NOT-LEAK',now())").execute(&state.pool).await.unwrap();
    let config = json!({"tls":{"certificate":"credential://00000000-0000-4000-8000-000000000001/1/password"}});
    let cipher = state.secrets.encrypt(&config.to_string()).unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,deployed_config_enc,ssh_credential_id,ssh_credential_version,created_at,updated_at) VALUES('node','Node','127.0.0.1',22,'root','private_key','unused','example.test',443,':443','hash','unused','unused',$1,$1,'00000000-0000-4000-8000-000000000001',1,now(),now())")
        .bind(cipher).execute(&state.pool).await.unwrap();
    sqlx::query("WITH assignment_fixture(user_id,node_id,credential_hash,credential_enc,created_at) AS MATERIALIZED (VALUES('user','node','PRIVATE-USER-HASH','PRIVATE-USER-MUST-NOT-LEAK',now())), password_fixture AS (UPDATE users u SET connection_credential_hash=c.credential_hash,connection_credential_enc=c.credential_enc FROM (SELECT DISTINCT ON(user_id) user_id,credential_hash,credential_enc FROM assignment_fixture ORDER BY user_id) c WHERE u.id=c.user_id), fixture_groups AS (INSERT INTO authorization_groups(id,name,created_at,updated_at) SELECT 'fixture-'||md5(user_id||chr(31)||node_id),'Fixture authorization',now(),now() FROM assignment_fixture ON CONFLICT(id) DO UPDATE SET name=EXCLUDED.name RETURNING id), fixture_members AS (INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT g.id,a.user_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING), fixture_nodes AS (INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT g.id,a.node_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING) INSERT INTO node_assignments(user_id,node_id,created_at) SELECT user_id,node_id,created_at FROM assignment_fixture").execute(&state.pool).await.unwrap();
    crate::credentials::reference_index::initialize(&state.pool, &state.secrets)
        .await
        .unwrap();
    state
}

#[tokio::test]
async fn clean_credential_reads_keep_a_committed_snapshot_during_writes() {
    let state = fixture_with_connections(4).await;
    crate::credentials::reference_index::initialize(&state.pool, &state.secrets)
        .await
        .unwrap();
    let mut writer = state.pool.begin().await.unwrap();
    // An uncommitted configuration is invisible together with its dirty marker.
    sqlx::query("UPDATE nodes SET name='Uncommitted' WHERE id='node'")
        .execute(&mut *writer)
        .await
        .unwrap();
    let id = "00000000-0000-4000-8000-000000000001";
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        assert_eq!(page(&state, json!({})).await["total"], 238);
        let detail = get(State(state.clone()), Path(id.to_owned()))
            .await
            .unwrap()
            .0;
        assert!(!detail["references"].as_array().unwrap().is_empty());
        let refs = references(State(state.clone()), Path(id.to_owned()))
            .await
            .unwrap()
            .0;
        assert!(!refs.is_empty());
    })
    .await
    .expect("clean GETs must complete while another connection holds the write lock");
    writer.rollback().await.unwrap();
}

#[tokio::test]
async fn transactional_reference_updates_keep_read_snapshots_consistent() {
    let state = fixture_with_connections(3).await;
    let id = "00000000-0000-4000-8000-000000000001";
    let detail = get(State(state.clone()), Path(id.to_owned()))
        .await
        .unwrap()
        .0;
    assert!(
        detail["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["source"] == "desired")
    );
    let mut snapshot = crate::credentials::reference_index::begin_read(&state.pool, &state.secrets)
        .await
        .unwrap();
    let cipher = state.secrets.encrypt("{}").unwrap();
    let mut source_tx = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE nodes SET desired_config_enc=$1,deployed_config_enc=$1 WHERE id='node'")
        .bind(cipher)
        .execute(&mut *source_tx)
        .await
        .unwrap();
    crate::credentials::reference_index::flush_node_on(&mut source_tx, &state.secrets, "node")
        .await
        .unwrap();
    source_tx.commit().await.unwrap();
    let old = read_references_on(&mut snapshot, id).await.unwrap();
    assert!(old.iter().any(|r| r["source"] == "desired"));
    snapshot.commit().await.unwrap();
    let current = find_references(&state, id).await.unwrap();
    assert!(
        !current
            .iter()
            .any(|r| r["source"] == "desired" || r["source"] == "deployed")
    );
    assert!(current.iter().any(|r| r["source"] == "ssh"));
}

async fn page(state: &AppState, query: Value) -> Value {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        list(
            State(state.clone()),
            Query(serde_json::from_value(query).unwrap()),
        ),
    )
    .await
    .expect("single-connection aggregation must not deadlock")
    .unwrap()
    .0
}

#[tokio::test]
async fn credentials_pagination_covers_mixed_catalog_and_stable_ties() {
    let state = fixture().await;
    let first = page(&state, json!({})).await;
    assert_eq!(first["total"], 238);
    assert_eq!(first["page_size"], 50);
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
    assert_eq!(ids.len(), 238);
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        238
    );
    assert!(ids.contains(&"admin:token".to_owned()));
    assert!(ids.contains(&"subscription:subscription".to_owned()));
    assert!(ids.contains(&"user:user".to_owned()));
    let last = page(&state, json!({"page":i64::MAX})).await;
    assert_eq!(last["page"], 5);
    assert_eq!(last["items"].as_array().unwrap().len(), 38);
    let reverse = page(&state, json!({"order":"desc","page_size":25})).await;
    assert_eq!(
        reverse["items"]
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
    for sort in ["kind", "status", "created_at"] {
        assert_eq!(page(&state, json!({"sort":sort})).await["total"], 238);
    }
}

#[tokio::test]
async fn credentials_pagination_filters_categories_preserves_metadata_and_never_exposes_secrets() {
    let state = fixture().await;
    assert_eq!(
        page(&state, json!({"category":"operations"})).await["total"],
        201
    );
    assert_eq!(page(&state, json!({"category":"user"})).await["total"], 37);
    assert_eq!(
        page(&state, json!({"category":"user","kind":"ssh_password"})).await["total"],
        35
    );
    let search = page(&state, json!({"q":" 历史%_ "})).await;
    assert_eq!(search["total"], 1);
    assert_eq!(search["items"][0]["id"], "credential-235");
    let all = page(&state, json!({"page_size":200,"category":"operations"})).await;
    assert!(!all.to_string().contains("PRIVATE-"));
    let archived = page(
        &state,
        json!({"q":"same","kind":"ssh_password","page_size":200}),
    )
    .await;
    let credential = archived["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == "00000000-0000-4000-8000-000000000001")
        .unwrap();
    assert_eq!(credential["archived"], true);
    assert_eq!(credential["status"], "archived");
    assert_eq!(
        credential["reference_count"], 1,
        "SSH, target config and deployed config on the same node count once"
    );
    sqlx::query("UPDATE credentials SET created_at=CASE WHEN id='credential-002' THEN '2000-01-01T00:00:00Z'::timestamptz ELSE '2000-01-01T00:00:00.5Z'::timestamptz END WHERE id IN ('credential-002','credential-003')")
        .execute(&state.pool).await.unwrap();
    let chronological = page(&state, json!({"sort":"created_at"})).await;
    assert_eq!(chronological["items"][0]["id"], "credential-002");
    assert_eq!(chronological["items"][1]["id"], "credential-003");
    let Json(catalog) = list(State(state.clone()), Query(CredentialsListQuery::default()))
        .await
        .unwrap();
    assert_eq!(catalog["total"], 238);
    let catalog = catalog["items"].as_array().unwrap();
    assert_eq!(catalog.len(), 50);
    let expected = catalog
        .iter()
        .find(|entry| entry["id"] == "00000000-0000-4000-8000-000000000001")
        .unwrap();
    assert_eq!(credential, expected);
    let token = page(&state, json!({"kind":"admin_token"})).await;
    assert_eq!(token["total"], 1);
    assert_eq!(token["items"][0]["status"], "revoked");
    let user = page(&state, json!({"kind":"user_credential"})).await;
    assert_eq!(user["total"], 1);
    assert_eq!(user["items"][0]["status"], "disabled");
    assert!(!user.to_string().contains("PRIVATE-"));
    let empty = page(&state, json!({"q":"' OR true --","page":200})).await;
    assert_eq!(empty["total"], 0);
    assert_eq!(empty["page"], 1);
    assert_eq!(empty["items"], json!([]));
    assert_eq!(
        page(&state, json!({"category":"user","kind":"admin_token"})).await["total"],
        0
    );
}

#[test]
fn credentials_pagination_rejects_invalid_parameters() {
    for query in [
        json!({"page":0}),
        json!({"page":-1}),
        json!({"page_size":0}),
        json!({"page_size":201}),
        json!({"category":"unknown"}),
        json!({"sort":"payload_enc"}),
        json!({"order":"ASC;--"}),
    ] {
        let query: CredentialsListQuery = serde_json::from_value(query).unwrap();
        assert!(query.validate().is_err());
    }
}

#[tokio::test]
async fn directory_status_order_matches_live_summaries_and_bad_expiry_falls_back() {
    let state = fixture().await;
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) SELECT id,2,'unused',jsonb_build_object('expires_at',CASE WHEN id='credential-002' THEN 'invalid' WHEN id='credential-003' THEN to_char(now()-interval '1 day','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') WHEN id='credential-004' THEN to_char(now()+interval '8 days','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') ELSE NULL END),now() FROM credentials WHERE id IN ('credential-002','credential-003','credential-004')")
        .execute(&state.pool).await.unwrap();
    sqlx::query(
        "UPDATE credentials SET reminder_at=now()+interval '10 days' WHERE id='credential-002'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let result = page(
        &state,
        json!({"sort":"status","page_size":200,"category":"operations"}),
    )
    .await;
    let entries = result["items"].as_array().unwrap();
    let statuses = entries
        .iter()
        .map(|v| v["status"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(statuses.windows(2).all(|pair| pair[0] <= pair[1]));
    let bad = page(
        &state,
        json!({"q":"Same","kind":"ssh_password","page_size":200}),
    )
    .await;
    let fallback = bad["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "credential-002")
        .unwrap();
    assert_eq!(fallback["status"], "expiring");
    assert!(fallback["expires_at"].is_string());
}

#[tokio::test]
async fn user_credentials_are_stable_per_user_with_zero_or_many_nodes_and_rotation() {
    let state = fixture().await;
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT 'extra-'||i,'Extra '||i,'127.0.0.1',22,'root','password','unused','example.test',443,':443','node-hash-'||i,'unused','unused',$1,now(),now() FROM generate_series(1,1000) i")
        .bind(state.secrets.encrypt("{}").unwrap()).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,created_at) SELECT 'user',id,now() FROM nodes WHERE id LIKE 'extra-%'")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO users(id,name,enabled,connection_credential_hash,connection_credential_enc,created_at,updated_at) VALUES('unassigned','No nodes',true,'unassigned-hash','PRIVATE-UNASSIGNED-PASSWORD',now(),now())")
        .execute(&state.pool).await.unwrap();
    crate::credentials::reference_index::initialize(&state.pool, &state.secrets)
        .await
        .unwrap();
    let result = page(&state, json!({"category":"user","kind":"user_credential"})).await;
    assert_eq!(result["total"], 2);
    assert_eq!(result["items"].as_array().unwrap().len(), 2);
    let current = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "user:user")
        .unwrap();
    assert_eq!(current["name"], "User");
    assert_eq!(current["reference_count"], 1001);
    assert_eq!(current["metadata"], json!({}));
    assert_eq!(current["owner_user_id"], "user");
    assert!(
        result.to_string().len() < 2000,
        "node fanout must not expand list responses"
    );
    assert!(!result.to_string().contains("PRIVATE-"));
    let empty = page(&state, json!({"kind":"user_credential","q":"No nodes"})).await;
    assert_eq!(empty["total"], 1);
    assert_eq!(empty["items"][0]["reference_count"], 0);
    assert_eq!(empty["items"][0]["status"], "active");
    let first = page(&state, json!({"kind":"user_credential","page_size":1})).await;
    let second = page(
        &state,
        json!({"kind":"user_credential","page_size":1,"page":2}),
    )
    .await;
    assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
    assert_eq!(second["page"], 2);
    // A password rotation changes the user revision, never the catalog identity.
    let _rotation = super::super::users::rotate_credentials(
        State(state.clone()),
        Path("user".into()),
        Json(serde_json::from_value(json!({"expected_revision":1})).unwrap()),
    )
    .await
    .unwrap();
    let rotated = page(&state, json!({"kind":"user_credential","q":"User"})).await;
    assert_eq!(rotated["total"], 1);
    assert_eq!(rotated["items"][0]["id"], "user:user");
    assert_eq!(rotated["items"][0]["revision"], 2);
    assert_eq!(rotated["items"][0]["reference_count"], 1001);
    sqlx::query("DELETE FROM node_assignments WHERE user_id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE users SET name='Renamed',enabled=true,quota_bytes=1,usage_bytes=1 WHERE id='user'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let renamed = page(&state, json!({"kind":"user_credential","q":"Renamed"})).await;
    assert_eq!(renamed["items"][0]["id"], "user:user");
    assert_eq!(renamed["items"][0]["reference_count"], 0);
    assert_eq!(renamed["items"][0]["status"], "quota_exhausted");
}

#[tokio::test]
async fn directory_upgrade_replaces_node_entries_without_changing_passwords_or_assignments() {
    let state = fixture().await;
    let baseline = include_str!("../../migrations/0001_baseline.sql");
    let old_view = baseline
        .split("CREATE VIEW credential_directory AS")
        .nth(1)
        .unwrap()
        .split("-- TABLE: credential_migrations")
        .next()
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE OR REPLACE VIEW credential_directory AS{old_view}"
    )))
    .execute(&state.pool)
    .await
    .unwrap();
    let legacy: String =
        sqlx::query_scalar("SELECT id FROM credential_directory WHERE kind='user_credential'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(legacy, "user:user:node");
    let before: (String, String) = sqlx::query_as(
        "SELECT connection_credential_hash,connection_credential_enc FROM users WHERE id='user'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
        "../../migrations/0006_user_credential_directory.sql"
    ))
    .execute(&state.pool)
    .await
    .unwrap();
    let after: (String, String) = sqlx::query_as(
        "SELECT connection_credential_hash,connection_credential_enc FROM users WHERE id='user'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    assert_eq!(page(&state, json!({})).await["total"], 238);
    let current = page(&state, json!({"kind":"user_credential"})).await;
    assert_eq!(current["items"][0]["id"], "user:user");
    assert_eq!(current["items"][0]["reference_count"], 1);
    sqlx::raw_sql(include_str!(
        "../../migrations/0006_user_credential_directory.sql"
    ))
    .execute(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        page(&state, json!({"category":"operations"})).await["total"],
        201
    );
}
