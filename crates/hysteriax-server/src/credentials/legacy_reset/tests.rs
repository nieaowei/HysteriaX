use super::*;
use crate::{credentials::migration, db};

fn secrets() -> SecretBox {
    SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").unwrap()
}
async fn legacy_node(pool: &sqlx::PgPool, secret: &SecretBox, desired: &Value, deployed: &Value) {
    sqlx::query("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,public_host,public_port,listen_addr,node_token_hash,node_token_enc,traffic_stats_secret_enc,desired_config_enc,deployed_config_enc,deployed_revision,created_at,updated_at) VALUES('legacy','Legacy','127.0.0.1',22,'root','password',$1,'example.test',443,':443','node-hash',$2,$3,$4,$5,1,now(),now())")
        .bind(secret.encrypt("password").unwrap()).bind(secret.encrypt("node-token").unwrap()).bind(secret.encrypt("stats-token").unwrap()).bind(secret.encrypt(&desired.to_string()).unwrap()).bind(secret.encrypt(&deployed.to_string()).unwrap()).execute(pool).await.unwrap();
    let snapshot = json!({"server_config":deployed,"public_host":"example.test","public_port":443,"listen_addr":":443"});
    sqlx::query("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,deployed_success,created_at) VALUES('history','legacy',1,$1,'old-digest',TRUE,now())").bind(secret.encrypt(&snapshot.to_string()).unwrap()).execute(pool).await.unwrap();
}

#[tokio::test]
async fn corrupt_legacy_inline_clients_are_discarded_before_validation_and_actual_mtls_blocks_access()
 {
    let pool = db::test_pool_with_max_connections(3).await;
    let secret = secrets();
    legacy_node(
        &pool,
        &secret,
        &json!({"mtls":{"mode":"off"}}),
        &json!({"tls":{"clientCA":"/etc/old-ca.pem"}}),
    )
    .await;
    sqlx::query(
        "INSERT INTO users(id,name,created_at,updated_at) VALUES('alice','Alice',now(),now())",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("WITH assignment_fixture(user_id,node_id,credential_hash,credential_enc,client_certificate_enc,client_private_key_enc,created_at) AS MATERIALIZED (VALUES('alice','legacy','unchanged-hash',$1,'broken-ciphertext',NULL,now())), password_fixture AS (UPDATE users u SET connection_credential_hash=c.credential_hash,connection_credential_enc=c.credential_enc FROM (SELECT DISTINCT ON(user_id) user_id,credential_hash,credential_enc FROM assignment_fixture ORDER BY user_id) c WHERE u.id=c.user_id), fixture_groups AS (INSERT INTO authorization_groups(id,name,created_at,updated_at) SELECT 'fixture-'||md5(user_id||chr(31)||node_id),'Fixture authorization',now(),now() FROM assignment_fixture ON CONFLICT(id) DO UPDATE SET name=EXCLUDED.name RETURNING id), fixture_members AS (INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT g.id,a.user_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING), fixture_nodes AS (INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT g.id,a.node_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING) INSERT INTO node_assignments(user_id,node_id,client_certificate_enc,client_private_key_enc,created_at) SELECT user_id,node_id,client_certificate_enc,client_private_key_enc,created_at FROM assignment_fixture").bind(secret.encrypt("connection-password").unwrap()).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,available_at,created_at,updated_at) VALUES('old-sync','sync','legacy',1,'queued','queued',now(),now(),now())").execute(&pool).await.unwrap();
    migration::migrate(&pool, &secret).await.unwrap();
    let row = sqlx::query("SELECT * FROM nodes WHERE id='legacy'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("mtls_state"), "needs_configuration");
    assert!(!row.get::<bool, _>("mtls_recovery_configured"));
    assert_eq!(
        secret
            .decrypt(&row.get::<String, _>("node_token_enc"))
            .unwrap(),
        "node-token"
    );
    let desired: Value = serde_json::from_str(
        &secret
            .decrypt(&row.get::<String, _>("desired_config_enc"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(desired["mtls"]["mode"], "off");
    let cipher: String =
        sqlx::query_scalar("SELECT config_enc FROM config_versions WHERE id='history'")
            .fetch_one(&pool)
            .await
            .unwrap();
    let history: Value = serde_json::from_str(&secret.decrypt(&cipher).unwrap()).unwrap();
    assert_eq!(history["legacy_mtls_reset"], true);
    assert!(history["server_config"]["tls"].get("clientCA").is_none());
    let row = sqlx::query("SELECT a.*,u.connection_credential_hash AS credential_hash,u.connection_credential_enc AS credential_enc FROM node_assignments a JOIN users u ON u.id=a.user_id WHERE user_id='alice'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("credential_hash"), "unchanged-hash");
    assert_eq!(
        secret
            .decrypt(&row.get::<String, _>("credential_enc"))
            .unwrap(),
        "connection-password"
    );
    assert!(row.try_get::<String, _>("client_certificate_enc").is_err());
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id='old-sync'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "cancelled");
    let kicks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM kick_requests WHERE reasons ? 'legacy_mtls_reset'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kicks, 1);
    migration::migrate(&pool, &secret).await.unwrap();
    let kicks: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind='kick'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kicks, 1);
    let audit: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_records WHERE action='mtls.legacy_reset'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audit, 1);
}

#[tokio::test]
async fn historical_mtls_alone_does_not_block_current_off_node() {
    let pool = db::test_pool_with_max_connections(3).await;
    let secret = secrets();
    legacy_node(&pool, &secret, &json!({}), &json!({})).await;
    sqlx::query("UPDATE config_versions SET config_enc=$1 WHERE id='history'")
        .bind(
            secret
                .encrypt(
                    &json!({"server_config":{"tls":{"clientCA":"/etc/old-ca.pem"}}}).to_string(),
                )
                .unwrap(),
        )
        .execute(&pool)
        .await
        .unwrap();
    migration::migrate(&pool, &secret).await.unwrap();
    let state: String = sqlx::query_scalar("SELECT mtls_state FROM nodes WHERE id='legacy'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "ready");
    let eligible: bool =
        sqlx::query_scalar("SELECT deployed_success FROM config_versions WHERE id='history'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!eligible);
    let kicks: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind='kick'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kicks, 0);
    let input = crate::deployment::JobInput {
        id: "history-replay".into(),
        kind: "rollback".into(),
        node_id: Some("legacy".into()),
        target_revision: Some(1),
        payload: json!({}),
        attempts: 1,
    };
    let error = crate::deployment::run_job(&pool, &secret, &input)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("history was invalidated"));
}

#[tokio::test]
async fn corrupt_converted_user_material_is_removed_but_non_client_credentials_survive() {
    let pool = db::test_pool_with_max_connections(3).await;
    let secret = secrets();
    legacy_node(&pool, &secret, &json!({}), &json!({})).await;
    migration::migrate(&pool, &secret).await.unwrap();
    sqlx::query("DELETE FROM credential_migrations WHERE name=$1")
        .bind(MARKER)
        .execute(&pool)
        .await
        .unwrap();
    for (id, kind, owner) in [
        ("old-user", "tls_identity", Some("alice")),
        ("keep-ca", "ca_certificate", None),
        ("keep-token", "api_token", Some("alice")),
    ] {
        sqlx::query("INSERT INTO credentials(id,name,kind,owner_user_id,created_at,updated_at) VALUES($1,$1,$2,$3,now(),now())").bind(id).bind(kind).bind(owner).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES($1,1,'corrupt-unused-material',now())").bind(id).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO credential_batches(id,credential_id,version,created_at) VALUES('old-batch','old-user',1,now())").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,node_id,payload_json,status,stage,available_at,created_at,updated_at) VALUES('old-apply','credential-apply','legacy','{\"mode\":\"mtls\",\"credential_id\":\"old-user\"}','queued','queued',now(),now(),now())").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO credential_batch_items(batch_id,node_id,user_id,expected_revision,job_id) VALUES('old-batch','legacy','alice',1,'old-apply')").execute(&pool).await.unwrap();
    migration::migrate(&pool, &secret).await.unwrap();
    let remaining: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM credentials WHERE id IN ('old-user','keep-ca','keep-token') ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, vec!["keep-ca", "keep-token"]);
    let payload: Value = sqlx::query_scalar("SELECT payload_json FROM jobs WHERE id='old-apply'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload, json!({}));
}

