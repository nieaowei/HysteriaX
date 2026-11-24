use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use sqlx::{Acquire, PgConnection};

use super::{Counts, Cursor, PAGE};

const NON_SAMPLING_WORK_KINDS: &[&str] = &[
    "access-revocation",
    "node-limit-dispatch",
    "mtls-trust-cleanup-page",
    "mtls-node-publication-admission",
    "mtls-node-publication-metadata",
    "mtls-rotation-publication",
    "mtls-rotation-user-admission",
    "mtls-rotation-user-issuance",
    "mtls-superseded-identity-flags",
    "mtls-rotation-probe-admission",
    "mtls-rotation-probe-switch",
    "mtls-identity-publication-schedule",
    "mtls-superseded-node-jobs",
    "mtls-rotation-trust",
    "mtls-identity-renewal",
    "mtls-probe-renewal",
    "mtls-ca-publication",
];

fn set_cursor(
    cursor: &mut Cursor,
    page_len: usize,
    limit: i64,
    last: Option<(DateTime<Utc>, String, Vec<String>)>,
) {
    cursor.done = page_len < limit as usize;
    if let Some((at, key, extra)) = last {
        cursor.after = Some((at, key));
        cursor.extra = extra;
    }
}

fn tally_reasons(
    counts: &mut Counts,
    locked_len: usize,
    reasons: Vec<(String, Option<String>)>,
) -> Vec<String> {
    counts.skip("row_gone", locked_len.saturating_sub(reasons.len()));
    let mut eligible = Vec::new();
    for (id, reason) in reasons {
        if let Some(reason) = reason {
            counts.skip(&reason, 1);
        } else {
            eligible.push(id);
        }
    }
    counts.eligible = eligible.len();
    eligible
}

pub(super) async fn traffic_baselines(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
    shard: Option<u8>,
) -> Result<Counts> {
    if shard.is_some_and(|s| s != 0) {
        cursor.done = true;
        return Ok(Counts::default());
    }
    let after = cursor.after.as_ref().and_then(|(at, user)| {
        cursor
            .extra
            .first()
            .map(|node| (*at, node.as_str(), user.as_str()))
    });
    let candidates: Vec<(DateTime<Utc>, String, String, bool)> =
        if let Some((at, node, user)) = after {
            sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                SELECT sampled_at, node_id, user_id FROM traffic_baselines
                WHERE sampled_at < $1 AND (sampled_at, node_id, user_id) > ($2, $3, $4)
                ORDER BY sampled_at, node_id, user_id LIMIT $5
             ), parents AS MATERIALIZED (
                SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM candidates)
                  AND n.state NOT IN ('deleting', 'delete_failed')
                ORDER BY n.id FOR NO KEY UPDATE OF n SKIP LOCKED
             ), locked AS MATERIALIZED (
                SELECT t.node_id, t.user_id FROM candidates c
                JOIN parents p ON p.id = c.node_id
                JOIN traffic_baselines t ON t.node_id = c.node_id AND t.user_id = c.user_id
                ORDER BY t.node_id, t.user_id FOR UPDATE OF t SKIP LOCKED
             )
             SELECT c.sampled_at, c.node_id, c.user_id, l.node_id IS NOT NULL
             FROM candidates c
             LEFT JOIN locked l USING (node_id, user_id)
             ORDER BY c.sampled_at, c.node_id, c.user_id",
            )
            .bind(cutoff)
            .bind(at)
            .bind(node)
            .bind(user)
            .bind(PAGE)
            .fetch_all(&mut *connection)
            .await?
        } else {
            sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                SELECT sampled_at, node_id, user_id FROM traffic_baselines
                WHERE sampled_at < $1
                ORDER BY sampled_at, node_id, user_id LIMIT $2
             ), parents AS MATERIALIZED (
                SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM candidates)
                  AND n.state NOT IN ('deleting', 'delete_failed')
                ORDER BY n.id FOR NO KEY UPDATE OF n SKIP LOCKED
             ), locked AS MATERIALIZED (
                SELECT t.node_id, t.user_id FROM candidates c
                JOIN parents p ON p.id = c.node_id
                JOIN traffic_baselines t ON t.node_id = c.node_id AND t.user_id = c.user_id
                ORDER BY t.node_id, t.user_id FOR UPDATE OF t SKIP LOCKED
             )
             SELECT c.sampled_at, c.node_id, c.user_id, l.node_id IS NOT NULL
             FROM candidates c
             LEFT JOIN locked l USING (node_id, user_id)
             ORDER BY c.sampled_at, c.node_id, c.user_id",
            )
            .bind(cutoff)
            .bind(PAGE)
            .fetch_all(&mut *connection)
            .await?
        };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(at, node, user, _)| (*at, user.clone(), vec![node.clone()])),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let mut nodes = Vec::new();
    let mut users = Vec::new();
    for (_, node, user, locked) in candidates {
        if locked {
            nodes.push(node);
            users.push(user);
        }
    }
    counts.skip("row_or_parent_locked_or_gone", counts.scanned - nodes.len());
    if nodes.is_empty() {
        return Ok(counts);
    }

    let reasons: Vec<(Option<String>, bool)> = sqlx::query_as(
        "WITH classified AS MATERIALIZED (
            SELECT t.node_id, t.user_id, CASE
                WHEN t.sampled_at >= $3 THEN 'recent'
                WHEN NOT EXISTS (
                    SELECT 1 FROM nodes n WHERE n.id = t.node_id
                    AND n.state NOT IN ('deleting', 'delete_failed')
                ) THEN 'node_deleting'
                WHEN EXISTS (
                    SELECT 1 FROM node_assignments a
                    WHERE a.node_id = t.node_id AND a.user_id = t.user_id
                ) THEN 'assigned'
                WHEN EXISTS (
                    SELECT 1 FROM traffic_sample_batches b
                    WHERE b.node_id = t.node_id AND b.state IN ('capturing', 'ready')
                ) THEN 'batch_active'
                ELSE NULL
            END AS reason
            FROM unnest($1::text[], $2::text[]) k(node_id, user_id)
            JOIN traffic_baselines t USING (node_id, user_id)
         ), deleted AS (
            DELETE FROM traffic_baselines t USING classified c
            WHERE t.node_id = c.node_id AND t.user_id = c.user_id AND c.reason IS NULL
            RETURNING t.node_id, t.user_id
         )
         SELECT c.reason, d.node_id IS NOT NULL
         FROM classified c LEFT JOIN deleted d USING (node_id, user_id)",
    )
    .bind(&nodes)
    .bind(&users)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    counts.skip("row_gone", nodes.len().saturating_sub(reasons.len()));
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
        "history delete count changed for traffic_baselines"
    );
    Ok(counts)
}

