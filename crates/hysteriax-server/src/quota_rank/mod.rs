//! Rebuildable Redis quota leaderboard. PostgreSQL remains the billing authority.
use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::{PgConnection, PgPool, postgres::PgListener};
use tokio::sync::Mutex;

use crate::{error::ApiError, redis_cache::RedisCache, state::AppState};

const ROWS: &str = "SELECT n.id,n.name,lower(n.name) AS sort_name,p.usage_bytes,(p.config->>'quota_bytes')::bigint AS quota,p.next_reset_at FROM nodes n JOIN node_packages p ON n.id=p.node_id WHERE (p.config->>'quota_bytes')::bigint>0";
const VERSION: &str = "SELECT array_agg(v.generation ORDER BY v.bucket),current_schema(),(SELECT epoch FROM cache_version_epoch WHERE id) FROM cache_bucket_versions v WHERE v.domain='quota_rank'";

#[derive(Clone, Debug, PartialEq, Eq)]
struct RankVersion {
    generations: Vec<i64>,
    epoch: String,
}
impl RankVersion {
    async fn read(connection: &mut PgConnection) -> Result<(Self, String), ApiError> {
        let (generations, schema, epoch): (Vec<i64>, String, String) =
            sqlx::query_as(VERSION).fetch_one(connection).await?;
        if generations.len() != 64 {
            return Err(ApiError::internal());
        }
        Ok((Self { generations, epoch }, schema))
    }
    fn frontier(&self) -> String {
        self.generations
            .iter()
            .map(|value| format!("{value:020}"))
            .collect::<Vec<_>>()
            .join(",")
    }
    fn stamp(&self) -> String {
        format!(
            "{}:{}",
            self.epoch,
            crate::security::token_digest(&self.frontier())
        )
    }
    fn parse_frontier(value: &str) -> Option<Vec<i64>> {
        let result: Vec<i64> = value
            .split(',')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        (result.len() == 64 && result.iter().all(|value| *value >= 0)).then_some(result)
    }
    fn dominates(&self, other: &[i64]) -> bool {
        self.generations
            .iter()
            .zip(other)
            .all(|(new, old)| new >= old)
    }
}

#[derive(sqlx::FromRow)]
struct Entry {
    id: String,
    name: String,
    sort_name: String,
    usage_bytes: i64,
    quota: i64,
    next_reset_at: Option<DateTime<Utc>>,
}
impl Entry {
    fn member(&self, now: DateTime<Utc>) -> Vec<u8> {
        let usage = if self.next_reset_at.is_some_and(|at| at <= now) {
            0
        } else {
            self.usage_bytes.max(0) as u128
        };
        let quota = self.quota as u128;
        // A 64-bit integer and 128 fractional bits distinguish every ratio of
        // nonnegative i64 counters. Complementing bytes gives descending usage.
        let mut bytes = ((usage / quota) as u64).to_be_bytes().to_vec();
        let mut remainder = usage % quota;
        for _ in 0..16 {
            remainder <<= 8;
            bytes.push((remainder / quota) as u8);
            remainder %= quota;
        }
        let mut member = String::with_capacity(48);
        use std::fmt::Write;
        for byte in bytes {
            write!(member, "{:02x}", !byte).unwrap();
        }
        let mut member = member.into_bytes();
        for field in [&self.sort_name, &self.name, &self.id] {
            member.push(0);
            member.extend_from_slice(field.as_bytes());
        }
        member
    }
    fn args(&self, command: &mut redis::Cmd, now: DateTime<Utc>, generation: i64) {
        let deadline = self
            .next_reset_at
            .filter(|at| *at > now)
            .map(|at| at.timestamp_micros().to_string())
            .unwrap_or_default();
        command
            .arg(&self.id)
            .arg(format!("{generation:020}"))
            .arg(self.member(now))
            .arg(deadline);
    }
}