#[tokio::test]
async fn empty_legacy_client_ca_and_disabled_mode_are_normalized_without_service_block() {
    let pool = db::test_pool_with_max_connections(3).await;
    let secret = secrets();
    legacy_node(
        &pool,
        &secret,
        &json!({"tls":{"clientCA":null},"mtls":{"mode":"disabled"}}),
        &json!({"tls":{"clientCA":""}}),
    )
    .await;
    migration::migrate(&pool, &secret).await.unwrap();
    let row = sqlx::query(
        "SELECT mtls_state,desired_config_enc,deployed_config_enc FROM nodes WHERE id='legacy'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("mtls_state"), "ready");
    for field in ["desired_config_enc", "deployed_config_enc"] {
        let value: Value =
            serde_json::from_str(&secret.decrypt(&row.get::<String, _>(field)).unwrap()).unwrap();
        assert!(value["tls"].get("clientCA").is_none());
    }
    let eligible: bool =
        sqlx::query_scalar("SELECT deployed_success FROM config_versions WHERE id='history'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(eligible);
}

#[tokio::test]
async fn invalid_retained_ssh_rolls_back_reset_and_discarded_material_deletions() {
    let pool = db::test_pool_with_max_connections(3).await;
    let secret = secrets();
    legacy_node(&pool, &secret, &json!({}), &json!({})).await;
    migration::migrate(&pool, &secret).await.unwrap();
    let original: String =
        sqlx::query_scalar("SELECT ssh_credential_id FROM nodes WHERE id='legacy'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("DELETE FROM credential_migrations WHERE name=$1")
        .bind(MARKER)
        .execute(&pool)
        .await
        .unwrap();
    for (id, kind, owner, payload) in [
        (
            "discard-user",
            "tls_identity",
            Some("alice"),
            "unreadable".to_owned(),
        ),
        (
            "bad-ssh",
            "ssh_private_key",
            None,
            secret
                .encrypt(&json!({"secret":"not-a-key"}).to_string())
                .unwrap(),
        ),
    ] {
        sqlx::query("INSERT INTO credentials(id,name,kind,owner_user_id,created_at,updated_at) VALUES($1,$1,$2,$3,now(),now())").bind(id).bind(kind).bind(owner).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES($1,1,$2,now())").bind(id).bind(payload).execute(&pool).await.unwrap();
    }
    sqlx::query(
        "UPDATE nodes SET ssh_credential_id='bad-ssh',ssh_credential_version=1 WHERE id='legacy'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let error = migration::migrate(&pool, &secret).await.unwrap_err();
    assert!(error.to_string().contains("SSH private key"));
    let retained: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials WHERE id='discard-user')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(retained);
    let completed: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credential_migrations WHERE name=$1)")
            .bind(MARKER)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!completed);
    sqlx::query("UPDATE nodes SET ssh_credential_id=$1 WHERE id='legacy'")
        .bind(original)
        .execute(&pool)
        .await
        .unwrap();
    migration::migrate(&pool, &secret).await.unwrap();
    let retained: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM credentials WHERE id='discard-user')")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!retained);
}
