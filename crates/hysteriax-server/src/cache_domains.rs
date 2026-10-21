//! Cache dependencies are committed database versions, never Redis-side authority.
use crate::{error::ApiError, state::AppState};
use sqlx::{PgPool, Row};
use std::time::Duration;
pub async fn version(pool: &PgPool, domains: &[&str]) -> Result<String, ApiError> {
    let mut connection = crate::read_snapshot::begin(pool).await?;
    version_on(&mut connection, domains).await
}

pub async fn version_on(
    connection: &mut sqlx::PgConnection,
    domains: &[&str],
) -> Result<String, ApiError> {
    let (epoch,vector):(String,String)=sqlx::query_as("SELECT e.epoch,coalesce((SELECT string_agg(v.domain||':'||v.bucket::text||'='||v.generation::text,',' ORDER BY v.domain,v.bucket) FROM cache_bucket_versions v WHERE v.domain=ANY($1::text[])),'') FROM cache_version_epoch e WHERE e.id")
        .bind(domains).fetch_one(connection).await?;
    Ok(format!(
        "b3:{epoch}:{}",
        crate::security::token_digest(&vector)
    ))
}

pub fn dependencies(path: &str, query: &[(String, String)]) -> Vec<&'static str> {
    if path == "/api/v1/overview/history" {
        let get = |key: &str| {
            query
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        let source = get("source").unwrap_or("users");
        let metrics = get("metrics").unwrap_or("all");
        let mut deps = vec!["node_config"];
        if metrics != "quality" {
            deps.push(if source == "network" {
                "network"
            } else {
                "traffic"
            });
            if source != "network" {
                deps.extend(["online", "gaps"]);
            }
        }
        if metrics != "traffic" {
            deps.extend(["online", "probe"]);
        }
        return deps;
    }
    if path == "/api/v1/overview/nodes" {
        return vec![
            "node_config",
            "nodes",
            "online",
            "probe",
            "network",
            "gaps",
            "jobs",
        ];
    }
    if path == "/api/v1/overview" {
        return vec![
            "node_config",
            "nodes",
            "user_config",
            "users",
            "online",
            "probe",
            "network",
            "gaps",
            "jobs",
        ];
    }
    if path.starts_with("/api/v1/dns/") {
        return vec!["dns", "node_config", "credentials"];
    }
    if path.starts_with("/api/v1/authorization-groups") {
        return vec!["groups", "user_config", "node_config"];
    }
    if path.starts_with("/api/v1/credential-batches") {
        return vec![
            "credentials",
            "jobs",
            "node_config",
            "dns",
            "users",
            "user_config",
            "groups",
        ];
    }
    if path.starts_with("/api/v1/credentials") {
        let mut d = vec!["credentials", "user_config", "node_config", "dns", "jobs"];
        if path == "/api/v1/credentials" {
            d.push("users");
        }
        return d;
    }
    if path.starts_with("/api/v1/users") {
        return vec![
            "user_config",
            "users",
            "node_config",
            "credentials",
            "groups",
            "jobs",
            "traffic",
            "gaps",
        ];
    }
    if path.starts_with("/api/v1/nodes") {
        if path.contains("/resources") {
            return vec!["resources", "node_config"];
        }
        return vec![
            "node_config",
            "nodes",
            "credentials",
            "resources",
            "dns",
            "jobs",
            "online",
            "probe",
            "network",
            "gaps",
        ];
    }
    if path.starts_with("/api/v1/jobs") {
        return vec!["jobs"];
    }
    if path == "/api/v1/audit" {
        return vec!["audit"];
    }
    vec![
        "node_config",
        "nodes",
        "user_config",
        "users",
        "credentials",
        "resources",
        "dns",
        "jobs",
        "audit",
        "traffic",
        "network",
        "online",
        "probe",
        "groups",
        "gaps",
    ]
}
pub async fn policy(
    state: &AppState,
    path: &str,
    query: &[(String, String)],
    ttl: Duration,
) -> Result<(String, Duration), ApiError> {
    if let Some(id) = path.strip_prefix("/api/v1/jobs/")
        && !id.contains('/')
    {
        let row=sqlx::query("SELECT j.status,(SELECT generation FROM job_cache_versions WHERE job_id=j.id) revision,md5(((to_jsonb(j)-ARRAY['lease_owner','lease_expires_at'])||jsonb_build_object('retry_job_id',(SELECT child.id FROM jobs child WHERE child.retry_of_job_id=j.id)))::text) digest FROM jobs j WHERE j.id=$1").bind(id).fetch_optional(&mut *crate::read_snapshot::begin(&state.pool).await?).await?;
        if let Some(row) = row
            && matches!(
                row.get::<String, _>("status").as_str(),
                "succeeded" | "failed" | "rolled_back" | "cancelled"
            )
        {
            return Ok((
                format!(
                    "{}:job:{}:{}",
                    version(&state.pool, &[]).await?,
                    row.get::<i64, _>("revision"),
                    row.get::<String, _>("digest")
                ),
                Duration::from_secs(300),
            ));
        }
    }
    if let Some(id) = path.strip_prefix("/api/v1/credential-batches/")
        && !id.contains('/')
    {
        let row = sqlx::query(include_str!("cache_batch_version.sql"))
            .bind(id)
            .fetch_optional(&mut *crate::read_snapshot::begin(&state.pool).await?)
            .await?;
        if let Some(row) = row
            && row.get::<bool, _>("terminal")
        {
            return Ok((
                format!(
                    "{}:batch:{}:{}",
                    version(&state.pool, &[]).await?,
                    row.get::<i64, _>("revision"),
                    row.get::<String, _>("digest")
                ),
                Duration::from_secs(300),
            ));
        }
    }
    let ttl = if path.starts_with("/api/v1/authorization-groups")
        || path.starts_with("/api/v1/dns/")
        || path.contains("/resources")
    {
        Duration::from_secs(60)
    } else {
        ttl
    };
    let mut revision = version(&state.pool, &dependencies(path, query)).await?;
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    let primary = match parts.as_slice() {
        ["api", "v1", "nodes", id] => Some(("nodes", *id)),
        ["api", "v1", "users", id] => Some(("users", *id)),
        ["api", "v1", "credentials", id] => Some(("credentials", *id)),
        ["api", "v1", "authorization-groups", id] => Some(("authorization_groups", *id)),
        ["api", "v1", "jobs", id] => Some(("jobs", *id)),
        ["api", "v1", "credential-batches", id] => Some(("credential_batches", *id)),
        ["api", "v1", "dns", "connections", id] => Some(("dns_connections", *id)),
        ["api", "v1", "dns", "records", id] => Some(("dns_records", *id)),
        _ => None,
    };
    if let Some((kind, id)) = primary {
        let entity: Option<(i64, bool)> = sqlx::query_as(
            "SELECT generation,deleted FROM cache_entity_versions WHERE kind=$1 AND entity_id=$2",
        )
        .bind(kind)
        .bind(id)
        .fetch_optional(&mut *crate::read_snapshot::begin(&state.pool).await?)
        .await?;
        revision.push_str(&format!(";entity={kind}:{id}:{entity:?}"));
    }
    if path == "/api/v1/overview" || path == "/api/v1/overview/nodes" {
        revision.push_str(";overview_schema=3");
    }
    if path == "/api/v1/overview" {
        revision.push_str(&format!(";quota_second={}", chrono::Utc::now().timestamp()));
    }
    if path == "/api/v1/overview/history" {
        revision.push_str(&format!(
            ";minute={}",
            chrono::Utc::now().timestamp().div_euclid(60)
        ));
    }
    Ok((revision, ttl))
}

