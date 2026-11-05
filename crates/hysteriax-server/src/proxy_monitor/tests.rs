use super::*;
use base64::Engine;
async fn fixture() -> (AppState, String) {
    let pool = crate::db::test_pool_with_max_connections(3).await;
    let key = base64::engine::general_purpose::STANDARD_NO_PAD.encode([8_u8; 32]);
    let state = AppState::new(pool, crate::security::SecretBox::from_base64(&key).unwrap());
    let (_,axum::Json(created))=crate::api::nodes::create(axum::extract::State(state.clone()),axum::Json(crate::credentials::test_node_request(&state,json!({"name":"Probe test","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"unused","public_host":"example.test","public_port":443,"listen_addr":":443"})).await)).await.unwrap();
    (state, created["node"]["id"].as_str().unwrap().into())
}
async fn proxy_work(state: &AppState, node: &str) -> crate::background::Work {
    sqlx::query("UPDATE nodes SET deployed_revision=1,mtls_state='ready' WHERE id=$1")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    crate::background::ensure_cycle(&state.pool, "proxy")
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour'")
        .execute(&state.pool)
        .await
        .unwrap();
    crate::background::claim(&state.pool, "proxy", &state.instance_id)
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn retention_preserves_accounting_and_removes_only_old_monitoring() {
    let (state, node) = fixture().await;
    for at in ["now()-interval '31 days'", "now()"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO online_samples(cycle_id,node_id,sampled_at,status) VALUES($1,$2,{at},'ok')"
        )))
        .bind(Uuid::new_v4())
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!("INSERT INTO proxy_probe_samples(node_id,revision,sampled_at,status) VALUES($1,1,{at},'ok')"))).bind(&node).execute(&state.pool).await.unwrap();
    }
    sqlx::query("INSERT INTO node_network_samples(node_id,period_id,delta_tx,delta_rx,sampled_at) VALUES($1,'billing',10,20,now()-interval '31 days')").bind(&node).execute(&state.pool).await.unwrap();
    cleanup(&state).await.unwrap();
    for table in [
        "online_samples",
        "proxy_probe_samples",
        "node_network_samples",
    ] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
    }
}
#[tokio::test]
async fn monitoring_retention_is_bounded_skips_claimed_rows_and_recovers_its_remaining_work() {
    use crate::background::maintenance;
    let (state, node) = fixture().await;
    sqlx::query("INSERT INTO proxy_probe_samples(node_id,revision,sampled_at,status) SELECT $1,1,clock_timestamp()-interval '32 days'-make_interval(secs=>i::double precision),'ok' FROM generate_series(1,301)i").bind(&node).execute(&state.pool).await.unwrap();
    let mut held = state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM proxy_probe_samples ORDER BY sampled_at,id LIMIT 1 FOR UPDATE")
        .fetch_one(&mut *held)
        .await
        .unwrap();
    maintenance::ensure(
        &state.pool,
        "retention-test",
        "monitoring-retention",
        json!({"table":"proxy_probe_samples"}),
    )
    .await
    .unwrap();
    let old = maintenance::claim(&state.pool, "monitoring-retention", "old")
        .await
        .unwrap()
        .unwrap();
    cleanup_work(&state, &old).await.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM proxy_probe_samples").await,
        45
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM background_work WHERE state='queued'"
        )
        .await,
        1
    );
    assert!(cleanup_work(&state, &old).await.is_err());
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    let next = maintenance::claim(&state.pool, "monitoring-retention", "next")
        .await
        .unwrap()
        .unwrap();
    cleanup_work(&state, &next).await.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM proxy_probe_samples").await,
        1
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM background_work WHERE state='queued'"
        )
        .await,
        1,
        "a skipped old row must keep the next batch scheduled"
    );
    held.rollback().await.unwrap();
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    let last = maintenance::claim(&state.pool, "monitoring-retention", "last")
        .await
        .unwrap()
        .unwrap();
    cleanup_work(&state, &last).await.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM proxy_probe_samples").await,
        0
    );
    assert_eq!(number(&state,"SELECT count(*) FROM background_work WHERE state='complete' AND available_at>clock_timestamp()+interval '23 hours'").await,1);
}

