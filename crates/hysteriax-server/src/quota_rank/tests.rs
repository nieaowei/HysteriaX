use super::*;
use axum::{Json, extract::State};
use serde_json::json;

fn entry(usage: i64, quota: i64) -> Entry {
    Entry {
        id: "id".into(),
        name: "Name".into(),
        sort_name: "name".into(),
        usage_bytes: usage,
        quota,
        next_reset_at: None,
    }
}
#[test]
fn exact_lexicographic_keys_preserve_integer_ratio_order() {
    let now = Utc::now();
    let maximum = i64::MAX;
    // These adjacent ratios collapse to the same f64 score.
    assert!(entry(maximum, maximum).member(now) < entry(maximum - 1, maximum).member(now));
    assert!(entry(maximum - 1, maximum).member(now) < entry(maximum - 2, maximum - 1).member(now));
    assert_eq!(entry(234, 1000).member(now), entry(468, 2000).member(now));
    let mut rows = vec![
        entry(0, 1),
        entry(maximum, 1),
        entry(maximum, maximum),
        entry(1, maximum),
    ];
    for i in 1..200_i64 {
        rows.push(entry(maximum / i, maximum / (201 - i)));
    }
    let mut expected: Vec<_> = (0..rows.len()).collect();
    expected.sort_by(|a, b| {
        ((rows[*b].usage_bytes as u128) * (rows[*a].quota as u128))
            .cmp(&((rows[*a].usage_bytes as u128) * (rows[*b].quota as u128)))
    });
    let mut actual: Vec<_> = (0..rows.len()).collect();
    actual.sort_by_key(|i| rows[*i].member(now));
    assert_eq!(actual, expected);
}