pub fn route(path: &str) -> String {
    let mut parts: Vec<_> = path.trim_matches('/').split('/').collect();
    if parts.len() > 3
        && matches!(
            parts[2],
            "nodes"
                | "users"
                | "credentials"
                | "jobs"
                | "credential-batches"
                | "authorization-groups"
        )
    {
        parts[3] = ":id";
    }
    if parts.len() > 4 && parts[2] == "dns" {
        parts[4] = ":id";
    }
    if parts.len() > 5 && parts[2] == "nodes" && parts[4] == "resources" {
        parts[5] = ":resource_id";
    }
    format!("/{}", parts.join("/"))
}

#[cfg(test)]
mod bucket_tests {
    use super::*;

    async fn user(pool: &PgPool, id: &str) {
        sqlx::query("INSERT INTO users(id,name,created_at,updated_at) VALUES($1,$1,now(),now())")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
    async fn revision(pool: &PgPool, id: &str) -> (i64, bool) {
        sqlx::query_as("SELECT generation,deleted FROM cache_entity_versions WHERE kind='users' AND entity_id=$1")
            .bind(id).fetch_one(pool).await.unwrap()
    }
    async fn empty_pending(pool: &PgPool) {
        let count: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM cache_bucket_pending)+(SELECT count(*) FROM cache_entity_pending)+(SELECT count(*) FROM cache_flush_pending)")
            .fetch_one(pool).await.unwrap();
        assert_eq!(count, 0);
    }
    #[tokio::test]
    async fn commits_merge_invalidations_and_savepoints_do_not_publish_rolled_back_changes() {
        let pool = crate::db::test_pool_with_max_connections(2).await;
        user(&pool, "one").await;
        let before = version(&pool, &["users"]).await.unwrap();
        let initial = revision(&pool, "one").await.0;
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("UPDATE users SET usage_bytes=usage_bytes+1 WHERE id='one'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET usage_bytes=usage_bytes+1 WHERE id='one'")
            .execute(&mut *tx)
            .await
            .unwrap();
        assert_eq!(version(&pool, &["users"]).await.unwrap(), before);
        tx.commit().await.unwrap();
        assert_eq!(revision(&pool, "one").await, (initial + 1, false));
        let committed = version(&pool, &["users"]).await.unwrap();
        assert_ne!(before, committed);
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SAVEPOINT absent")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET name='rolled back' WHERE id='one'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("ROLLBACK TO absent")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(version(&pool, &["users"]).await.unwrap(), committed);
        // The transaction-local queue marker must roll back with its savepoint.
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SAVEPOINT absent")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET name='rolled back' WHERE id='one'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("ROLLBACK TO absent")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET name='committed' WHERE id='one'")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(revision(&pool, "one").await, (initial + 2, false));
        sqlx::query("DELETE FROM users WHERE id='one'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(revision(&pool, "one").await, (initial + 3, true));
        user(&pool, "one").await;
        assert_eq!(revision(&pool, "one").await, (initial + 4, false));
        empty_pending(&pool).await;
    }