pub(super) async fn execution_operations(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let candidates: Vec<(DateTime<Utc>, String, bool)> = if let Some((at, id)) = &cursor.after {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT updated_at, operation_id FROM execution_operations
                WHERE state = 'verified' AND updated_at < $1 AND (updated_at, operation_id) > ($2, $3)
                ORDER BY updated_at, operation_id LIMIT $4
             ), locked AS MATERIALIZED (
                SELECT o.operation_id FROM execution_operations o
                JOIN candidates c USING (operation_id)
                ORDER BY o.operation_id FOR UPDATE OF o SKIP LOCKED
             )
             SELECT c.updated_at, c.operation_id, l.operation_id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (operation_id)
             ORDER BY c.updated_at, c.operation_id",
        )
        .bind(cutoff)
        .bind(at)
        .bind(id)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT updated_at, operation_id FROM execution_operations
                WHERE state = 'verified' AND updated_at < $1
                ORDER BY updated_at, operation_id LIMIT $2
             ), locked AS MATERIALIZED (
                SELECT o.operation_id FROM execution_operations o
                JOIN candidates c USING (operation_id)
                ORDER BY o.operation_id FOR UPDATE OF o SKIP LOCKED
             )
             SELECT c.updated_at, c.operation_id, l.operation_id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (operation_id)
             ORDER BY c.updated_at, c.operation_id",
        )
        .bind(cutoff)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(at, id, _)| (*at, id.clone(), Vec::new())),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let locked: Vec<String> = candidates
        .into_iter()
        .filter_map(|(_, id, ok)| ok.then_some(id))
        .collect();
    counts.skip("row_locked_or_gone", counts.scanned - locked.len());
    if locked.is_empty() {
        return Ok(counts);
    }

    let reasons: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT o.operation_id, CASE
            WHEN o.state <> 'verified' OR o.updated_at >= $2 THEN 'active_or_recent'
            WHEN EXISTS (
                SELECT 1 FROM execution_rechecks c
                WHERE c.operation_id = o.operation_id
                  AND c.state IN ('prepared', 'dispatching', 'needs_attention')
            ) THEN 'recheck_pending'
            WHEN EXISTS (
                SELECT 1 FROM jobs j WHERE j.id = o.work_id AND (
                    j.status NOT IN ('succeeded', 'failed', 'rolled_back', 'cancelled')
                    OR j.stage IN ('needs_attention', 'reconciliation_required')
                )
            ) THEN 'job_active_or_unresolved'
            WHEN EXISTS (
                SELECT 1 FROM background_work w
                WHERE w.id = o.work_id AND w.state IN ('queued', 'running')
            ) THEN 'work_active'
            WHEN EXISTS (
                SELECT 1 FROM resource_leases l
                WHERE l.resource_key = o.resource_key AND l.blocked
            ) THEN 'lease_blocked'
            ELSE NULL
        END
        FROM execution_operations o WHERE o.operation_id = ANY($1)",
    )
    .bind(&locked)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    let eligible = tally_reasons(&mut counts, locked.len(), reasons);
    if !eligible.is_empty() {
        let namespaces: Vec<String> = sqlx::query_scalar(
            "DELETE FROM execution_rechecks WHERE operation_id = ANY($1) RETURNING request_namespace",
        )
        .bind(&eligible)
        .fetch_all(&mut *connection)
        .await?;
        if !namespaces.is_empty() {
            sqlx::query(
                "DELETE FROM execution_recheck_requests r
                 WHERE r.namespace = ANY($1)
                   AND NOT EXISTS (
                       SELECT 1 FROM execution_rechecks c WHERE c.request_namespace = r.namespace
                   )",
            )
            .bind(&namespaces)
            .execute(&mut *connection)
            .await?;
        }
        counts.deleted =
            sqlx::query("DELETE FROM execution_operations WHERE operation_id = ANY($1)")
                .bind(&eligible)
                .execute(&mut *connection)
                .await?
                .rows_affected();
        ensure!(
            counts.deleted == eligible.len() as u64,
            "history delete count changed for execution_operations"
        );
    }
    Ok(counts)
}

pub(super) async fn execution_recheck_requests(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let candidates: Vec<(DateTime<Utc>, String, bool)> = if let Some((at, ns)) = &cursor.after {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT created_at, namespace FROM execution_recheck_requests
                WHERE created_at < $1 AND (created_at, namespace) > ($2, $3)
                ORDER BY created_at, namespace LIMIT $4
             ), locked AS MATERIALIZED (
                SELECT r.namespace FROM execution_recheck_requests r
                JOIN candidates c USING (namespace)
                ORDER BY r.namespace FOR UPDATE OF r SKIP LOCKED
             )
             SELECT c.created_at, c.namespace, l.namespace IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (namespace)
             ORDER BY c.created_at, c.namespace",
        )
        .bind(cutoff)
        .bind(at)
        .bind(ns)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT created_at, namespace FROM execution_recheck_requests
                WHERE created_at < $1
                ORDER BY created_at, namespace LIMIT $2
             ), locked AS MATERIALIZED (
                SELECT r.namespace FROM execution_recheck_requests r
                JOIN candidates c USING (namespace)
                ORDER BY r.namespace FOR UPDATE OF r SKIP LOCKED
             )
             SELECT c.created_at, c.namespace, l.namespace IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (namespace)
             ORDER BY c.created_at, c.namespace",
        )
        .bind(cutoff)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(at, ns, _)| (*at, ns.clone(), Vec::new())),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let locked: Vec<String> = candidates
        .into_iter()
        .filter_map(|(_, ns, ok)| ok.then_some(ns))
        .collect();
    counts.skip("row_locked_or_gone", counts.scanned - locked.len());
    if locked.is_empty() {
        return Ok(counts);
    }

    let reasons: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT r.namespace, CASE
            WHEN r.created_at >= $2 THEN 'recent'
            WHEN EXISTS (
                SELECT 1 FROM execution_rechecks c WHERE c.request_namespace = r.namespace
            ) THEN 'recheck_reference'
            ELSE NULL
        END
        FROM execution_recheck_requests r WHERE r.namespace = ANY($1)",
    )
    .bind(&locked)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    let eligible = tally_reasons(&mut counts, locked.len(), reasons);
    if !eligible.is_empty() {
        counts.deleted =
            sqlx::query("DELETE FROM execution_recheck_requests WHERE namespace = ANY($1)")
                .bind(&eligible)
                .execute(&mut *connection)
                .await?
                .rows_affected();
        ensure!(
            counts.deleted == eligible.len() as u64,
            "history delete count changed for execution_recheck_requests"
        );
    }
    Ok(counts)
}

pub(super) async fn non_sampling_work(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let candidates: Vec<(DateTime<Utc>, String, bool, bool)> = if let Some((at, id)) = &cursor.after
    {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT finished_at, id FROM background_work
                WHERE cycle_id IS NULL
                  AND state IN ('complete', 'failed', 'expired')
                  AND finished_at IS NOT NULL
                  AND kind = ANY($1)
                  AND finished_at < $2
                  AND (finished_at, id) > ($3, $4)
                ORDER BY finished_at, id LIMIT $5
             ), locked_work AS MATERIALIZED (
                SELECT w.id FROM background_work w JOIN candidates c USING (id)
                ORDER BY w.id FOR UPDATE OF w SKIP LOCKED
             ), locked_leases AS MATERIALIZED (
                SELECT w.id FROM locked_work w CROSS JOIN LATERAL (
                    SELECT 1 FROM resource_leases l WHERE l.resource_key = 'work:' || w.id
                    FOR UPDATE SKIP LOCKED
                ) l
             )
             SELECT c.finished_at, c.id, w.id IS NOT NULL, l.id IS NOT NULL
             FROM candidates c
             LEFT JOIN locked_work w USING (id)
             LEFT JOIN locked_leases l USING (id)
             ORDER BY c.finished_at, c.id",
        )
        .bind(NON_SAMPLING_WORK_KINDS)
        .bind(cutoff)
        .bind(at)
        .bind(id)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT finished_at, id FROM background_work
                WHERE cycle_id IS NULL
                  AND state IN ('complete', 'failed', 'expired')
                  AND finished_at IS NOT NULL
                  AND kind = ANY($1)
                  AND finished_at < $2
                ORDER BY finished_at, id LIMIT $3
             ), locked_work AS MATERIALIZED (
                SELECT w.id FROM background_work w JOIN candidates c USING (id)
                ORDER BY w.id FOR UPDATE OF w SKIP LOCKED
             ), locked_leases AS MATERIALIZED (
                SELECT w.id FROM locked_work w CROSS JOIN LATERAL (
                    SELECT 1 FROM resource_leases l WHERE l.resource_key = 'work:' || w.id
                    FOR UPDATE SKIP LOCKED
                ) l
             )
             SELECT c.finished_at, c.id, w.id IS NOT NULL, l.id IS NOT NULL
             FROM candidates c
             LEFT JOIN locked_work w USING (id)
             LEFT JOIN locked_leases l USING (id)
             ORDER BY c.finished_at, c.id",
        )
        .bind(NON_SAMPLING_WORK_KINDS)
        .bind(cutoff)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(at, id, _, _)| (*at, id.clone(), Vec::new())),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let mut work_ids = Vec::new();
    let mut lease_ids = Vec::new();
    for (_, id, work_locked, lease_locked) in candidates {
        if work_locked {
            work_ids.push(id.clone());
        }
        if lease_locked {
            lease_ids.push(id);
        }
    }
    counts.skip("work_locked", counts.scanned - work_ids.len());
    if work_ids.is_empty() {
        return Ok(counts);
    }

    let rows: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT w.id, CASE
            WHEN w.cycle_id IS NOT NULL OR w.kind <> ALL($2)
              OR w.state NOT IN ('complete', 'failed', 'expired')
              OR w.owner IS NOT NULL OR w.finished_at IS NULL OR w.finished_at >= $3
              OR w.available_at > clock_timestamp()
                THEN 'work_ineligible'
            WHEN EXISTS (SELECT 1 FROM resource_leases attached WHERE attached.work_id = w.id)
              OR (l.resource_key IS NOT NULL AND (
                  l.owner IS NOT NULL OR l.expires_at IS NOT NULL OR l.blocked
                  OR NOT (w.id = ANY($4::text[]))
              ))
                THEN 'lease_retained'
            WHEN EXISTS (SELECT 1 FROM execution_operations o WHERE o.work_id = w.id)
              OR EXISTS (SELECT 1 FROM execution_operations o WHERE o.resource_key = 'work:' || w.id)
                THEN 'execution_evidence'
            WHEN w.payload ? 'rotation_id' AND EXISTS (
                SELECT 1 FROM mtls_rotations r
                WHERE r.id = w.payload->>'rotation_id' AND r.phase NOT IN ('complete', 'failed')
            ) THEN 'rotation_active'
            WHEN w.kind = 'mtls-trust-cleanup-page' AND EXISTS (
                SELECT 1 FROM mtls_trust_cleanup c
                WHERE 'trust-cleanup:' || c.ca_id || ':' || c.ca_version = w.id
                  AND c.completed_at IS NULL
            ) THEN 'cleanup_active'
            WHEN w.kind = 'mtls-superseded-node-jobs' AND w.payload ? 'rotation_id' AND EXISTS (
                SELECT 1 FROM mtls_node_operations o
                WHERE o.rotation_id = w.payload->>'rotation_id' AND o.pending_job
            ) THEN 'pending_jobs_remain'
            ELSE NULL
        END AS reason, l.resource_key
        FROM background_work w
        LEFT JOIN resource_leases l ON l.resource_key = 'work:' || w.id
        WHERE w.id = ANY($1::text[])",
    )
    .bind(&work_ids)
    .bind(NON_SAMPLING_WORK_KINDS)
    .bind(cutoff)
    .bind(&lease_ids)
    .fetch_all(&mut *connection)
    .await?;

    counts.skip("row_gone", work_ids.len().saturating_sub(rows.len()));
    let mut eligible_work = Vec::new();
    let mut eligible_leases = Vec::new();
    for (id, reason, lease_key) in rows {
        if let Some(reason) = reason {
            counts.skip(&reason, 1);
        } else {
            eligible_work.push(id);
            if let Some(key) = lease_key {
                eligible_leases.push(key);
            }
        }
    }
    counts.eligible = eligible_work.len();
    if !eligible_work.is_empty() {
        if !eligible_leases.is_empty() {
            let removed_leases =
                sqlx::query("DELETE FROM resource_leases WHERE resource_key = ANY($1)")
                    .bind(&eligible_leases)
                    .execute(&mut *connection)
                    .await?
                    .rows_affected();
            ensure!(
                removed_leases == eligible_leases.len() as u64,
                "non-sampling work lease delete count changed"
            );
        }
        counts.deleted = sqlx::query("DELETE FROM background_work WHERE id = ANY($1)")
            .bind(&eligible_work)
            .execute(&mut *connection)
            .await?
            .rows_affected();
        ensure!(
            counts.deleted == eligible_work.len() as u64,
            "non-sampling work delete count changed"
        );
    }
    Ok(counts)
}