#[tokio::test]
async fn failed_retention_ack_rolls_back_deleted_rows_and_expired_token_cleanup_preserves_live_tokens()
 {
    use crate::background::maintenance;
    let (state, node) = fixture().await;
    maintenance::ensure(
        &state.pool,
        "token-retention",
        "monitoring-retention",
        json!({"table":"monitoring_probe_tokens"}),
    )
    .await
    .unwrap();
    let work = maintenance::claim(&state.pool, "monitoring-retention", "owner")
        .await
        .unwrap()
        .unwrap();
    sqlx::query("INSERT INTO monitoring_probe_tokens(node_id,token_hash,expires_at) VALUES($1,'expired',clock_timestamp()-interval '1 second')").bind(&node).execute(&state.pool).await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION reject_retention_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF OLD.state='running' AND NEW.state<>'running' THEN RAISE EXCEPTION 'reject acknowledgement'; END IF; RETURN NEW; END $$; CREATE TRIGGER reject_retention_ack BEFORE UPDATE ON background_work FOR EACH ROW EXECUTE FUNCTION reject_retention_ack();").execute(&state.pool).await.unwrap();
    assert!(cleanup_work(&state, &work).await.is_err());
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM monitoring_probe_tokens WHERE token_hash='expired'"
        )
        .await,
        1
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM background_work WHERE state='running'"
        )
        .await,
        1
    );
    sqlx::query("DROP TRIGGER reject_retention_ack ON background_work")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE monitoring_probe_tokens SET token_hash='live',expires_at=clock_timestamp()+interval '30 seconds'").execute(&state.pool).await.unwrap();
    cleanup_work(&state, &work).await.unwrap();
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM monitoring_probe_tokens WHERE token_hash='live'"
        )
        .await,
        1
    );
    assert_eq!(number(&state,"SELECT count(*) FROM background_work WHERE state='complete' AND available_at>clock_timestamp()+interval '50 seconds'").await,1);
    sqlx::query(
        "UPDATE monitoring_probe_tokens SET expires_at=clock_timestamp()-interval '1 second'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE background_work SET available_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    maintenance::ensure(
        &state.pool,
        "token-retention",
        "monitoring-retention",
        json!({"table":"monitoring_probe_tokens"}),
    )
    .await
    .unwrap();
    let next = maintenance::claim(&state.pool, "monitoring-retention", "next")
        .await
        .unwrap()
        .unwrap();
    cleanup_work(&state, &next).await.unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM monitoring_probe_tokens").await,
        0
    );
}

#[tokio::test]
async fn missing_mtls_certificate_is_unconfigured_not_an_outage() {
    let (state, node) = fixture().await;
    let encrypted = state
            .secrets
            .encrypt(
                &json!({"server_config":{"tls":{"cert":"/test/server.crt","key":"/test/server.key"},"mtls":{"mode":"managed","ca_credential_id":Uuid::new_v4().to_string(),"ca_credential_version":1}},"listen_addr":":443"})
                    .to_string(),
            )
            .unwrap();
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE config_versions SET config_enc=$2 WHERE node_id=$1")
        .bind(&node)
        .bind(encrypted)
        .execute(&state.pool)
        .await
        .unwrap();
    let row=sqlx::query("SELECT n.*,v.config_enc FROM nodes n JOIN config_versions v ON v.node_id=n.id WHERE n.id=$1").bind(&node).fetch_one(&state.pool).await.unwrap();
    assert_eq!(
        probe_using(
            &state,
            &proxy_work(&state, &node).await,
            &row,
            "/usr/bin/true",
            "fixture"
        )
        .await
        .unwrap()
        .status,
        "unconfigured"
    );
}
async fn number(state: &AppState, query: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&state.pool)
        .await
        .unwrap()
}
async fn take_over(state: &AppState) -> crate::background::Work {
    sqlx::query("UPDATE resource_leases SET expires_at=clock_timestamp()-interval '1 second'")
        .execute(&state.pool)
        .await
        .unwrap();
    crate::background::claim(&state.pool, "proxy", "replacement")
        .await
        .unwrap()
        .unwrap()
}
fn success() -> ProbeResult {
    ProbeResult {
        status: "ok".into(),
        connection_ms: Some(12.0),
        latency_ms: Some(4.0),
        ..Default::default()
    }
}

#[tokio::test]
async fn proxy_cycles_freeze_only_eligible_nodes_and_use_database_minutes() {
    let (state, node) = fixture().await;
    sqlx::query("UPDATE nodes SET deployed_revision=1,mtls_state='ready' WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        crate::background::ensure_cycle(&state.pool, "proxy"),
        crate::background::ensure_cycle(&state.pool, "proxy")
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(number(&state,"SELECT count(*) FROM sampling_cycles WHERE deadline_at-period_start<>interval '60 seconds' OR extract(second FROM period_start)<>0").await,0);
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM sampling_cycles WHERE expected_nodes<>1"
        )
        .await,
        0
    );
    let count = number(&state, "SELECT count(*) FROM sampling_cycles").await;
    assert!(count == 1 || count == 2); // A real minute boundary may separate the calls.
    if count == 1 {
        assert_eq!(a, b);
    }
    sqlx::query("UPDATE nodes SET mtls_state='needs_configuration' WHERE id=$1")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    crate::background::ensure_cycle(&state.pool, "proxy")
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM background_work WHERE cycle_id=$1")
            .bind(a)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1,
        "existing cycle membership is frozen"
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM online_samples").await,
        0
    );
}

