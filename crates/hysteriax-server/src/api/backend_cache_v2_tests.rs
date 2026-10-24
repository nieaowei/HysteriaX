use super::*;

async fn server() -> (AppState, String, tokio::task::JoinHandle<()>) {
    let pool = db::test_pool_with_max_connections(8).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    crate::credentials::migration::migrate(&pool, &secrets)
        .await
        .unwrap();
    crate::credentials::reference_index::initialize(&pool, &secrets)
        .await
        .unwrap();
    subscriptions::templates::initialize(&pool, &secrets)
        .await
        .unwrap();
    sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at,last_used_at) VALUES('cache-operator',$1,'Cache operator',clock_timestamp(),clock_timestamp())")
        .bind(token_digest("cache-admin-v2")).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES('user','Before',clock_timestamp(),clock_timestamp())").execute(&pool).await.unwrap();
    let state = AppState::new(pool, secrets);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(state.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (state, base, task)
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}
async fn get(client: &reqwest::Client, base: &str, path: &str) -> Value {
    let response = client
        .get(format!("{base}{path}"))
        .bearer_auth("cache-admin-v2")
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    serde_json::from_str(&body).unwrap()
}

#[tokio::test]
async fn v2_reads_are_authenticated_and_remain_available_with_the_same_business_projection() {
    let (_, base, server) = server().await;
    let client = client();
    for path in [
        "/client/summary",
        "/overview",
        "/overview/nodes",
        "/overview/history?range=24h&timezone=UTC",
        "/server/monitoring",
        "/audit",
        "/cache",
    ] {
        assert_eq!(
            client
                .get(format!("{base}/api/v2{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let value = get(&client, &base, &format!("/api/v2{path}")).await;
        if path == "/client/summary" {
            assert_eq!(value["user_total"], 1);
        }
    }
    server.abort();
}

#[tokio::test]
async fn protocol_namespaces_keep_separate_cache_entries_and_committed_versions_invalidate_both() {
    let (state, base, server) = server().await;
    let client = client();
    for prefix in ["v1", "v2"] {
        assert_eq!(
            get(&client, &base, &format!("/api/{prefix}/users")).await["items"][0]["name"],
            "Before"
        );
    }
    assert_eq!(state.caches.responses.stats()["entries"], 2);
    let before = state.caches.responses.stats()["hits"].as_u64().unwrap();
    for prefix in ["v1", "v2"] {
        get(&client, &base, &format!("/api/{prefix}/users")).await;
    }
    assert_eq!(
        state.caches.responses.stats()["hits"].as_u64().unwrap(),
        before + 2
    );
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE users SET name='Rolled back' WHERE id='user'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        get(&client, &base, "/api/v2/users").await["items"][0]["name"],
        "Before"
    );
    sqlx::query("UPDATE users SET name='Committed' WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    for prefix in ["v1", "v2"] {
        assert_eq!(
            get(&client, &base, &format!("/api/{prefix}/users")).await["items"][0]["name"],
            "Committed"
        );
    }
    sqlx::query("UPDATE admin_tokens SET revoked_at=clock_timestamp() WHERE id='cache-operator'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/api/v2/users"))
            .bearer_auth("cache-admin-v2")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    server.abort();
}

#[tokio::test]
async fn v2_sensitive_templates_tokens_subscriptions_and_heads_never_fill_the_response_cache() {
    let (state, base, server) = server().await;
    let client = client();
    let paths = [
        "/subscription-templates",
        "/subscription-templates/default-mihomo",
        "/subscription-templates/default-mihomo/draft",
        "/subscription-templates/default-mihomo/export",
        "/subscription-templates/default-mihomo/versions/1",
        "/admin/tokens",
        "/events/head",
        "/users/user/subscription",
    ];
    for _ in 0..2 {
        for path in paths {
            get(&client, &base, &format!("/api/v2{path}")).await;
        }
    }
    assert_eq!(state.caches.responses.stats()["entries"], 0);
    assert_eq!(state.caches.responses.stats()["hits"], 0);
    server.abort();
}

#[tokio::test]
async fn v2_directory_cache_ignores_usage_and_unrelated_audits_but_sees_group_changes() {
    let (state, base, server) = server().await;
    let client = client();
    let path = "/api/v2/authorization-groups";
    assert!(
        get(&client, &base, path)
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    sqlx::query("UPDATE users SET usage_bytes=100,updated_at=clock_timestamp() WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES('unrelated','admin','test','user','user','{}',clock_timestamp())").execute(&state.pool).await.unwrap();
    let hits = state.caches.responses.stats()["hits"].as_u64().unwrap();
    get(&client, &base, path).await;
    assert_eq!(
        state.caches.responses.stats()["hits"].as_u64().unwrap(),
        hits + 1
    );
    sqlx::query("INSERT INTO authorization_groups(id,name,created_at,updated_at) VALUES('group','Committed',clock_timestamp(),clock_timestamp())").execute(&state.pool).await.unwrap();
    assert_eq!(get(&client, &base, path).await[0]["name"], "Committed");
    server.abort();
}

#[tokio::test]
async fn v2_terminal_job_cache_survives_unrelated_jobs_and_tracks_retry_links() {
    let (state, base, server) = server().await;
    let client = client();
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at,finished_at) VALUES('terminal','sync','failed','failed',now(),now(),now(),now())").execute(&state.pool).await.unwrap();
    let path = "/api/v2/jobs/terminal";
    assert!(get(&client, &base, path).await["job"]["retry_job_id"].is_null());
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('other','sync','queued','queued',now(),now(),now())").execute(&state.pool).await.unwrap();
    let hits = state.caches.responses.stats()["hits"].as_u64().unwrap();
    get(&client, &base, path).await;
    assert_eq!(
        state.caches.responses.stats()["hits"].as_u64().unwrap(),
        hits + 1
    );
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,retry_of_job_id,available_at,created_at,updated_at) VALUES('retry','sync','queued','queued','terminal',now(),now(),now())").execute(&state.pool).await.unwrap();
    assert_eq!(
        get(&client, &base, path).await["job"]["retry_job_id"],
        "retry"
    );
    server.abort();
}
