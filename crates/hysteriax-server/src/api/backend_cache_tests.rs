use super::*;

#[tokio::test]
async fn subscription_cache_checks_quota_and_revocation_on_every_request() {
    let (state, base, task) = server().await;
    subscriptions::templates::initialize(&state.pool, &state.secrets)
        .await
        .unwrap();
    let config = state.secrets.encrypt("{}").unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,deployed_config_enc,deployed_revision,state,created_at,updated_at) VALUES('node','Node','localhost',22,'root','password','unused','cached.example.test',443,':443','node-hash','unused','unused',$1,$1,1,'deployed',now(),now())")
        .bind(config).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,created_at) VALUES('user','node','assignment',$1,now())")
        .bind(state.secrets.encrypt("password").unwrap()).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO subscription_credentials(id,user_id,token_hash,token_enc,created_at) VALUES('subscription','user',$1,$2,now())")
        .bind(token_digest("subscription-token")).bind(state.secrets.encrypt("subscription-token").unwrap()).execute(&state.pool).await.unwrap();
    let client = reqwest::Client::new();
    let url = format!("{base}/sub/subscription-token?template=default-mihomo&platform=linux");
    for _ in 0..2 {
        let response = client.get(&url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .text()
                .await
                .unwrap()
                .contains("cached.example.test")
        );
    }
    assert!(state.caches.responses.stats()["hits"].as_u64().unwrap() > 0);
    sqlx::query("UPDATE users SET quota_bytes=10 WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    client.get(&url).send().await.unwrap();
    let generation = crate::cache::configuration_generation(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET usage_bytes=10 WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        crate::cache::configuration_generation(&state.pool)
            .await
            .unwrap(),
        generation
    );
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE users SET usage_bytes=0 WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        StatusCode::OK
    );
    sqlx::query("UPDATE subscription_credentials SET revoked_at=now() WHERE id='subscription'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    task.abort();
}

