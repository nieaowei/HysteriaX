use std::collections::HashSet;

use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use sqlx::{Acquire, PgConnection};
use uuid::Uuid;

use super::{Candidate, Counts, Cursor, DATA_SHARDS, PAGE};

pub(super) async fn publications(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    // A publication and a prefix prune serialize on the same stream row.
    // Never skip a locked mapping in the middle of a prefix.
    let locked: Option<bool> = sqlx::query_scalar(
        "SELECT true FROM event_publication_stream WHERE id FOR UPDATE SKIP LOCKED",
    )
    .fetch_optional(&mut *connection)
    .await?;
    if locked.is_none() {
        cursor.done = true;
        let mut counts = Counts::default();
        counts.skip("stream_locked", 1);
        return Ok(counts);
    }

    let after = cursor
        .after
        .as_ref()
        .map(|(_, id)| id.parse::<i64>())
        .transpose()?
        .unwrap_or(0);
    let page: Vec<(DateTime<Utc>, i64)> = sqlx::query_as(
        "SELECT published_at,publication_seq FROM event_publications
         WHERE publication_seq>$1 ORDER BY publication_seq LIMIT $2",
    )
    .bind(after)
    .bind(PAGE)
    .fetch_all(&mut *connection)
    .await?;
    let old: Vec<_> = page
        .iter()
        .take_while(|(at, _)| *at < cutoff)
        .map(|(_, seq)| *seq)
        .collect();
    cursor.done = old.len() < PAGE as usize;
    let mut counts = Counts {
        scanned: page.len(),
        eligible: old.len(),
        oldest: page.first().map(|row| row.0).filter(|at| *at < cutoff),
        ..Counts::default()
    };
    if let Some(last) = old.last() {
        cursor.after = Some((page[old.len() - 1].0, last.to_string()));
        counts.deleted =
            sqlx::query("DELETE FROM event_publications WHERE publication_seq=ANY($1)")
                .bind(&old)
                .execute(connection)
                .await?
                .rows_affected();
        ensure!(
            counts.deleted == old.len() as u64,
            "publication prefix changed while locked"
        );
    }
    Ok(counts)
}

const ONLINE_PROBE_LIMIT: usize = 512;
const ONLINE_MAX_CYCLES: usize = 64;
const ONLINE_DELETE_LIMIT: i64 = 4096;

