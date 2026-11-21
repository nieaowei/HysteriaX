use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use sqlx::{AssertSqlSafe, PgConnection};

use super::{Counts, Cursor, DATA_SHARDS, PAGE};

#[derive(Clone, Copy)]
pub(super) struct Rule {
    pub table: &'static str,
    pub days: i64,
    time: &'static str,
    key: &'static str,
    key_type: &'static str,
    page_filter: &'static str,
    node_lock: bool,
    sample_order: bool,
    protected: &'static str,
}

pub(super) const ITEMS: Rule = Rule {
    table: "traffic_sample_items",
    days: 14,
    time: "processed_at",
    key: "id",
    key_type: "text",
    page_filter: "processed_at IS NOT NULL",
    node_lock: true,
    sample_order: true,
    protected: "WHEN NOT EXISTS (
        SELECT 1 FROM traffic_sample_batches b WHERE b.sample_id=t.sample_id
        AND b.node_id=t.node_id AND b.state='complete' AND b.completed_at<$2
        AND b.processed_count=b.item_count
    ) THEN 'batch_unsettled_or_recent'",
};

pub(super) const RECORDS: Rule = Rule {
    table: "traffic_records",
    days: 60,
    time: "sampled_at",
    key: "id",
    key_type: "text",
    page_filter: "true",
    node_lock: true,
    sample_order: true,
    protected: "WHEN EXISTS (
        SELECT 1 FROM traffic_sample_batches b WHERE b.sample_id=t.sample_id
        AND (b.state<>'complete' OR b.completed_at IS NULL OR b.processed_count<>b.item_count)
    ) THEN 'batch_unsettled'
    WHEN EXISTS (
        SELECT 1 FROM background_work w WHERE w.id=t.sample_id AND w.state IN ('queued','running')
    ) THEN 'work_active'
    WHEN EXISTS (
        SELECT 1 FROM execution_operations o WHERE o.work_id=t.sample_id
        AND o.state IN ('dispatching','reconciling','needs_attention')
    ) THEN 'execution_pending'",
};

pub(super) const NETWORK: Rule = Rule {
    table: "node_network_samples",
    days: 60,
    time: "sampled_at",
    key: "id",
    key_type: "bigint",
    page_filter: "true",
    node_lock: true,
    sample_order: false,
    protected: "WHEN EXISTS (
        SELECT 1 FROM node_packages p WHERE p.node_id=t.node_id
        AND p.config->>'cycle'='monthly' AND p.period_id=t.period_id
    ) THEN 'current_monthly_period'
    WHEN EXISTS (
        SELECT 1 FROM background_work w WHERE w.node_id=t.node_id AND w.kind='network'
        AND w.state IN ('queued','running') AND (w.payload->>'captured_at')::timestamptz=t.sampled_at
    ) THEN 'work_active'",
};

pub(super) const BATCHES: Rule = Rule {
    table: "traffic_sample_batches",
    days: 60,
    time: "completed_at",
    key: "sample_id",
    key_type: "text",
    page_filter: "state='complete' AND completed_at IS NOT NULL",
    node_lock: true,
    sample_order: false,
    protected: "WHEN t.state<>'complete' OR t.processed_count<>t.item_count
                       OR (t.clear_requested_at IS NOT NULL AND t.payload_digest IS NULL)
                  THEN 'batch_unsettled'
    WHEN t.instance_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM traffic_sample_batches newer WHERE newer.node_id=t.node_id
        AND newer.state='complete' AND newer.sample_order>t.sample_order AND newer.instance_id IS NOT NULL
    ) THEN 'latest_instance'
    WHEN EXISTS (
        SELECT 1 FROM traffic_sample_items i WHERE i.sample_id=t.sample_id
    ) OR EXISTS (
        SELECT 1 FROM traffic_records r WHERE r.sample_id=t.sample_id AND r.sample_id IS NOT NULL
    ) THEN 'billing_reference'
    WHEN EXISTS (
        SELECT 1 FROM background_work w WHERE w.id=t.sample_id AND w.state IN ('queued','running')
    ) OR EXISTS (
        SELECT 1 FROM online_samples s WHERE s.node_id=t.node_id AND s.cycle_id=t.cycle_id AND s.pending
    ) THEN 'work_active'
    WHEN EXISTS (
        SELECT 1 FROM execution_operations o WHERE o.work_id=t.sample_id OR o.resource_key='work:'||t.sample_id
    ) OR EXISTS (
        SELECT 1 FROM resource_leases l WHERE l.work_id=t.sample_id
        OR (l.resource_key='work:'||t.sample_id AND (l.owner IS NOT NULL OR l.blocked))
    ) THEN 'execution_evidence'",
};