async fn fixture() -> (AppState, Arc<RedisCache>, Vec<String>) {
    let url = std::env::var("TEST_REDIS_URL")
        .expect("TEST_REDIS_URL required for Redis integration tests");
    let pool = crate::db::test_pool_with_max_connections(4).await;
    let secrets =
        crate::security::SecretBox::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap();
    let mut state = AppState::new(pool, secrets);
    let redis = Arc::new(
        RedisCache::new(
            &url,
            format!("quota-test:{}", uuid::Uuid::new_v4()),
            state.secrets.clone(),
        )
        .unwrap(),
    );
    state.quota_rank = Arc::new(QuotaRank::new(Some(redis.clone())));
    Arc::get_mut(&mut state.caches).unwrap().redis = Some(redis.clone());
    let mut ids = Vec::new();
    for i in 0..12 {
        let request = crate::credentials::test_node_request(&state,json!({"name":format!("Node {i:02}"),"ssh_host":"127.0.0.1","ssh_port":22,"ssh_username":"root","ssh_auth_type":"password","ssh_secret":"unused","public_host":"example.test","public_port":443,"listen_addr":":443"})).await;
        let (_, Json(created)) = crate::api::nodes::create(State(state.clone()), Json(request))
            .await
            .unwrap();
        let id = created["node"]["id"].as_str().unwrap().to_owned();
        sqlx::query("UPDATE node_packages SET config='{\"quota_bytes\":1000}',usage_bytes=$1 WHERE node_id=$2")
                .bind(i as i64*50).bind(&id).execute(&state.pool).await.unwrap();
        ids.push(id);
    }
    (state, redis, ids)
}
async fn ids(state: &AppState, now: DateTime<Utc>) -> Vec<String> {
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await
        .unwrap();
    let result = state.quota_rank.ids_on(&mut tx, now).await.unwrap();
    let sql: Vec<String> = sqlx::query_scalar(include_str!("../overview/summary_quota.sql"))
        .bind(now)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        result, sql,
        "Redis must match the exact PostgreSQL ordering"
    );
    tx.commit().await.unwrap();
    result
}
async fn warm_without_package_access(state: &AppState, now: DateTime<Utc>) -> Vec<String> {
    let mut lock = state.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE node_packages IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await
        .unwrap();
    let mut reader = state.pool.begin().await.unwrap();
    sqlx::query("SET LOCAL statement_timeout='200ms'")
        .execute(&mut *reader)
        .await
        .unwrap();
    // This would time out if rank reads or sorts even one package row.
    let result = state.quota_rank.ids_on(&mut reader, now).await.unwrap();
    reader.commit().await.unwrap();
    lock.rollback().await.unwrap();
    result
}
async fn version(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT coalesce(sum(generation),0)::bigint FROM cache_bucket_versions WHERE domain='quota_rank'")
            .fetch_one(pool)
            .await
            .unwrap()
}
#[tokio::test]
async fn redis_rank_updates_resets_recovers_and_avoids_warm_package_scan() {
    if std::env::var("TEST_REDIS_URL").is_err() {
        return;
    }
    let (state, redis, nodes) = fixture().await;
    let now = Utc::now();
    let initial = ids(&state, now).await;
    assert_eq!(initial.len(), 10);
    assert_eq!(initial[0], nodes[11]);
    assert_eq!(warm_without_package_access(&state, now).await, initial);

    sqlx::query("UPDATE node_packages SET usage_bytes=950 WHERE node_id=$1")
        .bind(&nodes[0])
        .execute(&state.pool)
        .await
        .unwrap();
    state
        .quota_rank
        .update(&state.pool, &nodes[0], version(&state.pool).await)
        .await
        .unwrap();
    assert_eq!(
        warm_without_package_access(&state, Utc::now()).await[0],
        nodes[0]
    );
    ids(&state, Utc::now()).await;

    // Quota changes, unlimited plans, names and deletes all affect ranking.
    sqlx::query("UPDATE node_packages SET config='{\"quota_bytes\":2000}' WHERE node_id=$1")
        .bind(&nodes[0])
        .execute(&state.pool)
        .await
        .unwrap();
    state
        .quota_rank
        .update(&state.pool, &nodes[0], version(&state.pool).await)
        .await
        .unwrap();
    assert_eq!(
        warm_without_package_access(&state, Utc::now()).await[0],
        nodes[11]
    );
    sqlx::query("UPDATE node_packages SET config='{}' WHERE node_id=$1")
        .bind(&nodes[11])
        .execute(&state.pool)
        .await
        .unwrap();
    state
        .quota_rank
        .update(&state.pool, &nodes[11], version(&state.pool).await)
        .await
        .unwrap();
    assert!(
        !warm_without_package_access(&state, Utc::now())
            .await
            .contains(&nodes[11])
    );
    ids(&state, Utc::now()).await;

    let before = version(&state.pool).await;
    let mut rollback = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE node_packages SET usage_bytes=9999 WHERE node_id=$1")
        .bind(&nodes[1])
        .execute(&mut *rollback)
        .await
        .unwrap();
    rollback.rollback().await.unwrap();
    assert_eq!(before, version(&state.pool).await);

    let reset = Utc::now() + chrono::Duration::hours(1);
    sqlx::query("UPDATE node_packages SET next_reset_at=$1 WHERE node_id=$2")
        .bind(reset)
        .bind(&nodes[10])
        .execute(&state.pool)
        .await
        .unwrap();
    state
        .quota_rank
        .update(&state.pool, &nodes[10], version(&state.pool).await)
        .await
        .unwrap();
    assert_eq!(
        warm_without_package_access(&state, reset - chrono::Duration::seconds(1)).await[0],
        nodes[10]
    );
    assert_ne!(
        warm_without_package_access(&state, reset).await[0],
        nodes[10]
    );
    ids(&state, reset).await;
    let stored: i64 = sqlx::query_scalar("SELECT usage_bytes FROM node_packages WHERE node_id=$1")
        .bind(&nodes[10])
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(stored, 500, "Redis reset must not mutate billing records");

    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut connection = state.pool.acquire().await.unwrap();
    let (rank_version, _) = RankVersion::read(&mut connection).await.unwrap();
    drop(connection);
    let base = QuotaRank::base(&redis, &schema, &rank_version);
    for suffix in ["rank", "members", "deadlines", "versions", "meta"] {
        redis.delete(&format!("{base}:{suffix}")).await;
        ids(&state, Utc::now()).await;
    }
    // A missed update must cause a rebuild, never a stale Top 10.
    sqlx::query("UPDATE node_packages SET usage_bytes=10000 WHERE node_id=$1")
        .bind(&nodes[1])
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(ids(&state, Utc::now()).await[0], nodes[1]);
    let replacement = QuotaRank::new(Some(redis));
    let mut connection = state.pool.acquire().await.unwrap();
    assert_eq!(
        replacement
            .ids_on(&mut connection, Utc::now())
            .await
            .unwrap()[0],
        nodes[1]
    );
    drop(connection);
    sqlx::query("UPDATE nodes SET name='Changed name' WHERE id=$1")
        .bind(&nodes[1])
        .execute(&state.pool)
        .await
        .unwrap();
    state
        .quota_rank
        .update(&state.pool, &nodes[1], version(&state.pool).await)
        .await
        .unwrap();
    ids(&state, Utc::now()).await;
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(&nodes[1])
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(!ids(&state, Utc::now()).await.contains(&nodes[1]));
    sqlx::query("TRUNCATE nodes CASCADE")
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(ids(&state, Utc::now()).await.is_empty());
    assert!(
        warm_without_package_access(&state, Utc::now())
            .await
            .is_empty()
    );
}
#[tokio::test]
async fn redis_rank_listener_follows_committed_changes_and_outage_falls_back() {
    if std::env::var("TEST_REDIS_URL").is_err() {
        return;
    }
    let (mut state, redis, nodes) = fixture().await;
    ids(&state, Utc::now()).await;
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut connection = state.pool.acquire().await.unwrap();
    let (rank_version, _) = RankVersion::read(&mut connection).await.unwrap();
    drop(connection);
    let base = QuotaRank::base(&redis, &schema, &rank_version);
    let worker = tokio::spawn(run(state.clone()));
    // Wait until the listener has checked out its connection.
    tokio::time::sleep(Duration::from_millis(100)).await;
    sqlx::query("UPDATE node_packages SET usage_bytes=2000 WHERE node_id=$1")
        .bind(&nodes[0])
        .execute(&state.pool)
        .await
        .unwrap();
    let mut connection = state.pool.acquire().await.unwrap();
    let (expected_version, _) = RankVersion::read(&mut connection).await.unwrap();
    drop(connection);
    let expected = format!("{}|", expected_version.stamp());
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let meta: Option<String> = redis
                .command(redis::cmd("GET").arg(format!("{base}:meta")).to_owned())
                .await
                .flatten();
            if meta.is_some_and(|m| m.starts_with(&expected)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    worker.abort();
    let _ = worker.await;
    assert_eq!(
        warm_without_package_access(&state, Utc::now()).await[0],
        nodes[0]
    );
    let unavailable = Arc::new(
        RedisCache::new(
            "redis://127.0.0.1:1/",
            "offline".into(),
            state.secrets.clone(),
        )
        .unwrap(),
    );
    state.quota_rank = Arc::new(QuotaRank::new(Some(unavailable)));
    assert_eq!(ids(&state, Utc::now()).await[0], nodes[0]);
}
#[tokio::test]
async fn stale_snapshots_cannot_undo_deletion_even_after_meta_eviction() {
    if std::env::var("TEST_REDIS_URL").is_err() {
        return;
    }
    let (state, redis, nodes) = fixture().await;
    let initial = ids(&state, Utc::now()).await;
    let mut old = state.pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *old)
        .await
        .unwrap();
    let (old_version, schema) = RankVersion::read(&mut old).await.unwrap();
    sqlx::query("DELETE FROM nodes WHERE id=$1")
        .bind(&nodes[11])
        .execute(&state.pool)
        .await
        .unwrap();
    assert!(!ids(&state, Utc::now()).await.contains(&nodes[11]));
    // An unrelated process with an older DB snapshot can serve its own
    // snapshot, but must not replace the newer shared mirror.
    let other = QuotaRank::new(Some(redis.clone()));
    assert_eq!(other.ids_on(&mut old, Utc::now()).await.unwrap(), initial);
    let mut connection = state.pool.acquire().await.unwrap();
    let (current, current_schema) = RankVersion::read(&mut connection).await.unwrap();
    drop(connection);
    let members: Option<Vec<Vec<u8>>> = redis
        .command(QuotaRank::command(
            &redis,
            &current_schema,
            "read",
            &current,
            Utc::now(),
        ))
        .await
        .unwrap();
    assert!(
        !QuotaRank::decode(members.expect("new mirror must remain valid")).contains(&nodes[11])
    );
    let base = QuotaRank::base(&redis, &schema, &old_version);
    redis.delete(&format!("{base}:meta")).await;
    assert_eq!(other.ids_on(&mut old, Utc::now()).await.unwrap(), initial);
    let meta: Option<String> = redis
        .command(redis::cmd("GET").arg(format!("{base}:meta")).to_owned())
        .await
        .unwrap();
    assert!(
        meta.is_none(),
        "tombstone must reject an older rebuild before publishing metadata"
    );
    old.commit().await.unwrap();
    assert!(!ids(&state, Utc::now()).await.contains(&nodes[11]));
    assert!(
        !warm_without_package_access(&state, Utc::now())
            .await
            .contains(&nodes[11])
    );
}

