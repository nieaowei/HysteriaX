//! Durable node removal: bounded history transactions followed by metadata deletion.
use crate::{
    db,
    deployment::{JobInput, JobOutput},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool, Row};
use std::time::{Duration, Instant};

pub async fn needs_background_cleanup(
    connection: &mut PgConnection,
    node: &str,
) -> Result<bool, sqlx::Error> {
    let resuming: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs j JOIN nodes n ON n.id=j.node_id WHERE j.node_id=$1 AND j.kind='record-remove' AND j.status='failed' AND n.state='delete_failed' AND j.target_revision=n.desired_revision AND NOT EXISTS(SELECT 1 FROM jobs child WHERE child.retry_of_job_id=j.id))")
        .bind(node).fetch_one(&mut *connection).await?;
    if resuming {
        return Ok(true);
    }
    // Empty history after partial cleanup must not trigger a full-table scan
    // while the request holds the global write lock.
    sqlx::query("SET LOCAL enable_seqscan=off")
        .execute(&mut *connection)
        .await?;
    sqlx::query("SET LOCAL plan_cache_mode=force_custom_plan")
        .execute(&mut *connection)
        .await?;
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM (
        SELECT 1 FROM traffic_records WHERE node_id=$1 UNION ALL
        SELECT 1 FROM node_network_samples WHERE node_id=$1 UNION ALL
        SELECT 1 FROM online_samples WHERE node_id=$1 UNION ALL
        SELECT 1 FROM proxy_probe_samples WHERE node_id=$1 UNION ALL
        SELECT 1 FROM traffic_minute_totals WHERE source='users' AND node_id=$1 UNION ALL
        SELECT 1 FROM traffic_minute_totals WHERE source='network' AND node_id=$1 UNION ALL
        SELECT 1 FROM probe_minute_totals WHERE node_id=$1 UNION ALL
        SELECT 1 FROM traffic_hour_totals WHERE source='users' AND node_id=$1 UNION ALL
        SELECT 1 FROM traffic_hour_totals WHERE source='network' AND node_id=$1
    ) history OFFSET 1000 LIMIT 1)",
    )
    .bind(node)
    .fetch_one(connection)
    .await
}

pub fn retryable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<sqlx::Error>().is_some_and(|error| {
            matches!(error, sqlx::Error::PoolTimedOut | sqlx::Error::Io(_))
                || error
                    .as_database_error()
                    .and_then(|error| error.code())
                    .is_some_and(|code| matches!(code.as_ref(), "55P03" | "57014" | "40P01"))
        })
    })
}

pub async fn run_job(pool: &PgPool, job: &JobInput) -> anyhow::Result<JobOutput> {
    let removed = purge_history(pool, job).await?;
    Ok(JobOutput {
        stage: "record_removed".into(),
        result: json!({"remote_uninstall":false,"remote_service_may_be_running":true,"history_cleaned":true,"removed_history_rows":removed}),
        node_state: None,
        deployed_revision: None,
        deployed_config: None,
        deployed_sha256: None,
        published_connection: None,
        delete_node: true,
    })
}

#[derive(Clone, Copy)]
struct History {
    table: &'static str,
    time: &'static str,
    id: &'static str,
    id_type: &'static str,
    source: Option<&'static str>,
}

#[derive(Deserialize, Serialize)]
struct CleanupCursor {
    table: String,
    source: Option<String>,
    at: DateTime<Utc>,
    id: String,
}

impl CleanupCursor {
    fn matches(&self, history: &History) -> bool {
        self.table == history.table && self.source.as_deref() == history.source
    }
}