#[tokio::test]
async fn proxy_claims_share_the_node_resource_with_jobs_and_other_samplers() {
    let (state, node) = fixture().await;
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    crate::background::ensure_cycle(&state.pool, "proxy")
        .await
        .unwrap();
    sqlx::query("UPDATE background_work SET deadline_at=clock_timestamp()+interval '1 hour'")
        .execute(&state.pool)
        .await
        .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    let job = crate::execution::acquire(&mut tx, &format!("node:{node}"), "job", "job")
        .await
        .unwrap()
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        crate::background::claim(&state.pool, "proxy", "probe")
            .await
            .unwrap()
            .is_none()
    );
    let mut tx = state.pool.begin().await.unwrap();
    job.release(&mut tx).await.unwrap();
    tx.commit().await.unwrap();
    let (a, b) = tokio::join!(
        crate::background::claim(&state.pool, "proxy", "a"),
        crate::background::claim(&state.pool, "proxy", "b")
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.is_some(), b.is_some());
    let work = a.or(b).unwrap();
    crate::background::ensure_cycle(&state.pool, "traffic")
        .await
        .unwrap();
    assert!(
        crate::background::claim(&state.pool, "traffic", "traffic")
            .await
            .unwrap()
            .is_none()
    );
    crate::background::finish(&state.pool, &work, true)
        .await
        .unwrap();
    assert!(
        crate::background::claim(&state.pool, "traffic", "traffic")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn proxy_result_commit_is_idempotent_and_preserves_another_attempts_token() {
    let (state, node) = fixture().await;
    let work = proxy_work(&state, &node).await;
    crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
        install_token(&state, &work, "new-token").await.unwrap();
        save_result(&state, &work, success(), "old-token")
            .await
            .unwrap();
        assert_eq!(
            number(
                &state,
                "SELECT count(*) FROM monitoring_probe_tokens WHERE token_hash='new-token'"
            )
            .await,
            1
        );
        save_result(
            &state,
            &work,
            ProbeResult {
                status: "failed".into(),
                ..Default::default()
            },
            "new-token",
        )
        .await
        .unwrap();
        assert_eq!(
            number(
                &state,
                "SELECT count(*) FROM proxy_probe_samples WHERE status='ok'"
            )
            .await,
            1
        );
        assert_eq!(
            number(
                &state,
                "SELECT count(*) FROM monitoring_probe_state WHERE successes=1 AND failures=0"
            )
            .await,
            1
        );
        assert_eq!(
            number(&state, "SELECT count(*) FROM monitoring_probe_tokens").await,
            0
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn replacement_fences_old_probe_results_and_token_writes_then_acks_existing_result() {
    let (state, node) = fixture().await;
    let old = proxy_work(&state, &node).await;
    let new = take_over(&state).await;
    assert_eq!(old.payload, new.payload);
    install_token(&state, &new, "new-token").await.unwrap();
    assert!(install_token(&state, &old, "stale-token").await.is_err());
    assert!(
        save_result(&state, &old, success(), "new-token")
            .await
            .is_err()
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM proxy_probe_samples").await,
        0
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM monitoring_probe_tokens WHERE token_hash='new-token'"
        )
        .await,
        1
    );
    save_result(&state, &new, success(), "new-token")
        .await
        .unwrap();
    let resumed = take_over(&state).await;
    // If collection were repeated this deliberately corrupt ciphertext would fail.
    sqlx::query("UPDATE config_versions SET config_enc='not encrypted' WHERE node_id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    crate::execution::supervise(state.pool.clone(), resumed.leases.clone(), async {
        sample_work(&state, &resumed).await.unwrap();
        crate::background::finish(&state.pool, &resumed, true)
            .await
            .unwrap();
    })
    .await;
    // Completion releases the leases. A concurrent renewal may observe that
    // release before the commit response reaches this future; durable state
    // below proves completion regardless of the supervisor's local outcome.
    assert_eq!(
        number(&state, "SELECT count(*) FROM proxy_probe_samples").await,
        1
    );
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM background_work WHERE state='complete'"
        )
        .await,
        1
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM execution_operations").await,
        0
    );
}