async fn server() -> (AppState, String, tokio::task::JoinHandle<()>) {
    let state = AppState::new(
        crate::db::test_pool().await,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    for id in ["admin-a", "admin-b"] {
        sqlx::query(
            "INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES($1,$2,$1,now())",
        )
        .bind(id)
        .bind(token_digest(id))
        .execute(&state.pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO users(id,name,created_at,updated_at) VALUES('user','Before',now(),now())",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(state.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (state, base, task)
}
#[tokio::test]
async fn authenticated_cache_invalidation_rollback_refresh_and_revocation() {
    let (state, base, task) = server().await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v2/users");
    let get = |token: &str| client.get(&url).bearer_auth(token);
    let first: Value = get("admin-a").send().await.unwrap().json().await.unwrap();
    assert_eq!(first["items"][0]["name"], "Before");
    let used: DateTime<Utc> =
        sqlx::query_scalar("SELECT last_used_at FROM admin_tokens WHERE id='admin-a'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    for _ in 0..3 {
        assert_eq!(
            get("admin-a").send().await.unwrap().status(),
            StatusCode::OK
        );
    }
    assert!(state.caches.responses.stats()["hits"].as_u64().unwrap() >= 3);
    assert_eq!(
        used,
        sqlx::query_scalar::<_, DateTime<Utc>>(
            "SELECT last_used_at FROM admin_tokens WHERE id='admin-a'"
        )
        .fetch_one(&state.pool)
        .await
        .unwrap()
    );
    get("admin-b").send().await.unwrap();
    assert_eq!(state.caches.responses.stats()["entries"], 2);
    sqlx::query("UPDATE users SET name='Committed' WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    let changed: Value = get("admin-a").send().await.unwrap().json().await.unwrap();
    assert_eq!(changed["items"][0]["name"], "Committed");
    let generation = crate::cache::generation(&state.pool).await.unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE users SET name='Rolled back' WHERE id='user'")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        crate::cache::generation(&state.pool).await.unwrap(),
        generation
    );
    let misses = state.caches.responses.stats()["misses"].as_u64().unwrap();
    get("admin-a")
        .header("Cache-Control", "NO-CACHE")
        .send()
        .await
        .unwrap();
    assert!(state.caches.responses.stats()["misses"].as_u64().unwrap() > misses);
    sqlx::query("UPDATE admin_tokens SET revoked_at=now() WHERE id='admin-a'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        get("admin-a").send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get("admin-b").send().await.unwrap().status(),
        StatusCode::OK
    );
    task.abort();
}
#[tokio::test]
async fn static_etag_and_configuration_generation_ignore_sampling_churn() {
    let (state, base, task) = server().await;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{base}/openapi.yaml"))
        .send()
        .await
        .unwrap();
    let etag = response.headers()[header::ETAG].clone();
    assert_eq!(
        client
            .get(format!("{base}/openapi.yaml"))
            .header(header::IF_NONE_MATCH, etag)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_MODIFIED
    );
    let generation = crate::cache::configuration_generation(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET usage_bytes=usage_bytes+10,updated_at=now() WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(
        crate::cache::configuration_generation(&state.pool)
            .await
            .unwrap(),
        generation
    );
    sqlx::query("UPDATE users SET enabled=false WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(
        crate::cache::configuration_generation(&state.pool)
            .await
            .unwrap()
            > generation
    );
    task.abort();
}

#[tokio::test]
async fn directory_cache_ignores_sampling_and_tracks_membership_changes() {
    let (state, base, task) = server().await;
    let client = reqwest::Client::new();
    let get = || {
        client
            .get(format!("{base}/api/v2/authorization-groups"))
            .bearer_auth("admin-a")
    };
    assert!(
        get()
            .send()
            .await
            .unwrap()
            .json::<Vec<Value>>()
            .await
            .unwrap()
            .is_empty()
    );
    let dependencies = crate::cache_domains::dependencies("/api/v2/authorization-groups", &[]);
    let before = crate::cache_domains::version(&state.pool, &dependencies)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET usage_bytes=100,updated_at=now() WHERE id='user'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at) VALUES('domain-audit','admin','test','user','user','{}',now())").execute(&state.pool).await.unwrap();
    assert_eq!(
        before,
        crate::cache_domains::version(&state.pool, &dependencies)
            .await
            .unwrap()
    );
    let hits = state.caches.responses.stats()["hits"].as_u64().unwrap();
    get().send().await.unwrap();
    assert!(state.caches.responses.stats()["hits"].as_u64().unwrap() > hits);
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO authorization_groups(id,name,created_at,updated_at) VALUES('rolled-back','Temporary',now(),now())").execute(&mut *tx).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        before,
        crate::cache_domains::version(&state.pool, &dependencies)
            .await
            .unwrap()
    );
    sqlx::query("INSERT INTO authorization_groups(id,name,created_at,updated_at) VALUES('group','Committed',now(),now())").execute(&state.pool).await.unwrap();
    let response = get()
        .send()
        .await
        .unwrap()
        .json::<Vec<Value>>()
        .await
        .unwrap();
    assert_eq!(response[0]["name"], "Committed");
    assert!(
        state.caches.stats()["routes"]["/api/v2/authorization-groups"]["reused"]
            .as_u64()
            .unwrap()
            > 0
    );
    task.abort();
}

#[tokio::test]
async fn terminal_job_cache_survives_unrelated_jobs_and_invalidates_on_retry() {
    let (state, base, task) = server().await;
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at,finished_at) VALUES('terminal','sync','failed','failed',now(),now(),now(),now())").execute(&state.pool).await.unwrap();
    let client = reqwest::Client::new();
    let get = || {
        client
            .get(format!("{base}/api/v2/jobs/terminal"))
            .bearer_auth("admin-a")
    };
    let first: Value = get().send().await.unwrap().json().await.unwrap();
    assert!(first["job"]["retry_job_id"].is_null());
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('other','sync','queued','queued',now(),now(),now())").execute(&state.pool).await.unwrap();
    let hits = state.caches.responses.stats()["hits"].as_u64().unwrap();
    get().send().await.unwrap();
    assert!(state.caches.responses.stats()["hits"].as_u64().unwrap() > hits);
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,retry_of_job_id,available_at,created_at,updated_at) VALUES('retry','sync','queued','queued','terminal',now(),now(),now())").execute(&state.pool).await.unwrap();
    let retried: Value = get().send().await.unwrap().json().await.unwrap();
    assert_eq!(retried["job"]["retry_job_id"], "retry");
    task.abort();
}