    #[tokio::test]
    async fn usage_and_token_access_do_not_invalidate_configuration_directory() {
        let pool = crate::db::test_pool().await;
        user(&pool, "usage-only").await;
        sqlx::query("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('access','access','access',now())").execute(&pool).await.unwrap();
        let directory = version(&pool, &["directory"]).await.unwrap();
        let users = version(&pool, &["users"]).await.unwrap();
        let credentials = version(&pool, &["credentials"]).await.unwrap();
        sqlx::query("UPDATE users SET usage_bytes=1,updated_at=now() WHERE id='usage-only'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE admin_tokens SET last_used_at=now() WHERE id='access'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(version(&pool, &["directory"]).await.unwrap(), directory);
        assert_eq!(version(&pool, &["credentials"]).await.unwrap(), credentials);
        assert_ne!(version(&pool, &["users"]).await.unwrap(), users);
        sqlx::query("UPDATE users SET name='configuration' WHERE id='usage-only'")
            .execute(&pool)
            .await
            .unwrap();
        assert_ne!(version(&pool, &["directory"]).await.unwrap(), directory);
        empty_pending(&pool).await;
    }

    #[tokio::test]
    async fn independent_buckets_commit_and_collisions_increment_without_lost_versions() {
        let pool = crate::db::test_pool_with_max_connections(3).await;
        // Exercise the new commit mechanism without the legacy application lock,
        // which is intentionally still installed pending the remaining plan work.
        let keys: Vec<(String,i16)> = sqlx::query_as("SELECT id,hysteriax_cache_bucket(id) FROM (SELECT 'entity-'||generate_series(1,200) AS id) x")
            .fetch_all(&pool).await.unwrap();
        let (a, bucket) = &keys[0];
        let b = &keys.iter().find(|(_, v)| v != bucket).unwrap().0;
        let c = &keys.iter().skip(1).find(|(_, v)| v == bucket).unwrap().0;
        let mut held = pool.begin().await.unwrap();
        sqlx::query("SELECT hysteriax_cache_touch_domain('users',$1)")
            .bind(a)
            .execute(&mut *held)
            .await
            .unwrap();
        sqlx::query("SET CONSTRAINTS cache_commit_versions IMMEDIATE")
            .execute(&mut *held)
            .await
            .unwrap();
        let mut independent = pool.begin().await.unwrap();
        sqlx::query("SELECT hysteriax_cache_touch_domain('users',$1)")
            .bind(b)
            .execute(&mut *independent)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_millis(500), independent.commit())
            .await
            .unwrap()
            .unwrap();
        let mut collision = pool.begin().await.unwrap();
        sqlx::query("SELECT hysteriax_cache_touch_domain('users',$1)")
            .bind(c)
            .execute(&mut *collision)
            .await
            .unwrap();
        let commit = tokio::spawn(async move { collision.commit().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!commit.is_finished(), "same bucket must serialize commits");
        held.commit().await.unwrap();
        commit.await.unwrap().unwrap();
        let generation: i64 = sqlx::query_scalar(
            "SELECT generation FROM cache_bucket_versions WHERE domain='users' AND bucket=$1",
        )
        .bind(bucket)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(generation, 2);
        empty_pending(&pool).await;
    }

    #[tokio::test]
    async fn migration_preserves_source_rows_and_legacy_entity_versions() {
        let pool = crate::db::test_pool_through_version(1, 44).await;
        user(&pool, "historical-user").await;
        sqlx::query("INSERT INTO jobs(id,kind,status,stage,payload_json,available_at,created_at,updated_at) VALUES('historical-job','maintenance','succeeded','done','{}',now(),now(),now())")
            .execute(&pool).await.unwrap();
        let users: serde_json::Value =
            sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(u) ORDER BY id) FROM users u")
                .fetch_one(&pool)
                .await
                .unwrap();
        let jobs: serde_json::Value =
            sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j")
                .fetch_one(&pool)
                .await
                .unwrap();
        let legacy: i64 = sqlx::query_scalar(
            "SELECT generation FROM job_cache_versions WHERE job_id='historical-job'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        crate::db::run_migrations(&pool).await.unwrap();
        let users_after: serde_json::Value =
            sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(u) ORDER BY id) FROM users u")
                .fetch_one(&pool)
                .await
                .unwrap();
        let jobs_after: serde_json::Value =
            sqlx::query_scalar("SELECT jsonb_agg(to_jsonb(j) ORDER BY id) FROM jobs j")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(users, users_after);
        assert_eq!(jobs, jobs_after);
        let current: i64 = sqlx::query_scalar(
            "SELECT generation FROM job_cache_versions WHERE job_id='historical-job'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(legacy, current);
        let invalid:i64=sqlx::query_scalar("SELECT count(*) FROM (SELECT domain FROM cache_bucket_versions GROUP BY domain HAVING count(*)<>64) x").fetch_one(&pool).await.unwrap();
        assert_eq!(invalid, 0);
        let old_tables:i64=sqlx::query_scalar("SELECT count(*) FROM pg_class WHERE relnamespace=current_schema()::regnamespace AND relname IN ('cache_generation','cache_domain_versions','job_cache_versions','batch_cache_versions') AND relkind='r'").fetch_one(&pool).await.unwrap();
        assert_eq!(old_tables, 0);
        empty_pending(&pool).await;
    }
}
