//! Cache dependencies are committed database versions, never Redis-side authority.
use crate::{error::ApiError, state::AppState};
use sqlx::{PgPool, Row};
use std::time::Duration;
pub async fn version(pool: &PgPool, domains: &[&str]) -> Result<String, ApiError> {
    Ok(sqlx::query_scalar("SELECT coalesce(string_agg(domain||'='||generation::text,',' ORDER BY domain),'') FROM cache_domain_versions WHERE domain=ANY($1::text[])").bind(domains).fetch_one(pool).await?)
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
        return vec!["credentials", "jobs", "node_config", "dns"];
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
        let row=sqlx::query("SELECT j.status,(SELECT generation FROM job_cache_versions WHERE job_id=j.id) revision,md5(((to_jsonb(j)-ARRAY['lease_owner','lease_expires_at'])||jsonb_build_object('retry_job_id',(SELECT child.id FROM jobs child WHERE child.retry_of_job_id=j.id)))::text) digest FROM jobs j WHERE j.id=$1").bind(id).fetch_optional(&state.pool).await?;
        if let Some(row) = row
            && matches!(
                row.get::<String, _>("status").as_str(),
                "succeeded" | "failed" | "rolled_back" | "cancelled"
            )
        {
            return Ok((
                format!(
                    "job:{}:{}",
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
            .fetch_optional(&state.pool)
            .await?;
        if let Some(row) = row
            && row.get::<bool, _>("terminal")
        {
            return Ok((
                format!(
                    "batch:{}:{}",
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
    if parts.len() > 5 && parts[2] == "users" && parts[4] == "assignments" {
        parts[5] = ":node_id";
    }
    format!("/{}", parts.join("/"))
}
