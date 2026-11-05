use super::*;
use base64::Engine;

fn instant(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}
fn sample_value(tx: i64, rx: i64) -> NetworkSample {
    NetworkSample {
        boot: "11111111-1111-4111-8111-111111111111".into(),
        interface: "eth0".into(),
        tx,
        rx,
    }
}
async fn fixture() -> (AppState, String, String) {
    let pool = db::test_pool_with_max_connections(3).await;
    let key = base64::engine::general_purpose::STANDARD_NO_PAD.encode([3_u8; 32]);
    let state = AppState::new(pool, crate::security::SecretBox::from_base64(&key).unwrap());
    let (_, Json(created)) = crate::api::nodes::create(State(state.clone()), Json(crate::credentials::test_node_request(&state,json!({
            "name":"Package node", "ssh_host":"127.0.0.1", "ssh_port":22, "ssh_username":"root",
            "ssh_auth_type":"password", "ssh_secret":"unused", "public_host":"node.example.test", "public_port":443, "listen_addr":":443"
        })).await)).await.unwrap();
    (
        state,
        created["node"]["id"].as_str().unwrap().into(),
        created["node_auth_token"].as_str().unwrap().into(),
    )
}
async fn set_package(state: &AppState, id: &str, package: &Package) {
    let mut tx = state.pool.begin().await.unwrap();
    save(&mut tx, id, package).await.unwrap();
    tx.commit().await.unwrap();
}
async fn usage(pool: &PgPool, id: &str) -> i64 {
    sqlx::query_scalar("SELECT usage_bytes FROM node_packages WHERE node_id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}
async fn generation(pool: &PgPool, id: &str) -> i64 {
    sqlx::query_scalar("SELECT generation FROM node_packages WHERE node_id=$1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[test]
fn month_end_leap_year_timezone_dst_and_exact_boundary() {
    let package = Package {
        cycle: "monthly".into(),
        reset_day: 31,
        ..Package::default()
    };
    assert_eq!(
        next_reset(&package, instant("2024-02-01T00:00:00Z")),
        Some(instant("2024-02-28T16:00:00Z"))
    );
    assert_eq!(
        next_reset(&package, instant("2025-02-01T00:00:00Z")),
        Some(instant("2025-02-27T16:00:00Z"))
    );
    assert_eq!(
        next_reset(&package, instant("2025-02-27T16:00:00Z")),
        Some(instant("2025-03-30T16:00:00Z"))
    );
    assert_eq!(
        next_reset(&package, instant("2025-12-31T00:00:00Z")),
        Some(instant("2026-01-30T16:00:00Z"))
    );
    let dst = Package {
        timezone: "America/New_York".into(),
        reset_day: 9,
        ..package
    };
    assert_eq!(
        next_reset(&dst, instant("2025-03-08T12:00:00Z")),
        Some(instant("2025-03-09T05:00:00Z"))
    );
    assert_eq!(
        next_reset(&dst, instant("2025-03-09T05:00:00Z")),
        Some(instant("2025-04-09T04:00:00Z"))
    );
}
#[test]
fn validates_config_counters_and_threshold_boundaries() {
    for bad in [
        json!({"quota_bytes":0}),
        json!({"quota_bytes":-1}),
        json!({"timezone":"invalid"}),
        json!({"interface":"eth0'; exit"}),
        json!({"reset_day":32}),
        json!({"traffic_warning_percent":100}),
        json!({"expiry_warning_days":366}),
        json!({"direction":"bad"}),
    ] {
        assert!(
            serde_json::from_value::<Package>(bad)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    assert!(parse_sample("11111111-1111-4111-8111-111111111111 eth0 5 8").is_ok());
    assert!(parse_sample("bad eth0 5 8").is_err());
    assert!(parse_sample("11111111-1111-4111-8111-111111111111 eth0 -1 8").is_err());
    let package = Package {
        quota_bytes: Some(i64::MAX),
        ..Package::default()
    };
    assert!(reasons(&package, i64::MAX - 1, Utc::now()).is_empty());
    assert_eq!(
        reasons(&package, i64::MAX, Utc::now()),
        vec!["quota_exhausted"]
    );
}
#[tokio::test]
async fn first_sample_deltas_duplicates_directions_and_counter_resets() {
    let (state, id, _) = fixture().await;
    for (direction, expected) in [("both", 30), ("tx", 10), ("rx", 20)] {
        set_package(
            &state,
            &id,
            &Package {
                quota_bytes: Some(1000),
                direction: direction.into(),
                ..Package::default()
            },
        )
        .await;
        sqlx::query("UPDATE node_packages SET usage_bytes=0,boot_id=NULL WHERE node_id=$1")
            .bind(&id)
            .execute(&state.pool)
            .await
            .unwrap();
        let generation = generation(&state.pool, &id).await;
        apply_sample(
            &state.pool,
            &id,
            generation,
            sample_value(1000, 2000),
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(usage(&state.pool, &id).await, 0);
        apply_sample(
            &state.pool,
            &id,
            generation,
            sample_value(1010, 2020),
            Utc::now(),
        )
        .await
        .unwrap();
        apply_sample(
            &state.pool,
            &id,
            generation,
            sample_value(1010, 2020),
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(usage(&state.pool, &id).await, expected);
        let mut changed = sample_value(3000, 4000);
        changed.boot = Uuid::new_v4().to_string();
        apply_sample(&state.pool, &id, generation, changed, Utc::now())
            .await
            .unwrap();
        assert_eq!(usage(&state.pool, &id).await, expected);
        apply_sample(
            &state.pool,
            &id,
            generation,
            sample_value(10, 20),
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(usage(&state.pool, &id).await, expected);
        let mut changed = sample_value(100, 200);
        changed.interface = "ens3".into();
        apply_sample(&state.pool, &id, generation, changed, Utc::now())
            .await
            .unwrap();
        assert_eq!(usage(&state.pool, &id).await, expected);
    }
}
#[tokio::test]
async fn correction_invalidates_inflight_samples_and_revisions_without_sync() {
    let (state, id, _) = fixture().await;
    set_package(
        &state,
        &id,
        &Package {
            quota_bytes: Some(1000),
            ..Package::default()
        },
    )
    .await;
    let old_generation = generation(&state.pool, &id).await;
    apply_sample(
        &state.pool,
        &id,
        old_generation,
        sample_value(100, 100),
        Utc::now(),
    )
    .await
    .unwrap();
    let _ = update_usage(
        State(state.clone()),
        Path(id.clone()),
        Json(UsageUpdate {
            expected_revision: 1,
            usage_bytes: 500,
            reset: false,
        }),
    )
    .await
    .unwrap();
    apply_sample(
        &state.pool,
        &id,
        old_generation,
        sample_value(300, 300),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(usage(&state.pool, &id).await, 500);
    let new_generation = generation(&state.pool, &id).await;
    apply_sample(
        &state.pool,
        &id,
        new_generation,
        sample_value(350, 350),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(usage(&state.pool, &id).await, 500);
    apply_sample(
        &state.pool,
        &id,
        new_generation,
        sample_value(360, 360),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(usage(&state.pool, &id).await, 520);
    assert!(
        update_usage(
            State(state.clone()),
            Path(id.clone()),
            Json(UsageUpdate {
                expected_revision: 1,
                usage_bytes: 0,
                reset: true
            })
        )
        .await
        .is_err()
    );
    let versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM config_versions WHERE node_id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(versions, 2);
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE node_id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0);
}
#[tokio::test]
async fn warnings_deduplicate_and_monthly_rollover_catches_up_after_downtime() {
    let (state, id, _) = fixture().await;
    let package = Package {
        quota_bytes: Some(1000),
        cycle: "monthly".into(),
        ..Package::default()
    };
    set_package(&state, &id, &package).await;
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE node_packages SET usage_bytes=799 WHERE node_id=$1")
        .bind(&id)
        .execute(&mut *tx)
        .await
        .unwrap();
    evaluate(&mut tx, &id, Utc::now()).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM node_alerts WHERE node_id=$1")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 0);
    sqlx::query("UPDATE node_packages SET usage_bytes=800 WHERE node_id=$1")
        .bind(&id)
        .execute(&mut *tx)
        .await
        .unwrap();
    evaluate(&mut tx, &id, Utc::now()).await.unwrap();
    evaluate(&mut tx, &id, Utc::now()).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM node_alerts WHERE node_id=$1")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(count, 1);
    sqlx::query("UPDATE node_packages SET next_reset_at='2020-01-01T00:00:00Z',usage_bytes=1000 WHERE node_id=$1").bind(&id).execute(&mut *tx).await.unwrap();
    evaluate(&mut tx, &id, Utc::now()).await.unwrap();
    let row = sqlx::query("SELECT usage_bytes,next_reset_at FROM node_packages WHERE node_id=$1")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("usage_bytes"), 0);
    assert!(row.get::<DateTime<Utc>, _>("next_reset_at") > Utc::now());
    tx.commit().await.unwrap();
    assert!(!restricted(&state.pool, &id).await.unwrap());
    assert!(
        snapshot(&state.pool, &id).await.unwrap().1["alerts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn expiry_auth_is_node_local_probe_survives_and_renewal_cancels_old_kicks() {
    let (state, id, token) = fixture().await;
    sqlx::query(
        "INSERT INTO users(id,name,created_at,updated_at) VALUES ('user','User',now(),now())",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let credential = "credential";
    sqlx::query("WITH assignment_fixture(user_id,node_id,credential_hash,credential_enc,created_at) AS MATERIALIZED (VALUES ('user',$1,$2,'unused',now())), password_fixture AS (INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at) SELECT 'connection:'||user_id,user_id,'connection',credential_hash,credential_enc,now() FROM (SELECT DISTINCT ON(user_id) user_id,credential_hash,credential_enc FROM assignment_fixture ORDER BY user_id) c ON CONFLICT(user_id,kind) WHERE revoked_at IS NULL DO UPDATE SET secret_hash=EXCLUDED.secret_hash,secret_enc=EXCLUDED.secret_enc), fixture_groups AS (INSERT INTO authorization_groups(id,name,created_at,updated_at) SELECT 'fixture-'||md5(user_id||chr(31)||node_id),'Fixture authorization',now(),now() FROM assignment_fixture ON CONFLICT(id) DO UPDATE SET name=EXCLUDED.name RETURNING id), fixture_members AS (INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT g.id,a.user_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING), fixture_nodes AS (INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT g.id,a.node_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING) INSERT INTO node_assignments(user_id,node_id,created_at) SELECT user_id,node_id,created_at FROM assignment_fixture").bind(&id).bind(crate::security::token_digest(credential)).execute(&state.pool).await.unwrap();
    let request = || {
        Json(
            serde_json::from_value(json!({"addr":"127.0.0.1:1","auth":credential,"tx":0})).unwrap(),
        )
    };
    let auth = crate::api::subscriptions::hy2_auth(
        State(state.clone()),
        Path((id.clone(), token.clone())),
        request(),
    )
    .await
    .unwrap();
    assert_eq!(serde_json::to_value(auth.0).unwrap()["ok"], true);
    set_package(
        &state,
        &id,
        &Package {
            expires_at: Some(Utc::now() - chrono::Duration::seconds(1)),
            ..Package::default()
        },
    )
    .await;
    let auth = crate::api::subscriptions::hy2_auth(
        State(state.clone()),
        Path((id.clone(), token.clone())),
        request(),
    )
    .await
    .unwrap();
    assert_eq!(serde_json::to_value(auth.0).unwrap()["ok"], false);
    let (_,Json(other))=crate::api::nodes::create(State(state.clone()),Json(crate::credentials::test_node_request(&state,json!({
            "name":"Unlimited node","ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"unused","public_host":"other.example.test","public_port":443,"listen_addr":":443"
        })).await)).await.unwrap();
    let other_id = other["node"]["id"].as_str().unwrap();
    sqlx::query("WITH assignment_fixture(user_id,node_id,credential_hash,credential_enc,created_at) AS MATERIALIZED (SELECT a.user_id,$1,u.secret_hash,u.secret_enc,a.created_at FROM node_assignments a JOIN user_credentials u ON u.user_id=a.user_id AND u.kind='connection' AND u.revoked_at IS NULL WHERE a.node_id=$2), password_fixture AS (INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at) SELECT 'connection:'||user_id,user_id,'connection',credential_hash,credential_enc,now() FROM (SELECT DISTINCT ON(user_id) user_id,credential_hash,credential_enc FROM assignment_fixture ORDER BY user_id) c ON CONFLICT(user_id,kind) WHERE revoked_at IS NULL DO UPDATE SET secret_hash=EXCLUDED.secret_hash,secret_enc=EXCLUDED.secret_enc), fixture_groups AS (INSERT INTO authorization_groups(id,name,created_at,updated_at) SELECT 'fixture-'||md5(user_id||chr(31)||node_id),'Fixture authorization',now(),now() FROM assignment_fixture ON CONFLICT(id) DO UPDATE SET name=EXCLUDED.name RETURNING id), fixture_members AS (INSERT INTO authorization_group_users(group_id,user_id,created_at) SELECT g.id,a.user_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING), fixture_nodes AS (INSERT INTO authorization_group_nodes(group_id,node_id,created_at) SELECT g.id,a.node_id,now() FROM assignment_fixture a JOIN fixture_groups g ON g.id='fixture-'||md5(a.user_id||chr(31)||a.node_id) ON CONFLICT DO NOTHING) INSERT INTO node_assignments(user_id,node_id,created_at) SELECT user_id,node_id,created_at FROM assignment_fixture")
            .bind(other_id).bind(&id).execute(&state.pool).await.unwrap();
    let other_auth = crate::api::subscriptions::hy2_auth(
        State(state.clone()),
        Path((
            other_id.into(),
            other["node_auth_token"].as_str().unwrap().into(),
        )),
        request(),
    )
    .await
    .unwrap();
    assert_eq!(serde_json::to_value(other_auth.0).unwrap()["ok"], true);
    sqlx::query("UPDATE user_credentials SET secret_enc=$1 WHERE user_id='user' AND kind='connection' AND revoked_at IS NULL")
            .bind(state.secrets.encrypt(credential).unwrap())
            .execute(&state.pool)
            .await
            .unwrap();
    sqlx::query("UPDATE nodes SET deployed_revision=1,deployed_config_enc=$1 WHERE id IN ($2,$3)")
        .bind(state.secrets.encrypt("{}").unwrap())
        .bind(&id)
        .bind(other_id)
        .execute(&state.pool)
        .await
        .unwrap();
    let subscriptions = crate::api::subscriptions::subscription_node_count(&state, "user")
        .await
        .unwrap();
    assert_eq!(subscriptions, 1);
    maintenance::drain_for_test(&state.pool).await;
    let jobs: Vec<(String, String)> =
        sqlx::query_as("SELECT node_id,status FROM jobs WHERE kind='kick'")
            .fetch_all(&state.pool)
            .await
            .unwrap();
    assert_eq!(jobs, vec![(id.clone(), "queued".into())]);
    let probe_work = crate::proxy_monitor::token_tests::install_fixture(&state, &id, "probe").await;
    let probe = crate::api::subscriptions::hy2_auth(
        State(state.clone()),
        Path((id.clone(), token.clone())),
        Json(serde_json::from_value(json!({"addr":"","auth":"probe","tx":0})).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(serde_json::to_value(probe.0).unwrap()["ok"], true);
    crate::background::finish(&state.pool, &probe_work, true)
        .await
        .unwrap();
    set_package(&state, &id, &Package::default()).await;
    maintenance::drain_for_test(&state.pool).await;
    assert!(!restricted(&state.pool, &id).await.unwrap());
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE node_id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, "cancelled");
    let auth =
        crate::api::subscriptions::hy2_auth(State(state.clone()), Path((id, token)), request())
            .await
            .unwrap();
    assert_eq!(serde_json::to_value(auth.0).unwrap()["ok"], true);
}
#[tokio::test]
async fn package_patch_preserves_usage_clears_limits_and_never_queues_sync() {
    let (state, id, _) = fixture().await;
    let package = Package {
        quota_bytes: Some(1000),
        ..Package::default()
    };
    let response = crate::api::nodes::patch(
        State(state.clone()),
        Path(id.clone()),
        Json(serde_json::from_value(json!({"expected_revision":1,"package":package})).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(response.0["sync_job_queued"], false);
    assert_eq!(response.0["revision"], 2);
    let _ = update_usage(
        State(state.clone()),
        Path(id.clone()),
        Json(UsageUpdate {
            expected_revision: 2,
            usage_bytes: 900,
            reset: false,
        }),
    )
    .await
    .unwrap();
    let response = crate::api::nodes::patch(
        State(state.clone()),
        Path(id.clone()),
        Json(serde_json::from_value(json!({"expected_revision":3,"name":"Renamed"})).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(response.0["sync_job_queued"], false);
    assert_eq!(
        snapshot(&state.pool, &id).await.unwrap().0["quota_bytes"],
        1000
    );
    let _ = crate::api::nodes::patch(
        State(state.clone()),
        Path(id.clone()),
        Json(
            serde_json::from_value(json!({"expected_revision":4,"package":Package::default()}))
                .unwrap(),
        ),
    )
    .await
    .unwrap();
    let (package, status) = snapshot(&state.pool, &id).await.unwrap();
    assert!(package["quota_bytes"].is_null());
    assert_eq!(status["usage_bytes"], 900);
    assert_eq!(status["restricted"], false);
    assert!(status["alerts"].as_array().unwrap().is_empty());
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE node_id=$1")
        .bind(&id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 0);
    let invalid = crate::api::nodes::patch(
        State(state.clone()),
        Path(id),
        Json(
            serde_json::from_value(json!({"expected_revision":5,"package":{"quota_bytes":-1}}))
                .unwrap(),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(invalid.status, axum::http::StatusCode::BAD_REQUEST);
}
#[tokio::test]
async fn concurrent_corrections_allow_only_one_revision_and_failure_retains_usage() {
    let (state, id, _) = fixture().await;
    set_package(
        &state,
        &id,
        &Package {
            quota_bytes: Some(1000),
            ..Package::default()
        },
    )
    .await;
    let (a, b) = tokio::join!(
        update_usage(
            State(state.clone()),
            Path(id.clone()),
            Json(UsageUpdate {
                expected_revision: 1,
                usage_bytes: 500,
                reset: false
            })
        ),
        update_usage(
            State(state.clone()),
            Path(id.clone()),
            Json(UsageUpdate {
                expected_revision: 1,
                usage_bytes: 600,
                reset: false
            })
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let retained = usage(&state.pool, &id).await;
    record_failure(
        &state.pool,
        &id,
        generation(&state.pool, &id).await,
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(usage(&state.pool, &id).await, retained);
    assert_eq!(
        snapshot(&state.pool, &id).await.unwrap().1["gap_reason"],
        "network_sample_failed"
    );
}
#[tokio::test]
async fn monthly_reset_does_not_override_expiry_and_crossing_sample_charges_new_period() {
    let (state, id, _) = fixture().await;
    let package = Package {
        expires_at: Some(Utc::now() - chrono::Duration::days(1)),
        quota_bytes: Some(1000),
        cycle: "monthly".into(),
        ..Package::default()
    };
    set_package(&state, &id, &package).await;
    let generation = generation(&state.pool, &id).await;
    apply_sample(
        &state.pool,
        &id,
        generation,
        sample_value(100, 100),
        Utc::now(),
    )
    .await
    .unwrap();
    sqlx::query("UPDATE node_packages SET usage_bytes=1000,next_reset_at='2020-01-01T00:00:00Z' WHERE node_id=$1").bind(&id).execute(&state.pool).await.unwrap();
    apply_sample(
        &state.pool,
        &id,
        generation,
        sample_value(110, 120),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(usage(&state.pool, &id).await, 30);
    assert!(restricted(&state.pool, &id).await.unwrap());
    let (_, status) = snapshot(&state.pool, &id).await.unwrap();
    assert_eq!(status["reasons"], json!(["expired"]));
    apply_sample(
        &state.pool,
        &id,
        generation,
        sample_value(110, 120),
        Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(usage(&state.pool, &id).await, 30);
}
#[tokio::test]
async fn late_and_duplicate_network_results_never_regress_the_baseline_or_quality() {
    let (state, id, _) = fixture().await;
    set_package(
        &state,
        &id,
        &Package {
            quota_bytes: Some(1000),
            ..Package::default()
        },
    )
    .await;
    let generation = generation(&state.pool, &id).await;
    for (at, counter) in [
        ("2026-01-01T00:00:10Z", 100),
        ("2026-01-01T00:00:20Z", 200),
        ("2026-01-01T00:00:15Z", 150),
        ("2026-01-01T00:00:30Z", 250),
        ("2026-01-01T00:00:30Z", 250),
    ] {
        apply_sample(
            &state.pool,
            &id,
            generation,
            sample_value(counter, 0),
            instant(at),
        )
        .await
        .unwrap();
    }
    assert_eq!(usage(&state.pool, &id).await, 150);
    record_failure(
        &state.pool,
        &id,
        generation,
        instant("2026-01-01T00:00:25Z"),
    )
    .await
    .unwrap();
    let (counter, gap): (i64, Option<String>) =
        sqlx::query_as("SELECT tx_total,gap_reason FROM node_packages WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(counter, 250);
    assert!(gap.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM node_network_samples WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        3
    );
    for _ in 0..2 {
        record_failure(
            &state.pool,
            &id,
            generation,
            instant("2026-01-01T00:00:35Z"),
        )
        .await
        .unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM node_network_samples WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        4
    );
    apply_sample(
        &state.pool,
        &id,
        generation,
        sample_value(275, 0),
        instant("2026-01-01T00:00:40Z"),
    )
    .await
    .unwrap();
    record_failure(
        &state.pool,
        &id,
        generation,
        instant("2026-01-01T00:00:37Z"),
    )
    .await
    .unwrap();
    assert_eq!(usage(&state.pool, &id).await, 175);
    let gap: Option<String> =
        sqlx::query_scalar("SELECT gap_reason FROM node_packages WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(gap.is_none());
}

#[tokio::test]
async fn correction_fences_failed_samples_as_well_as_successful_ones() {
    let (state, id, _) = fixture().await;
    set_package(
        &state,
        &id,
        &Package {
            quota_bytes: Some(1000),
            ..Package::default()
        },
    )
    .await;
    let old = generation(&state.pool, &id).await;
    apply_sample(
        &state.pool,
        &id,
        old,
        sample_value(100, 0),
        instant("2026-01-01T00:00:10Z"),
    )
    .await
    .unwrap();
    let _ = update_usage(
        State(state.clone()),
        Path(id.clone()),
        Json(UsageUpdate {
            expected_revision: 1,
            usage_bytes: 500,
            reset: false,
        }),
    )
    .await
    .unwrap();
    record_failure(&state.pool, &id, old, Utc::now())
        .await
        .unwrap();
    apply_sample(
        &state.pool,
        &id,
        old,
        sample_value(200, 0),
        instant("2026-01-01T00:00:20Z"),
    )
    .await
    .unwrap();
    assert_eq!(usage(&state.pool, &id).await, 500);
    let gap: Option<String> =
        sqlx::query_scalar("SELECT gap_reason FROM node_packages WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(gap.as_deref(), Some("usage_corrected"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM node_network_samples WHERE node_id=$1")
            .bind(&id)
            .fetch_one(&state.pool)
            .await
            .unwrap(),
        1
    );
}