pub(super) async fn kick_requests(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let limit = 4 * PAGE;
    let after = cursor.after.as_ref().and_then(|(at, user)| {
        cursor
            .extra
            .first()
            .map(|node| (*at, node.as_str(), user.as_str()))
    });
    let candidates: Vec<(DateTime<Utc>, String, String, bool)> =
        if let Some((at, node, user)) = after {
            sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                SELECT updated_at, node_id, user_id FROM kick_requests
                WHERE state IN ('completed', 'cancelled') AND updated_at < $1
                  AND (updated_at, node_id, user_id) > ($2, $3, $4)
                ORDER BY updated_at, node_id, user_id LIMIT $5
             ), locked AS MATERIALIZED (
                SELECT r.node_id, r.user_id FROM kick_requests r
                JOIN candidates c USING (node_id, user_id)
                ORDER BY r.node_id, r.user_id FOR UPDATE OF r SKIP LOCKED
             )
             SELECT c.updated_at, c.node_id, c.user_id, l.node_id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (node_id, user_id)
             ORDER BY c.updated_at, c.node_id, c.user_id",
            )
            .bind(cutoff)
            .bind(at)
            .bind(node)
            .bind(user)
            .bind(limit)
            .fetch_all(&mut *connection)
            .await?
        } else {
            sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                SELECT updated_at, node_id, user_id FROM kick_requests
                WHERE state IN ('completed', 'cancelled') AND updated_at < $1
                ORDER BY updated_at, node_id, user_id LIMIT $2
             ), locked AS MATERIALIZED (
                SELECT r.node_id, r.user_id FROM kick_requests r
                JOIN candidates c USING (node_id, user_id)
                ORDER BY r.node_id, r.user_id FOR UPDATE OF r SKIP LOCKED
             )
             SELECT c.updated_at, c.node_id, c.user_id, l.node_id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (node_id, user_id)
             ORDER BY c.updated_at, c.node_id, c.user_id",
            )
            .bind(cutoff)
            .bind(limit)
            .fetch_all(&mut *connection)
            .await?
        };

    set_cursor(
        cursor,
        candidates.len(),
        limit,
        candidates
            .last()
            .map(|(at, node, user, _)| (*at, user.clone(), vec![node.clone()])),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let mut nodes = Vec::new();
    let mut users = Vec::new();
    for (_, node, user, locked) in candidates {
        if locked {
            nodes.push(node);
            users.push(user);
        }
    }
    counts.skip("row_locked_or_gone", counts.scanned - nodes.len());
    if nodes.is_empty() {
        return Ok(counts);
    }

    let reasons: Vec<(Option<String>, bool)> = sqlx::query_as(
        "WITH classified AS MATERIALIZED (
            SELECT r.node_id, r.user_id, CASE
                WHEN r.state NOT IN ('completed', 'cancelled') OR r.updated_at >= $3 THEN 'recent_or_active'
                WHEN r.latest_job_id IS NOT NULL AND EXISTS (
                    SELECT 1 FROM jobs j WHERE j.id = r.latest_job_id AND (
                        j.status NOT IN ('succeeded', 'failed', 'rolled_back', 'cancelled')
                        OR j.stage IN ('needs_attention', 'reconciliation_required')
                    )
                ) THEN 'job_active_or_unresolved'
                ELSE NULL
            END AS reason
            FROM unnest($1::text[], $2::text[]) k(node_id, user_id)
            JOIN kick_requests r USING (node_id, user_id)
         ), deleted AS (
            DELETE FROM kick_requests r USING classified c
            WHERE r.node_id = c.node_id AND r.user_id = c.user_id AND c.reason IS NULL
            RETURNING r.node_id, r.user_id
         )
         SELECT c.reason, d.node_id IS NOT NULL
         FROM classified c LEFT JOIN deleted d USING (node_id, user_id)",
    )
    .bind(&nodes)
    .bind(&users)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    counts.skip("row_gone", nodes.len().saturating_sub(reasons.len()));
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
        "history delete count changed for kick_requests"
    );
    Ok(counts)
}