const HISTORY: &[History] = &[
    History {
        table: "traffic_records",
        time: "sampled_at",
        id: "id",
        id_type: "text",
        source: None,
    },
    History {
        table: "traffic_sample_items",
        time: "sampled_at",
        id: "id",
        id_type: "text",
        source: None,
    },
    History {
        table: "traffic_sample_batches",
        time: "sampled_at",
        id: "sample_id",
        id_type: "text",
        source: None,
    },
    History {
        table: "node_network_samples",
        time: "sampled_at",
        id: "id",
        id_type: "bigint",
        source: None,
    },
    History {
        table: "proxy_probe_samples",
        time: "sampled_at",
        id: "id",
        id_type: "bigint",
        source: None,
    },
    History {
        table: "online_samples",
        time: "sampled_at",
        id: "id",
        id_type: "bigint",
        source: None,
    },
    History {
        table: "traffic_minute_totals",
        time: "minute",
        id: "source",
        id_type: "text",
        source: Some("users"),
    },
    History {
        table: "traffic_minute_totals",
        time: "minute",
        id: "source",
        id_type: "text",
        source: Some("network"),
    },
    History {
        table: "traffic_hour_totals",
        time: "hour",
        id: "source",
        id_type: "text",
        source: Some("users"),
    },
    History {
        table: "traffic_hour_totals",
        time: "hour",
        id: "source",
        id_type: "text",
        source: Some("network"),
    },
    History {
        table: "probe_minute_totals",
        time: "minute",
        id: "node_id",
        id_type: "text",
        source: None,
    },
    History {
        table: "user_node_usage",
        time: "sampled_at",
        id: "user_id",
        id_type: "text",
        source: None,
    },
];