pub(super) const GAPS: Rule = Rule {
    table: "data_gaps",
    days: 60,
    time: "resolved_at",
    key: "id",
    key_type: "text",
    page_filter: "resolved_at IS NOT NULL",
    node_lock: false,
    sample_order: false,
    protected: "",
};

pub(super) const INSTANCES: Rule = Rule {
    table: "service_instances",
    days: 30,
    time: "heartbeat_at",
    key: "instance_id",
    key_type: "text",
    page_filter: "true",
    node_lock: false,
    sample_order: false,
    protected: "WHEN EXISTS (
        SELECT 1 FROM resource_leases l WHERE l.owner=t.instance_id
        AND (l.expires_at>clock_timestamp() OR l.blocked)
    ) OR EXISTS (
        SELECT 1 FROM execution_operations o WHERE o.owner=t.instance_id
        AND o.state IN ('dispatching','reconciling','needs_attention')
    ) THEN 'active_owner'",
};

pub(super) const DEPLOYMENT_TOKENS: Rule = Rule {
    table: "deployment_probe_tokens",
    days: 0,
    time: "expires_at",
    key: "node_id",
    key_type: "text",
    page_filter: "true",
    node_lock: false,
    sample_order: false,
    protected: "",
};

pub(super) const JOB_EVENTS: Rule = Rule {
    table: "job_events",
    days: 90,
    time: "created_at",
    key: "id",
    key_type: "bigint",
    page_filter: "true",
    node_lock: false,
    sample_order: false,
    protected: "WHEN EXISTS (SELECT 1 FROM event_publication_pending p WHERE p.event_id=t.id)
                  THEN 'event_pending'
    WHEN EXISTS (SELECT 1 FROM event_publications p WHERE p.event_id=t.id)
                  THEN 'event_replayable'",
};

pub(super) const AUDIT: Rule = Rule {
    table: "audit_records",
    days: 90,
    time: "created_at",
    key: "id",
    key_type: "text",
    page_filter: "true",
    node_lock: false,
    sample_order: false,
    protected: "",
};