pub(super) async fn management_receipts(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let after = cursor.after.as_ref().and_then(|(at, key)| {
        if cursor.extra.len() == 2 {
            Some((
                *at,
                cursor.extra[0].as_str(),
                cursor.extra[1].as_str(),
                key.as_str(),
            ))
        } else {
            None
        }
    });
    let candidates: Vec<(DateTime<Utc>, String, String, String, bool)> = if let Some((
        at,
        actor,
        op,
        key,
    )) = after
    {
        sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                    SELECT created_at, actor_id, operation, idempotency_key
                    FROM management_receipts
                    WHERE created_at < $1
                      AND (created_at, actor_id, operation, idempotency_key) > ($2, $3, $4, $5)
                    ORDER BY created_at, actor_id, operation, idempotency_key LIMIT $6
                 ), locked AS MATERIALIZED (
                    SELECT r.actor_id, r.operation, r.idempotency_key
                    FROM management_receipts r
                    JOIN candidates c USING (actor_id, operation, idempotency_key)
                    ORDER BY r.actor_id, r.operation, r.idempotency_key FOR UPDATE OF r SKIP LOCKED
                 )
                 SELECT c.created_at, c.actor_id, c.operation, c.idempotency_key, l.actor_id IS NOT NULL
                 FROM candidates c LEFT JOIN locked l USING (actor_id, operation, idempotency_key)
                 ORDER BY c.created_at, c.actor_id, c.operation, c.idempotency_key",
            )
            .bind(cutoff)
            .bind(at)
            .bind(actor)
            .bind(op)
            .bind(key)
            .bind(PAGE)
            .fetch_all(&mut *connection)
            .await?
    } else {
        sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                    SELECT created_at, actor_id, operation, idempotency_key
                    FROM management_receipts
                    WHERE created_at < $1
                    ORDER BY created_at, actor_id, operation, idempotency_key LIMIT $2
                 ), locked AS MATERIALIZED (
                    SELECT r.actor_id, r.operation, r.idempotency_key
                    FROM management_receipts r
                    JOIN candidates c USING (actor_id, operation, idempotency_key)
                    ORDER BY r.actor_id, r.operation, r.idempotency_key FOR UPDATE OF r SKIP LOCKED
                 )
                 SELECT c.created_at, c.actor_id, c.operation, c.idempotency_key, l.actor_id IS NOT NULL
                 FROM candidates c LEFT JOIN locked l USING (actor_id, operation, idempotency_key)
                 ORDER BY c.created_at, c.actor_id, c.operation, c.idempotency_key",
            )
            .bind(cutoff)
            .bind(PAGE)
            .fetch_all(&mut *connection)
            .await?
    };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(at, actor, op, key, _)| (*at, key.clone(), vec![actor.clone(), op.clone()])),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let mut actors = Vec::new();
    let mut ops = Vec::new();
    let mut keys = Vec::new();
    for (_, actor, op, key, locked) in candidates {
        if locked {
            actors.push(actor);
            ops.push(op);
            keys.push(key);
        }
    }
    counts.skip("row_locked_or_gone", counts.scanned - actors.len());
    if actors.is_empty() {
        return Ok(counts);
    }

    let results: Vec<(bool, bool)> = sqlx::query_as(
        "WITH classified AS MATERIALIZED (
            SELECT r.actor_id, r.operation, r.idempotency_key, r.created_at < $4 AS ok
            FROM unnest($1::text[], $2::text[], $3::text[]) k(actor_id, operation, idempotency_key)
            JOIN management_receipts r USING (actor_id, operation, idempotency_key)
         ), deleted AS (
            DELETE FROM management_receipts r USING classified c
            WHERE r.actor_id = c.actor_id AND r.operation = c.operation
              AND r.idempotency_key = c.idempotency_key AND c.ok
            RETURNING r.actor_id, r.operation, r.idempotency_key
         )
         SELECT c.ok, d.actor_id IS NOT NULL
         FROM classified c LEFT JOIN deleted d USING (actor_id, operation, idempotency_key)",
    )
    .bind(&actors)
    .bind(&ops)
    .bind(&keys)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    counts.skip("row_gone", actors.len().saturating_sub(results.len()));
    for (ok, deleted) in results {
        if ok {
            counts.eligible += 1;
            counts.deleted += u64::from(deleted);
        } else {
            counts.skip("recent", 1);
        }
    }
    ensure!(
        counts.deleted == counts.eligible as u64,
        "history delete count changed for management_receipts"
    );
    Ok(counts)
}

pub(super) async fn mtls_operations(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let after = cursor.after.as_ref().and_then(|(at, key)| {
        cursor
            .extra
            .first()
            .map(|actor| (*at, actor.as_str(), key.as_str()))
    });
    let candidates: Vec<(DateTime<Utc>, String, String, bool)> =
        if let Some((at, actor, key)) = after {
            sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                    SELECT created_at, actor_id, idempotency_key FROM mtls_operations
                    WHERE created_at < $1 AND (created_at, actor_id, idempotency_key) > ($2, $3, $4)
                    ORDER BY created_at, actor_id, idempotency_key LIMIT $5
                 ), locked AS MATERIALIZED (
                    SELECT o.actor_id, o.idempotency_key FROM mtls_operations o
                    JOIN candidates c USING (actor_id, idempotency_key)
                    ORDER BY o.actor_id, o.idempotency_key FOR UPDATE OF o SKIP LOCKED
                 )
                 SELECT c.created_at, c.actor_id, c.idempotency_key, l.actor_id IS NOT NULL
                 FROM candidates c LEFT JOIN locked l USING (actor_id, idempotency_key)
                 ORDER BY c.created_at, c.actor_id, c.idempotency_key",
            )
            .bind(cutoff)
            .bind(at)
            .bind(actor)
            .bind(key)
            .bind(PAGE)
            .fetch_all(&mut *connection)
            .await?
        } else {
            sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                    SELECT created_at, actor_id, idempotency_key FROM mtls_operations
                    WHERE created_at < $1
                    ORDER BY created_at, actor_id, idempotency_key LIMIT $2
                 ), locked AS MATERIALIZED (
                    SELECT o.actor_id, o.idempotency_key FROM mtls_operations o
                    JOIN candidates c USING (actor_id, idempotency_key)
                    ORDER BY o.actor_id, o.idempotency_key FOR UPDATE OF o SKIP LOCKED
                 )
                 SELECT c.created_at, c.actor_id, c.idempotency_key, l.actor_id IS NOT NULL
                 FROM candidates c LEFT JOIN locked l USING (actor_id, idempotency_key)
                 ORDER BY c.created_at, c.actor_id, c.idempotency_key",
            )
            .bind(cutoff)
            .bind(PAGE)
            .fetch_all(&mut *connection)
            .await?
        };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(at, actor, key, _)| (*at, key.clone(), vec![actor.clone()])),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let mut actors = Vec::new();
    let mut keys = Vec::new();
    for (_, actor, key, locked) in candidates {
        if locked {
            actors.push(actor);
            keys.push(key);
        }
    }
    counts.skip("row_locked_or_gone", counts.scanned - actors.len());
    if actors.is_empty() {
        return Ok(counts);
    }

    let results: Vec<(bool, bool)> = sqlx::query_as(
        "WITH classified AS MATERIALIZED (
            SELECT o.actor_id, o.idempotency_key, o.created_at < $3 AS ok
            FROM unnest($1::text[], $2::text[]) k(actor_id, idempotency_key)
            JOIN mtls_operations o USING (actor_id, idempotency_key)
         ), deleted AS (
            DELETE FROM mtls_operations o USING classified c
            WHERE o.actor_id = c.actor_id AND o.idempotency_key = c.idempotency_key AND c.ok
            RETURNING o.actor_id, o.idempotency_key
         )
         SELECT c.ok, d.actor_id IS NOT NULL
         FROM classified c LEFT JOIN deleted d USING (actor_id, idempotency_key)",
    )
    .bind(&actors)
    .bind(&keys)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    counts.skip("row_gone", actors.len().saturating_sub(results.len()));
    for (ok, deleted) in results {
        if ok {
            counts.eligible += 1;
            counts.deleted += u64::from(deleted);
        } else {
            counts.skip("recent", 1);
        }
    }
    ensure!(
        counts.deleted == counts.eligible as u64,
        "history delete count changed for mtls_operations"
    );
    Ok(counts)
}