pub(super) async fn online_samples(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
    shard: Option<u8>,
) -> Result<Counts> {
    let shard_filter = shard.map_or_else(
        || "true".to_owned(),
        |s| format!("(hashtext(cycle_id::text) & {}) = {s}", DATA_SHARDS - 1),
    );
    let probe: Vec<(DateTime<Utc>, String, i64)> = if let Some((after_at, after_id_str)) =
        &cursor.after
        && let Some(after_cycle) = cursor.extra.first()
    {
        let after_id: i64 = after_id_str.parse()?;
        let after_cycle = Uuid::parse_str(after_cycle)?;
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT sampled_at, cycle_id::text, id FROM online_samples
             WHERE {shard_filter} AND NOT pending AND sampled_at < $1
               AND (sampled_at, cycle_id, id) > ($2, $3, $4)
             ORDER BY sampled_at, cycle_id, id LIMIT $5"
        )))
        .bind(cutoff)
        .bind(after_at)
        .bind(after_cycle)
        .bind(after_id)
        .bind(ONLINE_PROBE_LIMIT as i64)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT sampled_at, cycle_id::text, id FROM online_samples
             WHERE {shard_filter} AND NOT pending AND sampled_at < $1
             ORDER BY sampled_at, cycle_id, id LIMIT $2"
        )))
        .bind(cutoff)
        .bind(ONLINE_PROBE_LIMIT as i64)
        .fetch_all(&mut *connection)
        .await?
    };

    if probe.is_empty() {
        cursor.done = true;
        return Ok(Counts::default());
    }

    let mut cycles = Vec::new();
    let mut seen = HashSet::new();
    let mut prefix_len = 0;
    for (_, cycle_str, _) in &probe {
        let cycle_id = Uuid::parse_str(cycle_str)?;
        if !seen.contains(&cycle_id) {
            if cycles.len() == ONLINE_MAX_CYCLES {
                break;
            }
            seen.insert(cycle_id);
            cycles.push(cycle_id);
        }
        prefix_len += 1;
    }

    let (locked_count, deleted_ids): (i64, Vec<i64>) = sqlx::query_as(
        "WITH locked AS MATERIALIZED (
            SELECT id FROM online_samples
            WHERE cycle_id = ANY($1::uuid[]) AND sampled_at < $2 AND NOT pending
            ORDER BY cycle_id, id LIMIT $3
            FOR UPDATE SKIP LOCKED
         ), deleted AS (
            DELETE FROM online_samples s USING locked l WHERE s.id = l.id RETURNING s.id
         )
         SELECT (SELECT count(*) FROM locked),
                coalesce((SELECT array_agg(id) FROM deleted), '{}'::bigint[])",
    )
    .bind(&cycles)
    .bind(cutoff)
    .bind(ONLINE_DELETE_LIMIT)
    .fetch_one(&mut *connection)
    .await?;

    let mut counts = Counts {
        oldest: probe.first().map(|row| row.0),
        ..Counts::default()
    };
    if locked_count == ONLINE_DELETE_LIMIT {
        // At least one cycle in this cohort may still have remaining expired rows.
        cursor.done = false;
        counts.scanned = deleted_ids.len().max(prefix_len);
        counts.eligible = deleted_ids.len();
        counts.deleted = deleted_ids.len() as u64;
        return Ok(counts);
    }

    let prefix = &probe[..prefix_len];
    let last = prefix.last().expect("non-empty probe prefix");
    cursor.after = Some((last.0, last.2.to_string()));
    cursor.extra = vec![last.1.clone()];
    cursor.done = probe.len() < ONLINE_PROBE_LIMIT && prefix_len == probe.len();

    let deleted_set: HashSet<i64> = deleted_ids.into_iter().collect();
    let skipped_prefix = prefix
        .iter()
        .filter(|(_, _, id)| !deleted_set.contains(id))
        .count();
    let extra_deleted = deleted_set
        .len()
        .saturating_sub(prefix_len - skipped_prefix);
    counts.scanned = prefix_len + extra_deleted;
    counts.eligible = deleted_set.len();
    counts.deleted = deleted_set.len() as u64;
    counts.skip("row_locked_or_gone", skipped_prefix);
    Ok(counts)
}

const MAX_ROOTS: i64 = 64;
const MAX_CHAIN: usize = 256;