pub(super) async fn page(
    connection: &mut PgConnection,
    rule: Rule,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
    shard: Option<u8>,
    batch_cutoff: Option<DateTime<Utc>>,
) -> Result<Counts> {
    // Fixed rule identifiers are internal constants. Bind all data, including
    // cursors; use the native key type so the candidate query can use its index.
    let Rule {
        table,
        time,
        key,
        key_type,
        page_filter,
        ..
    } = rule;
    let page_size = match table {
        "node_network_samples" => 2 * PAGE,
        "traffic_sample_items" => 4 * PAGE,
        "traffic_records" => 6 * PAGE,
        _ => PAGE,
    };
    let sample_key = "coalesce(sample_id,'')";
    let (sample, order) = if rule.sample_order {
        (sample_key, format!("{time},{sample_key},{key}"))
    } else {
        ("NULL::text", format!("{time},{key}"))
    };
    let continuation = match (&cursor.after, rule.sample_order) {
        (Some(_), true) => {
            format!("AND ({time},{sample_key},{key}) > ($3,$5,$4::text::{key_type})")
        }
        (Some(_), false) => format!("AND ({time},{key}) > ($3,$4::text::{key_type})"),
        (None, _) => String::new(),
    };
    let partition = shard.map_or_else(String::new, |shard| {
        format!("AND (hashtext(node_id) & {}) = {shard}", DATA_SHARDS - 1)
    });
    let node = if rule.node_lock {
        "node_id"
    } else {
        "NULL::text"
    };
    let scan = format!(
        "SELECT {time} AS at,{key} AS id,{sample} AS sample,{node} AS node_id FROM {table}
         WHERE {time}<$1 AND ({page_filter}) {partition} {continuation}
         ORDER BY {order} LIMIT $2"
    );
    let batch_parameter = batch_cutoff.map(|_| if cursor.after.is_some() { "$6" } else { "$3" });
    let mut query = sqlx::query_as(AssertSqlSafe(locked_page(rule, &scan, batch_parameter)))
        .bind(cutoff)
        .bind(page_size);
    if let Some((time, id)) = &cursor.after {
        query = query.bind(time).bind(id);
        if rule.sample_order {
            query = query.bind(cursor.sample.as_deref().unwrap_or(""));
        }
    }
    if let Some(batch_cutoff) = batch_cutoff {
        query = query.bind(batch_cutoff);
    }
    let candidates: Vec<(DateTime<Utc>, String, Option<String>, bool, bool)> =
        query.fetch_all(&mut *connection).await?;
    cursor.done = candidates.len() < page_size as usize;
    if let Some((time, id, sample, _, _)) = candidates.last() {
        cursor.after = Some((*time, id.clone()));
        cursor.sample = sample.clone();
    }
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|row| row.0),
        ..Counts::default()
    };
    let mut ids = Vec::new();
    for (_, id, sample, locked, batch_locked) in candidates {
        if locked {
            ids.push(id);
        }
        if let (true, Some(sample)) = (batch_locked, sample) {
            counts.locked_batches.push(sample);
        }
    }
    counts.locked_batches.sort_unstable();
    counts.locked_batches.dedup();
    counts.skip("row_or_parent_locked_or_gone", counts.scanned - ids.len());
    if ids.is_empty() {
        return Ok(counts);
    }
    // Separate statement after locking. Under READ COMMITTED, dependency
    // checks must see work committed before the locks were granted.
    let classified = classification(rule);
    let query = format!(
        "WITH classified AS MATERIALIZED ({classified}), deleted AS (
                DELETE FROM {table} t USING classified c
                WHERE t.{key}=c.id AND c.reason IS NULL RETURNING t.{key} AS id
             )
             SELECT c.reason, d.id IS NOT NULL FROM classified c LEFT JOIN deleted d USING (id)"
    );
    let reasons: Vec<(Option<String>, bool)> = sqlx::query_as(AssertSqlSafe(query))
        .bind(&ids)
        .bind(cutoff)
        .fetch_all(&mut *connection)
        .await?;
    counts.skip("row_gone", ids.len() - reasons.len());
    for (reason, deleted) in reasons {
        if let Some(reason) = reason {
            counts.skip(&reason, 1);
        } else {
            counts.eligible += 1;
            counts.deleted += u64::from(deleted);
        }
    }
    ensure!(
        counts.deleted == counts.eligible as u64,
        "history delete count changed for {table}"
    );
    Ok(counts)
}

fn classification(rule: Rule) -> String {
    let Rule {
        table,
        time,
        key,
        key_type,
        protected,
        ..
    } = rule;
    let node_state = if rule.node_lock {
        "WHEN NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id=t.node_id
              AND n.state NOT IN ('deleting','delete_failed')) THEN 'node_deleting'"
    } else {
        ""
    };
    format!(
        "SELECT t.{key} AS id, CASE WHEN t.{time} IS NULL OR t.{time}>=$2 THEN 'recent'
            {node_state} {protected} END AS reason
         FROM {table} t WHERE t.{key}=ANY($1::text[]::{key_type}[])"
    )
}