#[tokio::test]
async fn lost_and_reordered_notifications_reconcile_every_entity_with_exact_bigint_versions() {
    if std::env::var("TEST_REDIS_URL").is_err() {
        return;
    }
    let (state, redis, nodes) = fixture().await;
    ids(&state, Utc::now()).await;
    // Deliberately exceed Lua's exact integer range. Redis must compare
    // padded decimal revisions, without tonumber on entity generations.
    sqlx::query(
        "UPDATE cache_entity_versions SET generation=9007199254740993 WHERE kind='quota_rank'",
    )
    .execute(&state.pool)
    .await
    .unwrap();
    let mut tx = state.pool.begin().await.unwrap();
    sqlx::query("UPDATE node_packages SET usage_bytes=2000 WHERE node_id=$1")
        .bind(&nodes[0])
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE node_packages SET usage_bytes=3000 WHERE node_id=$1")
        .bind(&nodes[1])
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    // Only the stale hint for one entity arrives. Reconciliation obtains
    // all committed changes from the DB vector rather than trusting it.
    state
        .quota_rank
        .update(&state.pool, &nodes[0], 0)
        .await
        .unwrap();
    assert_eq!(
        warm_without_package_access(&state, Utc::now()).await[0],
        nodes[1]
    );
    let mut connection = state.pool.acquire().await.unwrap();
    let (version, schema) = RankVersion::read(&mut connection).await.unwrap();
    drop(connection);
    let base = QuotaRank::base(&redis, &schema, &version);
    let revision: Option<String> = redis
        .command(
            redis::cmd("HGET")
                .arg(format!("{base}:versions"))
                .arg(&nodes[1])
                .to_owned(),
        )
        .await
        .unwrap();
    assert_eq!(revision.as_deref(), Some("00009007199254740994"));
    state
        .quota_rank
        .update(&state.pool, &nodes[1], 1)
        .await
        .unwrap();
    assert_eq!(ids(&state, Utc::now()).await[0], nodes[1]);
}