pub(super) async fn cache_tombstones(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let limit = 4 * PAGE;
    let after = cursor.after.as_ref().and_then(|(at, entity)| {
        cursor
            .extra
            .first()
            .map(|kind| (*at, kind.as_str(), entity.as_str()))
    });
    let candidates: Vec<(DateTime<Utc>, String, String, bool)> =
        if let Some((at, kind, entity)) = after {
            sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                    SELECT updated_at, kind, entity_id FROM cache_entity_versions
                    WHERE deleted AND kind <> 'quota_rank' AND updated_at < $1
                      AND (updated_at, kind, entity_id) > ($2, $3, $4)
                    ORDER BY updated_at, kind, entity_id LIMIT $5
                 ), locked AS MATERIALIZED (
                    SELECT v.kind, v.entity_id FROM cache_entity_versions v
                    JOIN candidates c USING (kind, entity_id)
                    ORDER BY v.kind, v.entity_id FOR UPDATE OF v SKIP LOCKED
                 )
                 SELECT c.updated_at, c.kind, c.entity_id, l.kind IS NOT NULL
                 FROM candidates c LEFT JOIN locked l USING (kind, entity_id)
                 ORDER BY c.updated_at, c.kind, c.entity_id",
            )
            .bind(cutoff)
            .bind(at)
            .bind(kind)
            .bind(entity)
            .bind(limit)
            .fetch_all(&mut *connection)
            .await?
        } else {
            sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                    SELECT updated_at, kind, entity_id FROM cache_entity_versions
                    WHERE deleted AND kind <> 'quota_rank' AND updated_at < $1
                    ORDER BY updated_at, kind, entity_id LIMIT $2
                 ), locked AS MATERIALIZED (
                    SELECT v.kind, v.entity_id FROM cache_entity_versions v
                    JOIN candidates c USING (kind, entity_id)
                    ORDER BY v.kind, v.entity_id FOR UPDATE OF v SKIP LOCKED
                 )
                 SELECT c.updated_at, c.kind, c.entity_id, l.kind IS NOT NULL
                 FROM candidates c LEFT JOIN locked l USING (kind, entity_id)
                 ORDER BY c.updated_at, c.kind, c.entity_id",
            )
            .bind(cutoff)
            .bind(limit)
            .fetch_all(&mut *connection)
            .await?
        };

    set_cursor(
        cursor,
        candidates.len(),
        limit,
        candidates
            .last()
            .map(|(at, kind, entity, _)| (*at, entity.clone(), vec![kind.clone()])),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let mut kinds = Vec::new();
    let mut entities = Vec::new();
    for (_, kind, entity, locked) in candidates {
        if locked {
            kinds.push(kind);
            entities.push(entity);
        }
    }
    counts.skip("row_locked_or_gone", counts.scanned - kinds.len());
    if kinds.is_empty() {
        return Ok(counts);
    }

    let results: Vec<(bool, bool)> = sqlx::query_as(
        "WITH classified AS MATERIALIZED (
            SELECT v.kind, v.entity_id,
                   (v.deleted AND v.kind <> 'quota_rank' AND v.updated_at < $3) AS ok
            FROM unnest($1::text[], $2::text[]) k(kind, entity_id)
            JOIN cache_entity_versions v USING (kind, entity_id)
         ), deleted AS (
            DELETE FROM cache_entity_versions v USING classified c
            WHERE v.kind = c.kind AND v.entity_id = c.entity_id AND c.ok
            RETURNING v.kind, v.entity_id
         )
         SELECT c.ok, d.kind IS NOT NULL
         FROM classified c LEFT JOIN deleted d USING (kind, entity_id)",
    )
    .bind(&kinds)
    .bind(&entities)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    counts.skip("row_gone", kinds.len().saturating_sub(results.len()));
    for (ok, deleted) in results {
        if ok {
            counts.eligible += 1;
            counts.deleted += u64::from(deleted);
        } else {
            counts.skip("revived_or_recent", 1);
        }
    }
    ensure!(
        counts.deleted == counts.eligible as u64,
        "history delete count changed for cache_entity_versions"
    );
    Ok(counts)
}

pub(super) async fn orphan_resource_leases(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    now: DateTime<Utc>,
) -> Result<Counts> {
    let after = cursor.after.as_ref().map(|(_, key)| key.as_str());
    let candidates: Vec<(String, bool)> = if let Some(after) = after {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT resource_key FROM resource_leases
                WHERE owner IS NULL AND work_id IS NULL AND NOT blocked AND resource_key > $1
                ORDER BY resource_key LIMIT $2
             ), locked AS MATERIALIZED (
                SELECT l.resource_key FROM resource_leases l JOIN candidates c USING (resource_key)
                ORDER BY l.resource_key FOR UPDATE OF l SKIP LOCKED
             )
             SELECT c.resource_key, l.resource_key IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (resource_key)
             ORDER BY c.resource_key",
        )
        .bind(after)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT resource_key FROM resource_leases
                WHERE owner IS NULL AND work_id IS NULL AND NOT blocked
                ORDER BY resource_key LIMIT $1
             ), locked AS MATERIALIZED (
                SELECT l.resource_key FROM resource_leases l JOIN candidates c USING (resource_key)
                ORDER BY l.resource_key FOR UPDATE OF l SKIP LOCKED
             )
             SELECT c.resource_key, l.resource_key IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (resource_key)
             ORDER BY c.resource_key",
        )
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(key, _)| (now, key.clone(), Vec::new())),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        ..Counts::default()
    };
    let locked: Vec<String> = candidates
        .into_iter()
        .filter_map(|(key, ok)| ok.then_some(key))
        .collect();
    counts.skip("row_locked_or_gone", counts.scanned - locked.len());
    if locked.is_empty() {
        return Ok(counts);
    }

    let reasons: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT l.resource_key, CASE
            WHEN l.owner IS NOT NULL OR l.work_id IS NOT NULL OR l.expires_at IS NOT NULL OR l.blocked
                THEN 'lease_active_or_blocked'
            WHEN EXISTS (SELECT 1 FROM execution_operations o WHERE o.resource_key = l.resource_key)
                THEN 'execution_evidence'
            WHEN CASE
                WHEN l.resource_key LIKE 'node:%'
                    THEN EXISTS (SELECT 1 FROM nodes n WHERE n.id = substr(l.resource_key, 6))
                WHEN l.resource_key LIKE 'job:%'
                    THEN EXISTS (SELECT 1 FROM jobs j WHERE j.id = substr(l.resource_key, 5))
                WHEN l.resource_key LIKE 'work:%'
                    THEN EXISTS (SELECT 1 FROM background_work w WHERE w.id = substr(l.resource_key, 6))
                WHEN l.resource_key LIKE 'ca:%'
                    THEN EXISTS (SELECT 1 FROM mtls_authorities a WHERE a.credential_id = substr(l.resource_key, 4))
                WHEN l.resource_key LIKE 'identity:%'
                    THEN EXISTS (SELECT 1 FROM mtls_user_identities i WHERE i.id = substr(l.resource_key, 10))
                ELSE true
            END THEN 'entity_present_or_other'
            ELSE NULL
        END
        FROM resource_leases l WHERE l.resource_key = ANY($1)",
    )
    .bind(&locked)
    .fetch_all(&mut *connection)
    .await?;

    let eligible = tally_reasons(&mut counts, locked.len(), reasons);
    if !eligible.is_empty() {
        counts.deleted = sqlx::query("DELETE FROM resource_leases WHERE resource_key = ANY($1)")
            .bind(&eligible)
            .execute(&mut *connection)
            .await?
            .rows_affected();
        ensure!(
            counts.deleted == eligible.len() as u64,
            "history delete count changed for resource_leases"
        );
    }
    Ok(counts)
}

pub(super) async fn orphan_job_node_kick_totals(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    now: DateTime<Utc>,
) -> Result<Counts> {
    let after = cursor.after.as_ref().map(|(_, node)| node.as_str());
    let candidates: Vec<(String, bool)> = if let Some(after) = after {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT node_id FROM job_node_kick_totals WHERE node_id > $1 ORDER BY node_id LIMIT $2
             ), locked AS MATERIALIZED (
                SELECT t.node_id FROM job_node_kick_totals t JOIN candidates c USING (node_id)
                ORDER BY t.node_id FOR UPDATE OF t SKIP LOCKED
             )
             SELECT c.node_id, l.node_id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (node_id) ORDER BY c.node_id",
        )
        .bind(after)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT node_id FROM job_node_kick_totals ORDER BY node_id LIMIT $1
             ), locked AS MATERIALIZED (
                SELECT t.node_id FROM job_node_kick_totals t JOIN candidates c USING (node_id)
                ORDER BY t.node_id FOR UPDATE OF t SKIP LOCKED
             )
             SELECT c.node_id, l.node_id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (node_id) ORDER BY c.node_id",
        )
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(node, _)| (now, node.clone(), Vec::new())),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        ..Counts::default()
    };
    let locked: Vec<String> = candidates
        .into_iter()
        .filter_map(|(node, ok)| ok.then_some(node))
        .collect();
    counts.skip("row_locked_or_gone", counts.scanned - locked.len());
    if locked.is_empty() {
        return Ok(counts);
    }

    let reasons: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT t.node_id, CASE
            WHEN t.pending <> 0 OR t.pending_limit <> 0 OR t.failed_limit <> 0
              OR EXISTS (SELECT 1 FROM nodes n WHERE n.id = t.node_id)
              OR EXISTS (SELECT 1 FROM jobs j WHERE j.kind = 'kick' AND j.node_id = t.node_id)
                THEN 'active_or_node_present'
            ELSE NULL
        END
        FROM job_node_kick_totals t WHERE t.node_id = ANY($1)",
    )
    .bind(&locked)
    .fetch_all(&mut *connection)
    .await?;

    let eligible = tally_reasons(&mut counts, locked.len(), reasons);
    if !eligible.is_empty() {
        counts.deleted = sqlx::query("DELETE FROM job_node_kick_totals WHERE node_id = ANY($1)")
            .bind(&eligible)
            .execute(&mut *connection)
            .await?
            .rows_affected();
        ensure!(
            counts.deleted == eligible.len() as u64,
            "history delete count changed for job_node_kick_totals"
        );
    }
    Ok(counts)
}