pub(super) async fn checkpoint(
    connection: &mut PgConnection,
    work_id: &str,
    payload: &serde_json::Value,
    cutoff: DateTime<Utc>,
    batches: &[String],
) -> Result<i64> {
    if batches.is_empty() {
        sqlx::query("UPDATE background_work SET payload=$2 WHERE id=$1")
            .bind(work_id)
            .bind(payload)
            .execute(connection)
            .await?;
        return Ok(0);
    }

    // The history page already holds every candidate batch lock. This fresh
    // statement sees the removal of source rows, and can safely check the
    // same remaining dependencies as the independent batch sweep.
    let classified = classification(BATCHES);
    Ok(sqlx::query_scalar(AssertSqlSafe(format!(
        "WITH classified AS MATERIALIZED ({classified}), removed AS (
            DELETE FROM traffic_sample_batches b USING classified c
            WHERE b.sample_id=c.id AND c.reason IS NULL RETURNING b.sample_id
         ), progress AS (UPDATE background_work SET payload=$4 WHERE id=$3)
         SELECT count(*) FROM removed"
    )))
    .bind(batches)
    .bind(cutoff)
    .bind(work_id)
    .bind(payload)
    .fetch_one(connection)
    .await?)
}

fn locked_page(rule: Rule, scan: &str, batch_parameter: Option<&str>) -> String {
    let Rule { table, key, .. } = rule;
    // Lock the node before any of its history, as sampling and billing do.
    // Network cleanup additionally fences rollover on its package.
    // Yield to active node work even when it is between database transactions.
    let parents = if rule.node_lock {
        ", parents AS MATERIALIZED (
            SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM candidates)
            AND n.state NOT IN ('deleting','delete_failed')
            AND NOT coalesce((
                SELECT l.owner IS NOT NULL AND l.expires_at>clock_timestamp()
                    AND l.epoch=hysteriax_execution_epoch()
                FROM resource_leases l WHERE l.resource_key='node:'||n.id
            ),false)
            ORDER BY n.id FOR NO KEY UPDATE OF n SKIP LOCKED
         )"
    } else {
        ""
    };
    let (packages, parent_join) = if table == NETWORK.table {
        (
            ", packages AS MATERIALIZED (
                SELECT p.node_id FROM node_packages p JOIN parents n ON n.id=p.node_id
                ORDER BY p.node_id FOR NO KEY UPDATE OF p SKIP LOCKED
             )",
            "JOIN packages p ON p.node_id=c.node_id",
        )
    } else if rule.node_lock {
        ("", "JOIN parents p ON p.id=c.node_id")
    } else {
        ("", "")
    };
    let node_check = if rule.node_lock {
        "AND t.node_id=c.node_id"
    } else {
        ""
    };
    // Acquire the parent batch lock before deleting source rows, so the
    // snapshot used later for the dependency check is newer than the lock.
    let (batch_locks, batch_join, batch_locked) = if let Some(batch_cutoff) = batch_parameter {
        (
            format!(
                ", batches AS MATERIALIZED (
                SELECT b.sample_id FROM traffic_sample_batches b
                JOIN (SELECT DISTINCT c.sample FROM candidates c JOIN locked l USING (id)
                      WHERE c.sample<>'') k ON k.sample=b.sample_id
                WHERE b.state='complete' AND b.completed_at<{batch_cutoff}
                ORDER BY b.sample_id FOR UPDATE OF b SKIP LOCKED
             )"
            ),
            "LEFT JOIN batches b ON b.sample_id=c.sample",
            "l.id IS NOT NULL AND b.sample_id IS NOT NULL",
        )
    } else {
        (String::new(), "", "false")
    };
    format!(
        "WITH candidates AS MATERIALIZED ({scan}) {parents} {packages},
         locked AS MATERIALIZED (
            SELECT t.{key} AS id FROM candidates c {parent_join}
            JOIN {table} t ON t.{key}=c.id {node_check}
            ORDER BY t.{key} FOR UPDATE OF t SKIP LOCKED
         ) {batch_locks}
         SELECT c.at,c.id::text,c.sample,l.id IS NOT NULL,{batch_locked} FROM candidates c
         LEFT JOIN locked l ON l.id=c.id {batch_join} ORDER BY c.at,c.sample,c.id"
    )
}