pub struct QuotaRank {
    redis: Option<Arc<RedisCache>>,
    gate: Mutex<()>,
}
impl QuotaRank {
    pub fn new(redis: Option<Arc<RedisCache>>) -> Self {
        Self {
            redis,
            gate: Mutex::new(()),
        }
    }
    fn base(redis: &RedisCache, schema: &str, version: &RankVersion) -> String {
        redis.key(
            "quota_rank",
            &format!("exact-v2:{schema}:{}", version.epoch),
        )
    }
    fn command(
        redis: &RedisCache,
        schema: &str,
        mode: &str,
        version: &RankVersion,
        now: DateTime<Utc>,
    ) -> redis::Cmd {
        let base = Self::base(redis, schema, version);
        let mut command = redis::cmd("EVAL");
        command.arg(include_str!("quota_rank.lua")).arg(5);
        for suffix in ["meta", "rank", "members", "deadlines", "versions"] {
            command.arg(format!("{base}:{suffix}"));
        }
        command
            .arg(mode)
            .arg(version.stamp())
            .arg(now.timestamp_micros())
            .arg(version.frontier());
        command
    }
    fn decode(members: Vec<Vec<u8>>) -> Vec<String> {
        members
            .into_iter()
            .filter_map(|member| {
                member
                    .rsplit(|byte| *byte == 0)
                    .next()
                    .and_then(|id| String::from_utf8(id.to_vec()).ok())
            })
            .collect()
    }
    /// The caller owns a read snapshot. Version metadata, changed entities and
    /// values all come from that same snapshot; Redis is only an exact mirror.
    pub async fn ids_on(
        &self,
        connection: &mut PgConnection,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, ApiError> {
        let Some(redis) = &self.redis else {
            return Self::database_ids(connection, now).await;
        };
        if !redis.available() {
            return Self::database_ids(connection, now).await;
        }
        let _guard = self.gate.lock().await;
        let (version, schema) = RankVersion::read(connection).await?;
        if let Some(Some(members)) = redis
            .command::<Option<Vec<Vec<u8>>>>(Self::command(redis, &schema, "read", &version, now))
            .await
        {
            return Ok(Self::decode(members));
        }
        let basis = redis
            .command::<Option<String>>(Self::command(redis, &schema, "basis", &version, now))
            .await
            .flatten();
        let frontier = basis.as_deref().and_then(RankVersion::parse_frontier);
        if frontier
            .as_ref()
            .is_some_and(|previous| !version.dominates(previous))
        {
            return Self::database_ids(connection, now).await;
        }
        let changes: Vec<(String, i64)> = if let Some(previous) = &frontier {
            sqlx::query_as("SELECT v.entity_id,v.generation FROM cache_entity_versions v JOIN unnest($1::bigint[]) WITH ORDINALITY f(generation,bucket) ON v.bucket=f.bucket-1 WHERE v.kind='quota_rank' AND v.bucket_generation>f.generation ORDER BY v.entity_id")
                .bind(previous).fetch_all(&mut *connection).await?
        } else {
            sqlx::query_as("SELECT entity_id,generation FROM cache_entity_versions WHERE kind='quota_rank' ORDER BY entity_id")
                .fetch_all(&mut *connection).await?
        };
        let ids: Vec<String> = changes.iter().map(|(id, _)| id.clone()).collect();
        let rows: Vec<Entry> = if frontier.is_some() {
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "{ROWS} AND n.id=ANY($1::text[])"
            )))
            .bind(&ids)
            .fetch_all(&mut *connection)
            .await?
        } else {
            sqlx::query_as(ROWS).fetch_all(&mut *connection).await?
        };
        let entries: std::collections::BTreeMap<_, _> =
            rows.iter().map(|row| (row.id.as_str(), row)).collect();
        let mode = if frontier.is_some() {
            "apply"
        } else {
            "rebuild"
        };
        let mut command = Self::command(redis, &schema, mode, &version, now);
        if let Some(basis) = basis
            && frontier.is_some()
        {
            command.arg(basis);
        }
        for (id, generation) in changes {
            if let Some(entry) = entries.get(id.as_str()) {
                entry.args(&mut command, now, generation);
            } else {
                command
                    .arg(id)
                    .arg(format!("{generation:020}"))
                    .arg("")
                    .arg("");
            }
        }
        let _: Option<i64> = redis.command(command).await;
        if let Some(Some(members)) = redis
            .command::<Option<Vec<Vec<u8>>>>(Self::command(redis, &schema, "read", &version, now))
            .await
        {
            return Ok(Self::decode(members));
        }
        Self::database_ids(connection, now).await
    }
    async fn database_ids(
        connection: &mut PgConnection,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, ApiError> {
        Ok(
            sqlx::query_scalar(include_str!("../overview/summary_quota.sql"))
                .bind(now)
                .fetch_all(connection)
                .await?,
        )
    }
    async fn update(&self, pool: &PgPool, _id: &str, _version: i64) -> Result<(), ApiError> {
        // A notification only wakes reconciliation. Never advance a vector on
        // the assumption that unrelated notifications arrived in order.
        self.refresh(pool).await
    }
    async fn refresh(&self, pool: &PgPool) -> Result<(), ApiError> {
        let mut tx = pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *tx)
            .await?;
        self.ids_on(&mut tx, Utc::now()).await?;
        tx.commit().await?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct Change {
    schema: String,
    id: Option<String>,
    version: i64,
}

pub async fn run(state: AppState) {
    if state.caches.redis.is_none() {
        return;
    }
    loop {
        if let Err(error) = listen(&state).await {
            tracing::warn!(%error,"quota leaderboard listener reconnecting");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
async fn listen(state: &AppState) -> anyhow::Result<()> {
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&state.pool)
        .await?;
    let mut listener = PgListener::connect_with(&state.pool).await?;
    listener.listen("hysteriax_quota_rank").await?;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            notification = listener.recv() => {
                let notification = notification?;
                if let Ok(change) = serde_json::from_str::<Change>(notification.payload())
                    && change.schema == schema {
                    if let Some(id) = change.id {
                        state.quota_rank.update(&state.pool,&id,change.version).await
                            .map_err(|e| anyhow::anyhow!("{}",e.message))?;
                    } else {
                        state.quota_rank.refresh(&state.pool).await
                            .map_err(|e| anyhow::anyhow!("{}",e.message))?;
                    }
                }
            }
            _ = tick.tick() => {
                // The 64-row version vector detects lost notifications and
                // evicted keys. Redis applies due resets before reading Top 10.
                if state.caches.redis.as_ref().is_some_and(|r| r.available()) {
                    state.quota_rank.refresh(&state.pool).await
                        .map_err(|e| anyhow::anyhow!("{}",e.message))?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