pub(super) async fn credential_batches(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let candidates: Vec<(DateTime<Utc>, String, bool)> = if let Some((at, id)) = &cursor.after {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT created_at, id FROM credential_batches
                WHERE created_at < $1 AND (created_at, id) > ($2, $3)
                ORDER BY created_at, id LIMIT $4
             ), locked AS MATERIALIZED (
                SELECT b.id FROM credential_batches b JOIN candidates c USING (id)
                ORDER BY b.id FOR UPDATE OF b SKIP LOCKED
             )
             SELECT c.created_at, c.id, l.id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (id) ORDER BY c.created_at, c.id",
        )
        .bind(cutoff)
        .bind(at)
        .bind(id)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT created_at, id FROM credential_batches
                WHERE created_at < $1
                ORDER BY created_at, id LIMIT $2
             ), locked AS MATERIALIZED (
                SELECT b.id FROM credential_batches b JOIN candidates c USING (id)
                ORDER BY b.id FOR UPDATE OF b SKIP LOCKED
             )
             SELECT c.created_at, c.id, l.id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (id) ORDER BY c.created_at, c.id",
        )
        .bind(cutoff)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(at, id, _)| (*at, id.clone(), Vec::new())),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let locked: Vec<String> = candidates
        .into_iter()
        .filter_map(|(_, id, ok)| ok.then_some(id))
        .collect();
    counts.skip("row_locked_or_gone", counts.scanned - locked.len());
    if locked.is_empty() {
        return Ok(counts);
    }

    let reasons: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT b.id, CASE
            WHEN b.created_at >= $2 THEN 'recent'
            WHEN EXISTS (SELECT 1 FROM mtls_rotations r WHERE r.batch_id = b.id)
                THEN 'rotation_reference'
            WHEN EXISTS (
                SELECT 1 FROM credential_batch_items i JOIN jobs j ON j.id = i.job_id
                WHERE i.batch_id = b.id AND (
                    j.status NOT IN ('succeeded', 'failed', 'rolled_back', 'cancelled')
                    OR j.finished_at IS NULL OR j.finished_at >= $2
                    OR j.stage IN ('needs_attention', 'reconciliation_required')
                )
            ) OR EXISTS (
                SELECT 1 FROM dns_credential_batch_items i JOIN jobs j ON j.id = i.job_id
                WHERE i.batch_id = b.id AND (
                    j.status NOT IN ('succeeded', 'failed', 'rolled_back', 'cancelled')
                    OR j.finished_at IS NULL OR j.finished_at >= $2
                    OR j.stage IN ('needs_attention', 'reconciliation_required')
                )
            ) THEN 'job_active_recent_or_unresolved'
            ELSE NULL
        END
        FROM credential_batches b WHERE b.id = ANY($1)",
    )
    .bind(&locked)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    let eligible = tally_reasons(&mut counts, locked.len(), reasons);
    if !eligible.is_empty() {
        sqlx::query("DELETE FROM dns_credential_batch_items WHERE batch_id = ANY($1)")
            .bind(&eligible)
            .execute(&mut *connection)
            .await?;
        counts.deleted = sqlx::query("DELETE FROM credential_batches WHERE id = ANY($1)")
            .bind(&eligible)
            .execute(&mut *connection)
            .await?
            .rows_affected();
        ensure!(
            counts.deleted == eligible.len() as u64,
            "history delete count changed for credential_batches"
        );
    }
    Ok(counts)
}

pub(super) async fn mtls_trust_cleanup(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let after = cursor.after.as_ref().and_then(|(at, ca)| {
        cursor
            .extra
            .first()
            .and_then(|v| v.parse::<i64>().ok())
            .map(|ver| (*at, ca.as_str(), ver))
    });
    let candidates: Vec<(DateTime<Utc>, String, i64, bool)> = if let Some((at, ca, ver)) = after {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT completed_at, ca_id, ca_version FROM mtls_trust_cleanup
                WHERE completed_at IS NOT NULL AND completed_at < $1
                  AND (completed_at, ca_id, ca_version) > ($2, $3, $4)
                ORDER BY completed_at, ca_id, ca_version LIMIT $5
             ), locked AS MATERIALIZED (
                SELECT t.ca_id, t.ca_version FROM mtls_trust_cleanup t
                JOIN candidates c USING (ca_id, ca_version)
                ORDER BY t.ca_id, t.ca_version FOR UPDATE OF t SKIP LOCKED
             )
             SELECT c.completed_at, c.ca_id, c.ca_version, l.ca_id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (ca_id, ca_version)
             ORDER BY c.completed_at, c.ca_id, c.ca_version",
        )
        .bind(cutoff)
        .bind(at)
        .bind(ca)
        .bind(ver)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT completed_at, ca_id, ca_version FROM mtls_trust_cleanup
                WHERE completed_at IS NOT NULL AND completed_at < $1
                ORDER BY completed_at, ca_id, ca_version LIMIT $2
             ), locked AS MATERIALIZED (
                SELECT t.ca_id, t.ca_version FROM mtls_trust_cleanup t
                JOIN candidates c USING (ca_id, ca_version)
                ORDER BY t.ca_id, t.ca_version FOR UPDATE OF t SKIP LOCKED
             )
             SELECT c.completed_at, c.ca_id, c.ca_version, l.ca_id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (ca_id, ca_version)
             ORDER BY c.completed_at, c.ca_id, c.ca_version",
        )
        .bind(cutoff)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(at, ca, ver, _)| (*at, ca.clone(), vec![ver.to_string()])),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let mut cas = Vec::new();
    let mut versions = Vec::new();
    for (_, ca, ver, locked) in candidates {
        if locked {
            cas.push(ca);
            versions.push(ver);
        }
    }
    counts.skip("row_locked_or_gone", counts.scanned - cas.len());
    if cas.is_empty() {
        return Ok(counts);
    }

    let rows: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT t.ca_id, t.ca_version, CASE
            WHEN t.completed_at IS NULL OR t.completed_at >= $3 THEN 'recent_or_incomplete'
            WHEN EXISTS (
                SELECT 1 FROM mtls_cleanup_nodes c JOIN jobs j ON j.id = c.job_id
                WHERE c.ca_id = t.ca_id AND c.ca_version = t.ca_version AND (
                    j.status NOT IN ('succeeded', 'failed', 'rolled_back', 'cancelled')
                    OR j.finished_at IS NULL OR j.finished_at >= $3
                    OR j.stage IN ('needs_attention', 'reconciliation_required')
                )
            ) THEN 'job_active_recent_or_unresolved'
            ELSE NULL
        END
        FROM unnest($1::text[], $2::bigint[]) k(ca_id, ca_version)
        JOIN mtls_trust_cleanup t USING (ca_id, ca_version)",
    )
    .bind(&cas)
    .bind(&versions)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    counts.skip("row_gone", cas.len().saturating_sub(rows.len()));
    let mut eligible_cas = Vec::new();
    let mut eligible_versions = Vec::new();
    for (ca, ver, reason) in rows {
        if let Some(reason) = reason {
            counts.skip(&reason, 1);
        } else {
            eligible_cas.push(ca);
            eligible_versions.push(ver);
        }
    }
    counts.eligible = eligible_cas.len();
    if !eligible_cas.is_empty() {
        sqlx::query(
            "DELETE FROM mtls_cleanup_nodes c
             USING unnest($1::text[], $2::bigint[]) k(ca_id, ca_version)
             WHERE c.ca_id = k.ca_id AND c.ca_version = k.ca_version",
        )
        .bind(&eligible_cas)
        .bind(&eligible_versions)
        .execute(&mut *connection)
        .await?;
        counts.deleted = sqlx::query(
            "DELETE FROM mtls_trust_cleanup t
             USING unnest($1::text[], $2::bigint[]) k(ca_id, ca_version)
             WHERE t.ca_id = k.ca_id AND t.ca_version = k.ca_version",
        )
        .bind(&eligible_cas)
        .bind(&eligible_versions)
        .execute(&mut *connection)
        .await?
        .rows_affected();
        ensure!(
            counts.deleted == eligible_cas.len() as u64,
            "history delete count changed for mtls_trust_cleanup"
        );
    }
    Ok(counts)
}

