use sqlx::Row;

use super::{enqueue_job_in_tx, job_summary, validate_admin_token_label};

#[tokio::test]
async fn job_node_snapshot_survives_node_rename_and_deletion() {
    let pool = crate::db::test_pool().await;
    sqlx::query("WITH fixture_ssh AS (INSERT INTO credentials(id,name,kind,created_at,updated_at) VALUES('fixture-ssh','Fixture SSH','ssh_password',now(),now()) ON CONFLICT DO NOTHING), fixture_ssh_version AS (INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES('fixture-ssh',1,'unused',now()) ON CONFLICT DO NOTHING) INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,created_at,updated_at,ssh_credential_id,ssh_credential_version) VALUES ('snapshot-node','Original node','127.0.0.1',22,'root','node.example.test',443,':443','node-hash','node-token','stats-secret','{}',now(),now(),'fixture-ssh',1)")
            .execute(&pool).await.unwrap();

    let mut tx = pool.begin().await.unwrap();
    let id = enqueue_job_in_tx(&mut tx, "sync", Some("snapshot-node"), Some(1))
        .await
        .unwrap();
    let global_id = enqueue_job_in_tx(&mut tx, "global", None, None)
        .await
        .unwrap();
    assert!(
        enqueue_job_in_tx(&mut tx, "sync", Some("missing-node"), Some(1))
            .await
            .is_err()
    );
    tx.commit().await.unwrap();

    sqlx::query("UPDATE nodes SET name = 'Renamed node' WHERE id = 'snapshot-node'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM nodes WHERE id = 'snapshot-node'")
        .execute(&pool)
        .await
        .unwrap();

    let row = sqlx::query("SELECT * FROM jobs WHERE id = $1")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let summary = serde_json::to_value(job_summary(&row)).unwrap();
    assert_eq!(summary["node_id"], "snapshot-node");
    assert_eq!(summary["node_name"], "Original node");
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload_json FROM job_events WHERE job_id = $1")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(payload["node_name"], "Original node");

    let global = sqlx::query("SELECT node_id, node_name FROM jobs WHERE id = $1")
        .bind(&global_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(global.get::<Option<String>, _>("node_id").is_none());
    assert!(global.get::<Option<String>, _>("node_name").is_none());
}

#[tokio::test]
async fn collection_routes_always_paginate_without_page_suffix() {
    use super::{AppState, Duration, StatusCode, Value, json, router, token_digest};

    let pool = crate::db::test_pool().await;
    let state = AppState::new(
        pool,
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap(),
    );
    let token = "collection-route-test";
    sqlx::query(
        "INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('admin',$1,'Test',now())",
    )
    .bind(token_digest(token))
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO users(id,name,enabled,created_at,updated_at) SELECT 'user-'||i,'User '||i,true,now(),now() FROM generate_series(1,65) i")
            .execute(&state.pool).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(state);
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    for path in ["nodes", "users", "dns/records", "credentials"] {
        let url = format!("{base}/api/v2/{path}");
        let all = client.get(&url).bearer_auth(token).send().await.unwrap();
        assert_eq!(all.status(), StatusCode::OK);
        let all: Value = all.json().await.unwrap();
        assert!(
            all.is_object(),
            "collection responses always use the page envelope"
        );
        assert_eq!(all["page"], 1);
        assert_eq!(all["page_size"], 50);
        let total = all["total"].as_u64().unwrap() as usize;
        assert!(all["items"].as_array().unwrap().len() <= 50);
        if path == "users" {
            assert_eq!(total, 65);
            assert_eq!(all["items"].as_array().unwrap().len(), 50);
        }
        for query in ["page=1", "page_size=25", "page=2&page_size=25"] {
            let response = client
                .get(format!("{url}?{query}"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let page: Value = response.json().await.unwrap();
            assert_eq!(page["total"], total);
            let size = if query == "page=1" { 50 } else { 25 };
            assert_eq!(page["page_size"], size);
            assert!(page["items"].as_array().unwrap().len() <= size);
            if path == "users" && query == "page=2&page_size=25" {
                assert_eq!(page["page"], 2);
                assert_eq!(page["items"].as_array().unwrap().len(), 25);
            }
        }
        for query in ["page=0", "page_size=201", "sort=invalid"] {
            assert_eq!(
                client
                    .get(format!("{url}?{query}"))
                    .bearer_auth(token)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            client
                .get(format!("{url}/page?page=1"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            client
                .get(format!("{url}?page=1"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let filtered = client
        .get(format!("{base}/api/v2/dns/records?zone_id=missing"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(filtered.status(), StatusCode::OK);
    let filtered: Value = filtered.json().await.unwrap();
    assert_eq!(filtered["items"], json!([]));
    assert_eq!(filtered["total"], 0);
    let searched: Value = client
        .get(format!("{base}/api/v2/users?q=User%2065"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(searched["total"], 1);
    assert_eq!(searched["items"][0]["id"], "user-65");
    server.abort();
}

#[test]
fn admin_token_labels_count_unicode_characters_not_utf8_bytes() {
    let hundred_characters = "令牌".repeat(50);
    assert_eq!(
        validate_admin_token_label(&hundred_characters).unwrap(),
        hundred_characters
    );
    assert!(validate_admin_token_label(&format!("{hundred_characters}令牌")).is_err());
    assert!(validate_admin_token_label("   ").is_err());
}