#[tokio::test]
async fn revoked_administrator_loses_existing_global_and_job_streams() {
    let (state, base, task) = server().await;
    sqlx::query("INSERT INTO jobs(id,kind,status,stage,available_at,created_at,updated_at) VALUES('stream-job','sync','succeeded','done',now(),now(),now())").execute(&state.pool).await.unwrap();
    let client = reqwest::Client::new();
    let head: Value = client
        .get(format!("{base}/api/v2/events/head"))
        .bearer_auth("admin-a")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let cursor = head["cursor"].as_str().unwrap();
    let mut global = client
        .get(format!("{base}/api/v2/events"))
        .bearer_auth("admin-a")
        .header("Last-Event-ID", cursor)
        .send()
        .await
        .unwrap();
    let mut job = client
        .get(format!("{base}/api/v2/jobs/stream-job/events"))
        .bearer_auth("admin-a")
        .header("Last-Event-ID", cursor)
        .send()
        .await
        .unwrap();
    assert_eq!(global.status(), StatusCode::OK);
    assert_eq!(job.status(), StatusCode::OK);
    assert_eq!(
        client
            .delete(format!("{base}/api/v2/admin/tokens/admin-a"))
            .bearer_auth("admin-b")
            .header("Last-Event-ID", cursor)
            .header("Idempotency-Key", "revoke-stream-admin")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    for response in [&mut global, &mut job] {
        assert!(
            tokio::time::timeout(Duration::from_secs(3), response.chunk())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(
        client
            .get(format!("{base}/api/v2/version"))
            .bearer_auth("admin-b")
            .header("Last-Event-ID", cursor)
            .header("Idempotency-Key", "revoke-stream-admin")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v2/events"))
            .bearer_auth("admin-a")
            .header("Last-Event-ID", cursor)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // A database-only revocation is caught by the periodic fallback.
    let mut survivor = client
        .get(format!("{base}/api/v2/events"))
        .bearer_auth("admin-b")
        .header("Last-Event-ID", cursor)
        .header("Idempotency-Key", "revoke-stream-admin")
        .send()
        .await
        .unwrap();
    sqlx::query("UPDATE admin_tokens SET revoked_at=now() WHERE id='admin-b'")
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(7), survivor.chunk())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    task.abort();
}

#[tokio::test]
async fn client_summary_counts_all_users_and_finds_notifications_beyond_first_page() {
    let (state, base, task) = server().await;
    sqlx::query("INSERT INTO users(id,name,enabled,expires_at,quota_bytes,usage_bytes,created_at,updated_at) SELECT 'bulk-'||i,'Bulk '||i,i<>235,CASE WHEN i=234 THEN now()-interval '1 day' WHEN i=233 THEN now()+interval '1 day' END,CASE WHEN i=232 THEN 10 END,CASE WHEN i=232 THEN 10 ELSE 0 END,now(),now() FROM generate_series(1,235) i").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at) SELECT 'node-'||i,'Node '||i,'localhost',22,'root','password','PRIVATE-SSH','example.test',443,':443','hash-'||i,'PRIVATE-TOKEN','PRIVATE-STATS','PRIVATE-CONFIG',now(),now() FROM generate_series(1,235) i").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO node_packages(node_id,period_id,config,usage_bytes) VALUES('node-235','test','{\"quota_bytes\":10}',10)").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO credentials(id,name,kind,archived,created_at,updated_at) SELECT 'credential-'||i,'Credential '||i,'ssh_password',i=234,now(),now() FROM generate_series(1,235) i").execute(&state.pool).await.unwrap();
    sqlx::query(
        "UPDATE credentials SET reminder_at=now()+interval '1 day' WHERE id='credential-235'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,metadata,created_at) SELECT id,1,'PRIVATE-CREDENTIAL',CASE WHEN id='credential-235' THEN '{\"expires_at\":\"invalid\"}'::jsonb WHEN id='credential-234' THEN jsonb_build_object('expires_at',now()+interval '1 day') ELSE '{}'::jsonb END,now() FROM credentials").execute(&state.pool).await.unwrap();
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{base}/api/v2/client/summary"))
        .bearer_auth("admin-a")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(!body.contains("PRIVATE-"));
    let summary: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(summary["node_count"], 235);
    assert_eq!(summary["user_total"], 236);
    assert_eq!(summary["user_enabled"], 235);
    assert_eq!(summary["user_expired"], 1);
    assert_eq!(summary["user_expiring"], 1);
    assert_eq!(summary["user_depleted"], 1);
    assert_eq!(summary["notification_nodes"].as_array().unwrap().len(), 1);
    assert_eq!(summary["notification_nodes"][0]["id"], "node-235");
    assert_eq!(
        summary["notification_credentials"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        summary["notification_credentials"][0]["id"],
        "credential-235"
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v2/client/summary"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    task.abort();
}