pub(super) async fn mtls_deployment_snapshots(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let after = cursor.after.as_ref().and_then(|(at, activation)| {
        if cursor.extra.len() == 2 {
            cursor.extra[1]
                .parse::<i64>()
                .ok()
                .map(|rev| (*at, cursor.extra[0].as_str(), rev, activation.as_str()))
        } else {
            None
        }
    });
    let candidates: Vec<(DateTime<Utc>, String, i64, String, bool)> =
        if let Some((at, node, rev, activation)) = after {
            sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                    SELECT created_at, node_id, revision, activation_id
                    FROM mtls_deployment_snapshots
                    WHERE created_at < $1
                      AND (created_at, node_id, revision, activation_id) > ($2, $3, $4, $5)
                    ORDER BY created_at, node_id, revision, activation_id LIMIT $6
                 ), locked_nodes AS MATERIALIZED (
                    SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM candidates)
                    ORDER BY n.id FOR NO KEY UPDATE OF n SKIP LOCKED
                 ), locked AS MATERIALIZED (
                    SELECT s.node_id, s.revision, s.activation_id
                    FROM candidates c
                    JOIN mtls_deployment_snapshots s USING (node_id, revision, activation_id)
                    WHERE (s.node_id IN (SELECT id FROM locked_nodes)
                           OR NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id = s.node_id))
                    ORDER BY s.node_id, s.revision, s.activation_id FOR UPDATE OF s SKIP LOCKED
                 )
                 SELECT c.created_at, c.node_id, c.revision, c.activation_id, l.node_id IS NOT NULL
                 FROM candidates c LEFT JOIN locked l USING (node_id, revision, activation_id)
                 ORDER BY c.created_at, c.node_id, c.revision, c.activation_id",
            )
            .bind(cutoff)
            .bind(at)
            .bind(node)
            .bind(rev)
            .bind(activation)
            .bind(PAGE)
            .fetch_all(&mut *connection)
            .await?
        } else {
            sqlx::query_as(
                "WITH candidates AS MATERIALIZED (
                    SELECT created_at, node_id, revision, activation_id
                    FROM mtls_deployment_snapshots
                    WHERE created_at < $1
                    ORDER BY created_at, node_id, revision, activation_id LIMIT $2
                 ), locked_nodes AS MATERIALIZED (
                    SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM candidates)
                    ORDER BY n.id FOR NO KEY UPDATE OF n SKIP LOCKED
                 ), locked AS MATERIALIZED (
                    SELECT s.node_id, s.revision, s.activation_id
                    FROM candidates c
                    JOIN mtls_deployment_snapshots s USING (node_id, revision, activation_id)
                    WHERE (s.node_id IN (SELECT id FROM locked_nodes)
                           OR NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id = s.node_id))
                    ORDER BY s.node_id, s.revision, s.activation_id FOR UPDATE OF s SKIP LOCKED
                 )
                 SELECT c.created_at, c.node_id, c.revision, c.activation_id, l.node_id IS NOT NULL
                 FROM candidates c LEFT JOIN locked l USING (node_id, revision, activation_id)
                 ORDER BY c.created_at, c.node_id, c.revision, c.activation_id",
            )
            .bind(cutoff)
            .bind(PAGE)
            .fetch_all(&mut *connection)
            .await?
        };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(at, node, rev, act, _)| (*at, act.clone(), vec![node.clone(), rev.to_string()])),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let mut nodes = Vec::new();
    let mut revs = Vec::new();
    let mut acts = Vec::new();
    for (_, node, rev, act, locked) in candidates {
        if locked {
            nodes.push(node);
            revs.push(rev);
            acts.push(act);
        }
    }
    counts.skip("row_or_parent_locked_or_gone", counts.scanned - nodes.len());
    if nodes.is_empty() {
        return Ok(counts);
    }

    let reasons: Vec<(Option<String>, bool)> = sqlx::query_as(
        "WITH classified AS MATERIALIZED (
            SELECT s.node_id, s.revision, s.activation_id, CASE
                WHEN s.created_at >= $4 THEN 'recent'
                WHEN EXISTS (
                    SELECT 1 FROM nodes n WHERE n.id = s.node_id AND (
                        n.state IN ('deleting', 'delete_failed')
                        OR n.desired_revision = s.revision
                        OR n.deployed_revision = s.revision
                        OR n.mtls_activation_id = s.activation_id
                    )
                ) THEN 'node_current_reference'
                WHEN EXISTS (
                    SELECT 1 FROM config_versions v
                    WHERE v.node_id = s.node_id AND v.revision = s.revision
                ) THEN 'config_version_reference'
                ELSE NULL
            END AS reason
            FROM unnest($1::text[], $2::bigint[], $3::text[]) k(node_id, revision, activation_id)
            JOIN mtls_deployment_snapshots s USING (node_id, revision, activation_id)
         ), deleted AS (
            DELETE FROM mtls_deployment_snapshots s USING classified c
            WHERE s.node_id = c.node_id AND s.revision = c.revision
              AND s.activation_id = c.activation_id AND c.reason IS NULL
            RETURNING s.node_id, s.revision, s.activation_id
         )
         SELECT c.reason, d.node_id IS NOT NULL
         FROM classified c LEFT JOIN deleted d USING (node_id, revision, activation_id)",
    )
    .bind(&nodes)
    .bind(&revs)
    .bind(&acts)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    counts.skip("row_gone", nodes.len().saturating_sub(reasons.len()));
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
        "history delete count changed for mtls_deployment_snapshots"
    );
    Ok(counts)
}

async fn delete_activations_set(
    connection: &mut PgConnection,
    ids: &[String],
) -> Result<Option<u64>> {
    let mut savepoint = connection.begin().await?;
    sqlx::query(
        "UPDATE mtls_activations SET previous_activation_id = NULL
         WHERE previous_activation_id = ANY($1)
           AND phase IN ('committed', 'rolled_back')",
    )
    .bind(ids)
    .execute(&mut *savepoint)
    .await?;
    let result = sqlx::query("DELETE FROM mtls_activations WHERE id = ANY($1)")
        .bind(ids)
        .execute(&mut *savepoint)
        .await;
    match result {
        Ok(result) => {
            ensure!(
                result.rows_affected() == ids.len() as u64,
                "history delete count changed for mtls_activations"
            );
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

pub(super) async fn mtls_activations(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let candidates: Vec<(DateTime<Utc>, String, bool)> = if let Some((at, id)) = &cursor.after {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT updated_at, id, node_id FROM mtls_activations
                WHERE phase IN ('committed', 'rolled_back') AND updated_at < $1
                  AND (updated_at, id) > ($2, $3)
                ORDER BY updated_at, id LIMIT $4
             ), locked_nodes AS MATERIALIZED (
                SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM candidates)
                ORDER BY n.id FOR NO KEY UPDATE OF n SKIP LOCKED
             ), locked AS MATERIALIZED (
                SELECT a.id FROM candidates c
                JOIN mtls_activations a USING (id)
                WHERE (a.node_id IN (SELECT id FROM locked_nodes)
                       OR NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id = a.node_id))
                ORDER BY a.id FOR UPDATE OF a SKIP LOCKED
             )
             SELECT c.updated_at, c.id, l.id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (id) ORDER BY c.updated_at, c.id",
        )
        .bind(cutoff)
        .bind(at)
        .bind(id)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT updated_at, id, node_id FROM mtls_activations
                WHERE phase IN ('committed', 'rolled_back') AND updated_at < $1
                ORDER BY updated_at, id LIMIT $2
             ), locked_nodes AS MATERIALIZED (
                SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM candidates)
                ORDER BY n.id FOR NO KEY UPDATE OF n SKIP LOCKED
             ), locked AS MATERIALIZED (
                SELECT a.id FROM candidates c
                JOIN mtls_activations a USING (id)
                WHERE (a.node_id IN (SELECT id FROM locked_nodes)
                       OR NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id = a.node_id))
                ORDER BY a.id FOR UPDATE OF a SKIP LOCKED
             )
             SELECT c.updated_at, c.id, l.id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (id) ORDER BY c.updated_at, c.id",
        )
        .bind(cutoff)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(at, id, _)| (*at, id.clone(), Vec::new())),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let locked: Vec<String> = candidates
        .into_iter()
        .filter_map(|(_, id, ok)| ok.then_some(id))
        .collect();
    counts.skip(
        "row_or_parent_locked_or_gone",
        counts.scanned - locked.len(),
    );
    if locked.is_empty() {
        return Ok(counts);
    }

    let reasons: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT a.id, CASE
            WHEN a.phase NOT IN ('committed', 'rolled_back') OR a.updated_at >= $2
                THEN 'active_or_recent'
            WHEN EXISTS (SELECT 1 FROM nodes n WHERE n.mtls_activation_id = a.id)
                THEN 'node_active_activation'
            WHEN EXISTS (SELECT 1 FROM mtls_deployment_snapshots s WHERE s.activation_id = a.id)
                THEN 'snapshot_reference'
            WHEN EXISTS (SELECT 1 FROM mtls_node_operations o WHERE o.base_activation_id = a.id)
                THEN 'node_operation_reference'
            WHEN EXISTS (SELECT 1 FROM mtls_live_activation_trust l WHERE l.activation_id = a.id)
                THEN 'live_activation_trust'
            WHEN EXISTS (SELECT 1 FROM nodes n WHERE n.id = a.node_id)
              AND a.phase = 'committed'
              AND NOT EXISTS (
                  SELECT 1 FROM mtls_activations newer
                  WHERE newer.node_id = a.node_id
                    AND newer.phase = 'committed'
                    AND newer.generation > a.generation
              ) THEN 'latest_node_committed'
            WHEN EXISTS (
                SELECT 1 FROM mtls_activations succ
                WHERE succ.previous_activation_id = a.id
                  AND succ.phase NOT IN ('committed', 'rolled_back')
            ) THEN 'active_successor'
            WHEN EXISTS (
                SELECT 1 FROM jobs j WHERE j.id = a.job_id AND (
                    j.status NOT IN ('succeeded', 'failed', 'rolled_back', 'cancelled')
                    OR j.finished_at IS NULL OR j.finished_at >= $2
                    OR j.stage IN ('needs_attention', 'reconciliation_required')
                )
            ) THEN 'job_active_recent_or_unresolved'
            ELSE NULL
        END
        FROM mtls_activations a WHERE a.id = ANY($1)",
    )
    .bind(&locked)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    let eligible = tally_reasons(&mut counts, locked.len(), reasons);
    if eligible.is_empty() {
        return Ok(counts);
    }

    if let Some(deleted) = delete_activations_set(&mut *connection, &eligible).await? {
        counts.deleted = deleted;
        return Ok(counts);
    }

    counts.eligible = 0;
    for id in eligible {
        match delete_activations_set(&mut *connection, std::slice::from_ref(&id)).await? {
            Some(deleted) => {
                counts.eligible += 1;
                counts.deleted += deleted;
            }
            None => {
                counts.skip("additional_activation_reference", 1);
            }
        }
    }
    Ok(counts)
}