pub async fn prepare_final_delete(pool: &PgPool, node: &str) -> Result<(), sqlx::Error> {
    // A purge leaves dead entries in every index, including indexes the batch
    // cursor did not use. Visit the FK lookup paths before taking the global
    // write lock, so cold heap reads and marking dead index entries cannot
    // monopolize that lock. Use the same scan settings as the final cascade.
    let mut previous = "";
    for history in HISTORY {
        if history.table == previous {
            continue;
        }
        previous = history.table;
        let mut tx = pool.begin().await?;
        for setting in [
            "SET LOCAL statement_timeout='30s'",
            "SET LOCAL enable_seqscan=off",
            "SET LOCAL enable_bitmapscan=off",
            "SET LOCAL plan_cache_mode=force_custom_plan",
            "SET LOCAL jit=off",
        ] {
            sqlx::query(setting).execute(&mut *tx).await?;
        }
        sqlx::query(&format!(
            "SELECT ctid FROM ONLY {} WHERE node_id=$1",
            history.table
        ))
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

pub async fn purge_history(pool: &PgPool, job: &JobInput) -> Result<i64, sqlx::Error> {
    crate::execution::require_job_scope(&job.id, job.node_id.as_deref())
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let node = job
        .node_id
        .as_deref()
        .ok_or_else(|| sqlx::Error::Protocol("node removal job has no node".into()))?;
    let saved: serde_json::Value =
        sqlx::query_scalar("SELECT coalesce(result_json,'{}'::jsonb) FROM jobs WHERE id=$1")
            .bind(&job.id)
            .fetch_one(pool)
            .await?;
    let mut total = saved["result"]["removed_history_rows"]
        .as_i64()
        .unwrap_or(0);
    let running: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=$1 AND status='running')")
            .bind(&job.id)
            .fetch_one(pool)
            .await?;
    if !running {
        return Ok(total);
    }
    let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM nodes WHERE id=$1 AND state='deleting' AND desired_revision=$2)")
        .bind(node).bind(job.target_revision).fetch_one(pool).await?;
    if !valid {
        return Err(sqlx::Error::Protocol(
            "node removal was superseded; reload before retrying".into(),
        ));
    }
    crate::traffic::finish_captured_for_removal(pool, node)
        .await
        .map_err(|error| match error.downcast::<sqlx::Error>() {
            Ok(error) => error,
            Err(error) => sqlx::Error::Protocol(error.to_string()),
        })?;
    let checkpoint: Option<CleanupCursor> = saved
        .pointer("/result/cleanup_cursor")
        .and_then(|value| serde_json::from_value(value.clone()).ok());
    let start = checkpoint
        .as_ref()
        .and_then(|cursor| HISTORY.iter().position(|history| cursor.matches(history)))
        .unwrap_or(0);
    let mut last_progress = Instant::now() - Duration::from_secs(2);
    for history in &HISTORY[start..] {
        let mut cursor = checkpoint
            .as_ref()
            .filter(|cursor| cursor.matches(history))
            .map(|cursor| (cursor.at, cursor.id.clone()));
        let mut limit = if history.table == "online_samples" {
            128_i64
        } else {
            512_i64
        };
        loop {
            let batch = purge_batch(pool, job, node, history, cursor.as_ref(), limit, total).await;
            let (rows, next) = match batch {
                Ok(batch) => batch,
                Err(error)
                    if matches!(error, sqlx::Error::PoolTimedOut)
                        || error
                            .as_database_error()
                            .and_then(|error| error.code())
                            .as_deref()
                            == Some("55P03") =>
                {
                    // Busy writers are not a failed deletion attempt. Keep the
                    // committed cursor and yield before acquiring a new batch.
                    let running: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM jobs WHERE id=$1 AND status='running')",
                    )
                    .bind(&job.id)
                    .fetch_one(pool)
                    .await?;
                    if !running {
                        return Ok(total);
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                Err(error)
                    if error
                        .as_database_error()
                        .and_then(|error| error.code())
                        .as_deref()
                        == Some("57014")
                        && limit > 32 =>
                {
                    limit /= 2;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if rows == 0 {
                break;
            }
            total += rows;
            cursor = next;
            // Rebuild small online batches before their dirty cycles accumulate
            // into another long global write transaction.
            if history.table == "online_samples" {
                refresh_analytics(pool, job).await?;
            }
            if last_progress.elapsed() >= Duration::from_secs(1) {
                if let Err(error) = crate::deployment::report_progress(
                    pool,
                    job,
                    "removing_history",
                    &format!("Removing node history; {total} rows removed."),
                )
                .await
                {
                    // The batch checkpoint is already durable. A delayed log
                    // event must not invalidate committed cleanup progress.
                    tracing::warn!(job_id=job.id, %error, "node cleanup progress event delayed");
                }
                last_progress = Instant::now();
            }
            // Give queued writes a chance between committed batches.
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    refresh_analytics(pool, job).await?;
    Ok(total)
}

async fn refresh_analytics(pool: &PgPool, job: &JobInput) -> Result<(), sqlx::Error> {
    loop {
        match crate::analytics::refresh(pool).await {
            Ok(()) => return Ok(()),
            Err(error)
                if matches!(error, sqlx::Error::PoolTimedOut)
                    || error
                        .as_database_error()
                        .and_then(|error| error.code())
                        .as_deref()
                        == Some("55P03") =>
            {
                let running: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM jobs WHERE id=$1 AND status='running')",
                )
                .bind(&job.id)
                .fetch_one(pool)
                .await?;
                if !running {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn purge_batch(
    pool: &PgPool,
    job: &JobInput,
    node: &str,
    history: &History,
    cursor: Option<&(DateTime<Utc>, String)>,
    limit: i64,
    total: i64,
) -> Result<(i64, Option<(DateTime<Utc>, String)>), sqlx::Error> {
    crate::execution::require_job_scope(&job.id, job.node_id.as_deref())
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let (job, node, history, cursor) = (job.clone(), node.to_owned(), *history, cursor.cloned());
    db::write(pool, move |tx| {
        let (job, node, cursor) = (job.clone(), node.clone(), cursor.clone());
        Box::pin(async move {
            purge_batch_on(tx, &job, &node, &history, cursor.as_ref(), limit, total).await
        })
    })
    .await
}

async fn purge_batch_on(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job: &JobInput,
    node: &str,
    history: &History,
    cursor: Option<&(DateTime<Utc>, String)>,
    limit: i64,
    total: i64,
) -> Result<(i64, Option<(DateTime<Utc>, String)>), sqlx::Error> {
    let Some(source) = sqlx::query("SELECT kind,node_id,target_revision,payload_json,attempts,resource_key FROM jobs WHERE id=$1 AND status='running' FOR UPDATE")
        .bind(&job.id).fetch_optional(&mut **tx).await? else { return Ok((0,None)); };
    if job.node_id.as_deref() != Some(node)
        || source.get::<String, _>("kind") != job.kind
        || source.get::<Option<String>, _>("node_id") != job.node_id
        || source.get::<Option<i64>, _>("target_revision") != job.target_revision
        || source.get::<serde_json::Value, _>("payload_json") != job.payload
        || source.get::<i64, _>("attempts") != job.attempts
    {
        return Err(sqlx::Error::Protocol(
            "node cleanup job source changed".into(),
        ));
    }
    if let Some(resource) = source.get::<Option<String>, _>("resource_key") {
        crate::execution::require_job_resource_scope(&job.id, &resource)
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    }
    let valid = sqlx::query_scalar::<_,bool>("SELECT true FROM nodes WHERE id=$1 AND state='deleting' AND desired_revision=$2 FOR NO KEY UPDATE")
        .bind(node).bind(job.target_revision).fetch_optional(&mut **tx).await?.unwrap_or(false);
    if !valid {
        return Err(sqlx::Error::Protocol(
            "node removal was superseded; reload before retrying".into(),
        ));
    }
    sqlx::query("SET LOCAL statement_timeout='2s'")
        .execute(&mut **tx)
        .await?;
    // Cleanup selectivity changes drastically as each batch commits. Avoid a
    // cached generic plan and always cost the matching node/seek index.
    sqlx::query("SET LOCAL plan_cache_mode=force_custom_plan")
        .execute(&mut **tx)
        .await?;
    // With skewed node ages, incremental sort badly underestimates the cost
    // of filtering a global time index before finding this node's first row.
    sqlx::query("SET LOCAL enable_incremental_sort=off")
        .execute(&mut **tx)
        .await?;
    sqlx::query("SELECT set_config('hysteriax.history_cleanup_node',$1,true)")
        .bind(node)
        .execute(&mut **tx)
        .await?;
    let source = history
        .source
        .map(|source| format!(" AND source='{source}'"))
        .unwrap_or_default();
    let seek = if cursor.is_some() {
        format!(
            " AND {time}<=$2 AND ({time},{id})<($2,$3::{kind})",
            time = history.time,
            id = history.id,
            kind = history.id_type
        )
    } else {
        String::new()
    };
    let limit_parameter = if cursor.is_some() { "$4" } else { "$2" };
    let statement = format!(
        "SELECT ctid::text AS location,{time} AS at_time,{id}::text AS cursor_id FROM {table} WHERE node_id=$1{source}{seek} ORDER BY {time} DESC,{id} DESC LIMIT {limit_parameter} FOR UPDATE",
        time = history.time,
        id = history.id,
        table = history.table
    );
    let mut query = sqlx::query(&statement).bind(node);
    if let Some((at, id)) = cursor {
        query = query.bind(at).bind(id);
    }
    let rows = query.bind(limit).fetch_all(&mut **tx).await?;
    if rows.is_empty() {
        return Ok((0, None));
    }
    let last = rows.last().unwrap();
    let next = (
        last.get::<DateTime<Utc>, _>("at_time"),
        last.get::<String, _>("cursor_id"),
    );
    let locations: Vec<String> = rows.iter().map(|row| row.get("location")).collect();
    let deleted = sqlx::query(&format!(
        "DELETE FROM {} WHERE ctid=ANY($1::text[]::tid[])",
        history.table
    ))
    .bind(locations)
    .execute(&mut **tx)
    .await?
    .rows_affected() as i64;
    let checkpoint = CleanupCursor {
        table: history.table.into(),
        source: history.source.map(str::to_owned),
        at: next.0,
        id: next.1.clone(),
    };
    sqlx::query("UPDATE jobs SET stage='removing_history',result_json=jsonb_build_object('stage','removing_history','result',jsonb_build_object('removed_history_rows',$1::bigint,'cleanup_cursor',$3::jsonb)),updated_at=clock_timestamp() WHERE id=$2 AND status='running'")
        .bind(total+deleted).bind(&job.id).bind(sqlx::types::Json(checkpoint)).execute(&mut **tx).await?;
    Ok((deleted, Some(next)))
}

#[cfg(test)]
mod ownership_tests;