async fn delete_job_chain_set(
    connection: &mut PgConnection,
    ids: &[String],
) -> Result<Option<u64>> {
    let mut savepoint = connection.begin().await?;
    let result = sqlx::query("DELETE FROM jobs WHERE id=ANY($1)")
        .bind(ids)
        .execute(&mut *savepoint)
        .await;
    match result {
        Ok(result) => {
            ensure!(
                result.rows_affected() == ids.len() as u64,
                "job chain changed while locked"
            );
            sqlx::query(
                "DELETE FROM resource_leases l
                 WHERE l.resource_key = ANY(SELECT 'job:' || id FROM unnest($1::text[]) id)
                   AND l.owner IS NULL AND l.expires_at IS NULL AND l.work_id IS NULL
                   AND NOT l.blocked
                   AND NOT EXISTS (
                       SELECT 1 FROM execution_operations o WHERE o.resource_key = l.resource_key
                   )",
            )
            .bind(ids)
            .execute(&mut *savepoint)
            .await?;
            savepoint.commit().await?;
            Ok(Some(result.rows_affected()))
        }
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|e| e.is_foreign_key_violation()) =>
        {
            savepoint.rollback().await?;
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) async fn job_chain(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let after_time = cursor.after.as_ref().map(|(time, _)| *time);
    let after_id = cursor.after.as_ref().map(|(_, id)| id.as_str());
    let roots: Vec<Candidate> = if let Some(after_time) = after_time {
        sqlx::query_as(
            "SELECT finished_at,id FROM jobs WHERE retry_of_job_id IS NULL AND finished_at<$1
             AND status IN ('succeeded','failed','rolled_back','cancelled')
             AND (finished_at,id)>($2,$3) ORDER BY finished_at,id LIMIT $4",
        )
        .bind(cutoff)
        .bind(after_time)
        .bind(after_id)
        .bind(MAX_ROOTS)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(
            "SELECT finished_at,id FROM jobs WHERE retry_of_job_id IS NULL AND finished_at<$1
             AND status IN ('succeeded','failed','rolled_back','cancelled')
             ORDER BY finished_at,id LIMIT $2",
        )
        .bind(cutoff)
        .bind(MAX_ROOTS)
        .fetch_all(&mut *connection)
        .await?
    };
    if roots.is_empty() {
        cursor.done = true;
        return Ok(Counts::default());
    }

    let mut counts = Counts {
        oldest: roots.first().map(|r| r.0),
        ..Counts::default()
    };
    let mut processed_roots = 0_usize;
    let mut eligible_chains: Vec<Vec<String>> = Vec::new();
    let mut total_eligible_jobs = 0_usize;

    for (at, root_id) in &roots {
        let ids: Vec<String> = sqlx::query_scalar(
            "WITH RECURSIVE chain(id,depth) AS (
                 SELECT id,1 FROM jobs WHERE id=$1 AND retry_of_job_id IS NULL
                 UNION ALL
                 SELECT j.id,c.depth+1 FROM chain c JOIN jobs j ON j.retry_of_job_id=c.id WHERE c.depth<=$2
             ) SELECT id FROM chain ORDER BY id",
        )
        .bind(root_id)
        .bind(MAX_CHAIN as i32)
        .fetch_all(&mut *connection)
        .await?;
        if total_eligible_jobs > 0 && total_eligible_jobs + ids.len() > MAX_CHAIN {
            break;
        }
        processed_roots += 1;
        cursor.after = Some((*at, root_id.clone()));
        if ids.is_empty() {
            counts.scanned += 1;
            counts.skip("job_gone", 1);
            continue;
        }
        if ids.len() > MAX_CHAIN {
            counts.scanned += 1;
            counts.skip("chain_too_long", 1);
            continue;
        }
        counts.scanned += ids.len();
        let locked: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM jobs WHERE id=ANY($1) ORDER BY id FOR UPDATE SKIP LOCKED",
        )
        .bind(&ids)
        .fetch_all(&mut *connection)
        .await?;
        if locked != ids {
            counts.skip("chain_locked_or_changed", ids.len());
            continue;
        }

        let reason: Option<String> = sqlx::query_scalar(include_str!("job_chain_protection.sql"))
            .bind(&ids)
            .bind(cutoff)
            .bind(root_id)
            .fetch_one(&mut *connection)
            .await?;
        if let Some(reason) = reason {
            counts.skip(&reason, ids.len());
            continue;
        }
        total_eligible_jobs += ids.len();
        eligible_chains.push(ids);
    }

    cursor.done = roots.len() < MAX_ROOTS as usize && processed_roots == roots.len();
    if eligible_chains.is_empty() {
        return Ok(counts);
    }

    let all_ids: Vec<String> = eligible_chains.iter().flatten().cloned().collect();
    if let Some(deleted) = delete_job_chain_set(connection, &all_ids).await? {
        counts.eligible = all_ids.len();
        counts.deleted = deleted;
        return Ok(counts);
    }

    for chain in eligible_chains {
        match delete_job_chain_set(connection, &chain).await? {
            Some(deleted) => {
                counts.eligible += chain.len();
                counts.deleted += deleted;
            }
            None => {
                counts.skip("additional_job_reference", chain.len());
            }
        }
    }
    Ok(counts)
}