pub(super) async fn config_versions(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let candidates: Vec<(DateTime<Utc>, String, bool)> = if let Some((at, id)) = &cursor.after {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT created_at, id, node_id FROM config_versions
                WHERE created_at < $1 AND (created_at, id) > ($2, $3)
                ORDER BY created_at, id LIMIT $4
             ), parents AS MATERIALIZED (
                SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM candidates)
                  AND n.state NOT IN ('deleting', 'delete_failed')
                ORDER BY n.id FOR NO KEY UPDATE OF n SKIP LOCKED
             ), locked AS MATERIALIZED (
                SELECT v.id FROM candidates c
                JOIN parents p ON p.id = c.node_id
                JOIN config_versions v ON v.id = c.id
                ORDER BY v.id FOR UPDATE OF v SKIP LOCKED
             )
             SELECT c.created_at, c.id, l.id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (id) ORDER BY c.created_at, c.id",
        )
        .bind(cutoff)
        .bind(at)
        .bind(id)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    } else {
        sqlx::query_as(
            "WITH candidates AS MATERIALIZED (
                SELECT created_at, id, node_id FROM config_versions
                WHERE created_at < $1
                ORDER BY created_at, id LIMIT $2
             ), parents AS MATERIALIZED (
                SELECT n.id FROM nodes n WHERE n.id IN (SELECT node_id FROM candidates)
                  AND n.state NOT IN ('deleting', 'delete_failed')
                ORDER BY n.id FOR NO KEY UPDATE OF n SKIP LOCKED
             ), locked AS MATERIALIZED (
                SELECT v.id FROM candidates c
                JOIN parents p ON p.id = c.node_id
                JOIN config_versions v ON v.id = c.id
                ORDER BY v.id FOR UPDATE OF v SKIP LOCKED
             )
             SELECT c.created_at, c.id, l.id IS NOT NULL
             FROM candidates c LEFT JOIN locked l USING (id) ORDER BY c.created_at, c.id",
        )
        .bind(cutoff)
        .bind(PAGE)
        .fetch_all(&mut *connection)
        .await?
    };

    set_cursor(
        cursor,
        candidates.len(),
        PAGE,
        candidates
            .last()
            .map(|(at, id, _)| (*at, id.clone(), Vec::new())),
    );
    let mut counts = Counts {
        scanned: candidates.len(),
        oldest: candidates.first().map(|r| r.0),
        ..Counts::default()
    };
    let locked: Vec<String> = candidates
        .into_iter()
        .filter_map(|(_, id, ok)| ok.then_some(id))
        .collect();
    counts.skip(
        "row_or_parent_locked_or_gone",
        counts.scanned - locked.len(),
    );
    if locked.is_empty() {
        return Ok(counts);
    }

    let reasons: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT v.id, CASE
            WHEN v.created_at >= $2 THEN 'recent'
            WHEN NOT EXISTS (
                SELECT 1 FROM nodes n WHERE n.id = v.node_id
                  AND n.state NOT IN ('deleting', 'delete_failed')
            ) THEN 'node_deleting'
            WHEN EXISTS (
                SELECT 1 FROM nodes n WHERE n.id = v.node_id
                  AND (n.desired_revision = v.revision OR n.deployed_revision = v.revision)
            ) THEN 'node_current_revision'
            WHEN v.deployed_success AND v.revision = (
                SELECT max(s.revision) FROM config_versions s
                WHERE s.node_id = v.node_id AND s.deployed_success
            ) THEN 'latest_deployed_success'
            WHEN v.deployed_success AND EXISTS (
                SELECT 1 FROM nodes n WHERE n.id = v.node_id
                  AND n.deployed_revision IS NOT NULL
                  AND v.revision = (
                      SELECT max(s.revision) FROM config_versions s
                      WHERE s.node_id = v.node_id AND s.deployed_success
                        AND s.revision < n.deployed_revision
                  )
            ) THEN 'rollback_target'
            WHEN v.revision IN (
                SELECT r.revision FROM config_versions r
                WHERE r.node_id = v.node_id ORDER BY r.revision DESC LIMIT 20
            ) THEN 'recent_node_revisions'
            WHEN EXISTS (
                SELECT 1 FROM jobs j WHERE j.node_id = v.node_id
                  AND j.target_revision = v.revision AND (
                      j.status NOT IN ('succeeded', 'failed', 'rolled_back', 'cancelled')
                      OR j.finished_at IS NULL OR j.finished_at >= $2
                      OR j.stage IN ('needs_attention', 'reconciliation_required')
                  )
            ) THEN 'job_active_recent_or_unresolved'
            WHEN EXISTS (
                SELECT 1 FROM mtls_activations a
                WHERE a.node_id = v.node_id AND a.target_revision = v.revision
                  AND a.phase NOT IN ('committed', 'rolled_back')
            ) THEN 'activation_active'
            WHEN EXISTS (
                SELECT 1 FROM credential_reference_dirty d
                WHERE d.kind = 'history' AND d.source_id = v.id
            ) THEN 'reference_dirty'
            ELSE NULL
        END
        FROM config_versions v WHERE v.id = ANY($1)",
    )
    .bind(&locked)
    .bind(cutoff)
    .fetch_all(&mut *connection)
    .await?;

    let eligible = tally_reasons(&mut counts, locked.len(), reasons);
    if !eligible.is_empty() {
        counts.deleted = sqlx::query("DELETE FROM config_versions WHERE id = ANY($1)")
            .bind(&eligible)
            .execute(&mut *connection)
            .await?
            .rows_affected();
        ensure!(
            counts.deleted == eligible.len() as u64,
            "history delete count changed for config_versions"
        );
    }
    Ok(counts)
}