#[tokio::test]
async fn proxy_results_cannot_overwrite_a_newer_deployment_or_newer_health() {
    let (state, node) = fixture().await;
    let work = proxy_work(&state, &node).await;
    sqlx::query("UPDATE nodes SET deployed_revision=2 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    save_result(&state, &work, success(), "unused")
        .await
        .unwrap();
    assert_eq!(
        number(&state, "SELECT count(*) FROM proxy_probe_samples").await,
        0
    );
    sqlx::query("UPDATE nodes SET deployed_revision=1 WHERE id=$1")
        .bind(&node)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO monitoring_probe_state(node_id,revision,health,failures,successes,sampled_at) VALUES($1,1,'healthy',0,2,clock_timestamp()+interval '1 minute')").bind(&node).execute(&state.pool).await.unwrap();
    save_result(
        &state,
        &work,
        ProbeResult {
            status: "failed".into(),
            ..Default::default()
        },
        "unused",
    )
    .await
    .unwrap();
    assert_eq!(
        number(
            &state,
            "SELECT count(*) FROM proxy_probe_samples WHERE status='failed'"
        )
        .await,
        1
    );
    assert_eq!(number(&state,"SELECT count(*) FROM monitoring_probe_state WHERE health='healthy' AND successes=2 AND failures=0").await,1);
}

#[tokio::test]
async fn failed_probe_process_uses_fenced_token_and_saves_one_sanitized_result() {
    let (state, node) = fixture().await;
    let work = proxy_work(&state, &node).await;
    let row=sqlx::query("SELECT n.*,v.config_enc FROM nodes n JOIN config_versions v ON v.node_id=n.id AND v.revision=n.deployed_revision WHERE n.id=$1").bind(node).fetch_one(&state.pool).await.unwrap();
    crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
        let error = probe_using(&state, &work, &row, "/usr/bin/true", "private-token")
            .await
            .err()
            .unwrap();
        assert!(error.is::<ProbeFailure>());
        assert!(!error.to_string().contains("private-token"));
        assert_eq!(
            number(&state, "SELECT count(*) FROM monitoring_probe_tokens").await,
            1
        );
        save_result(
            &state,
            &work,
            ProbeResult {
                status: "failed".into(),
                reason: Some(error.to_string()),
                ..Default::default()
            },
            &token_digest("private-token"),
        )
        .await
        .unwrap();
        assert_eq!(
            number(&state, "SELECT count(*) FROM monitoring_probe_tokens").await,
            0
        );
        assert_eq!(
            number(
                &state,
                "SELECT count(*) FROM proxy_probe_samples WHERE status='failed'"
            )
            .await,
            1
        );
        assert_eq!(
            number(&state, "SELECT count(*) FROM execution_operations").await,
            0
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelled_proxy_work_cannot_create_a_probe_token_or_health_result() {
    let (state, node) = fixture().await;
    let work = proxy_work(&state, &node).await;
    sqlx::query("UPDATE nodes SET state='deleting' WHERE id=$1")
        .bind(node)
        .execute(&state.pool)
        .await
        .unwrap();
    let result = crate::execution::supervise(state.pool.clone(), work.leases.clone(), async {
        install_token(&state, &work, "stale").await.unwrap();
        save_result(&state, &work, success(), "stale")
            .await
            .unwrap();
    })
    .await;
    assert!(result.is_none());
    assert_eq!(
        number(&state, "SELECT count(*) FROM monitoring_probe_tokens").await,
        0
    );
    assert_eq!(
        number(&state, "SELECT count(*) FROM proxy_probe_samples").await,
        0
    );
}

#[test]
fn deployed_null_sni_does_not_use_an_undeployed_edit() {
    assert_eq!(
        deployed_sni(&json!({"tls_sni":null}), Some("new.example".into())),
        None
    );
    assert_eq!(
        deployed_sni(
            &json!({"tls_sni":"old.example"}),
            Some("new.example".into())
        ),
        Some("old.example".into())
    );
    assert_eq!(
        deployed_sni(&json!({}), Some("legacy.example".into())),
        Some("legacy.example".into())
    );
}

#[test]
fn client_error_classification_never_returns_log_credentials() {
    assert_eq!(
        client_failure("authentication failed auth=hx_private_test_value"),
        "代理身份验证失败"
    );
    assert_eq!(
        client_failure("FATAL connect error: timeout: no recent network activity"),
        "公网代理连接超时"
    );
    assert_eq!(
        client_failure("bind: address already in use; auth=hx_private_test_value"),
        "本地转发端口已被占用"
    );
}

#[test]
fn temporary_material_is_removed_on_drop() {
    let path = std::env::temp_dir().join(format!("hysteriax-monitor-test-{}", Uuid::new_v4()));
    {
        let dir = WorkDir(path.clone(), None);
        std::fs::create_dir(&dir.0).unwrap();
        std::fs::write(dir.0.join("client.key"), "test-only").unwrap();
    }
    assert!(!path.exists());
}
