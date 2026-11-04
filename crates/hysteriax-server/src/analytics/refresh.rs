//! Derived work owns advisory locks, never the source writer's dirty rows while
//! computing. Only the final generation acknowledgement touches those rows.
use super::*;
use sqlx::PgConnection;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub(super) enum Queue {
    Cycles,
    Hours,
    Minutes,
    Jobs,
}
impl Queue {
    const ALL: [Self; 4] = [Self::Cycles, Self::Hours, Self::Minutes, Self::Jobs];
    fn table(self) -> &'static str {
        match self {
            Self::Cycles => "analytics_dirty_cycles",
            Self::Hours => "traffic_dirty_hours",
            Self::Minutes => "online_dirty_minutes",
            Self::Jobs => "job_retry_state_dirty",
        }
    }
    fn limit(self) -> i64 {
        match self {
            Self::Cycles => 1,
            Self::Hours | Self::Minutes => 64,
            Self::Jobs => 32,
        }
    }
    fn row_type(self) -> &'static str {
        match self {
            Self::Cycles => "cycle_id uuid,generation bigint",
            Self::Hours => "source text,node_id text,hour timestamptz,generation bigint",
            Self::Minutes => "scope_node_id text,minute timestamptz,generation bigint",
            Self::Jobs => "job_id text,generation bigint",
        }
    }
    fn equality(self) -> &'static str {
        match self {
            Self::Cycles => "d.cycle_id=c.cycle_id",
            Self::Hours => "(d.source,d.node_id,d.hour)=(c.source,c.node_id,c.hour)",
            Self::Minutes => "(d.scope_node_id,d.minute)=(c.scope_node_id,c.minute)",
            Self::Jobs => "d.job_id=c.job_id",
        }
    }
    fn key(self) -> &'static str {
        match self {
            Self::Cycles => "d.cycle_id",
            Self::Hours => "(d.source,d.node_id,d.hour)",
            Self::Minutes => "(d.scope_node_id,d.minute)",
            Self::Jobs => "d.job_id",
        }
    }
    fn after(self) -> &'static str {
        match self {
            Self::Cycles => "(($1::jsonb->>'cycle_id')::uuid)",
            Self::Hours => {
                "($1::jsonb->>'source',$1::jsonb->>'node_id',($1::jsonb->>'hour')::timestamptz)"
            }
            Self::Minutes => "($1::jsonb->>'scope_node_id',($1::jsonb->>'minute')::timestamptz)",
            Self::Jobs => "($1::jsonb->>'job_id')",
        }
    }
    fn order_columns(self) -> &'static str {
        match self {
            Self::Cycles => "d.cycle_id",
            Self::Hours => "d.source,d.node_id,d.hour",
            Self::Minutes => "d.scope_node_id,d.minute",
            Self::Jobs => "d.job_id",
        }
    }
    fn lock_key(self) -> &'static str {
        match self {
            Self::Cycles => "item->>'cycle_id'",
            Self::Hours => {
                "jsonb_build_array(item->>'source',item->>'node_id',extract(epoch FROM (item->>'hour')::timestamptz))::text"
            }
            Self::Minutes => {
                "jsonb_build_array(item->>'scope_node_id',extract(epoch FROM (item->>'minute')::timestamptz))::text"
            }
            Self::Jobs => "item->>'job_id'",
        }
    }
}

pub(super) struct Claim {
    pub rows: Vec<Value>,
    pub cursor: Option<Value>,
    pub scanned: usize,
}

/// The cursor advances over busy keys too; a held first key cannot hide later
/// work. It is local to a sweep, never a durable acknowledgement.
pub(super) fn candidates_sql(queue: Queue, after: bool) -> String {
    // Separate the initial page from keyset pages so a generic prepared plan
    // can still use a range condition, without an optional-cursor OR filter.
    let predicate = if after {
        format!("{}>{}", queue.key(), queue.after())
    } else {
        "$1::jsonb IS NULL".to_owned()
    };
    format!(
        "SELECT to_jsonb(d), to_jsonb(d)-'generation' FROM {} d \
         WHERE {} ORDER BY {} LIMIT $2",
        queue.table(),
        predicate,
        queue.order_columns()
    )
}

pub(super) async fn claim_on(
    connection: &mut PgConnection,
    queue: Queue,
    after: Option<&Value>,
) -> Result<Claim, sqlx::Error> {
    let sql = candidates_sql(queue, after.is_some());
    let candidates: Vec<(Value, Value)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(after)
        .bind(queue.limit())
        .fetch_all(&mut *connection)
        .await?;
    let scanned = candidates.len();
    let cursor = candidates.last().map(|(_, key)| key.clone());
    if candidates.is_empty() {
        return Ok(Claim {
            rows: vec![],
            cursor,
            scanned,
        });
    }
    let payload = Value::Array(candidates.into_iter().map(|(row, _)| row).collect());
    let lock_sql = format!(
        "SELECT item FROM jsonb_array_elements($1::jsonb) item \
         WHERE pg_try_advisory_xact_lock(hashtextextended( \
           current_database()||':'||current_schema()||':analytics:'||$2||':'||({}),0)) \
         ORDER BY (item-'generation')::text",
        queue.lock_key()
    );
    let rows: Vec<Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(lock_sql))
        .bind(payload)
        .bind(queue.table())
        .fetch_all(&mut *connection)
        .await?;
    Ok(Claim {
        rows,
        cursor,
        scanned,
    })
}

pub(super) async fn acknowledge_on(
    connection: &mut PgConnection,
    queue: Queue,
    rows: &Value,
) -> Result<u64, sqlx::Error> {
    // A source transaction may still own a queue row. Never wait for it while
    // retaining result locks; leave that key dirty for the next pass instead.
    let sql = format!(
        "WITH candidates AS MATERIALIZED ( \
           SELECT d.ctid FROM {} d JOIN jsonb_to_recordset($1::jsonb) AS c({}) \
           ON {} AND d.generation=c.generation \
           ORDER BY (to_jsonb(d)-'generation')::text FOR UPDATE OF d SKIP LOCKED \
         ) DELETE FROM {} d USING candidates c WHERE d.ctid=c.ctid",
        queue.table(),
        queue.row_type(),
        queue.equality(),
        queue.table()
    );
    Ok(sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(rows)
        .execute(connection)
        .await?
        .rows_affected())
}

pub(super) async fn refresh_queue_on(
    connection: &mut PgConnection,
    queue: Queue,
    claim: Claim,
) -> Result<u64, sqlx::Error> {
    if claim.rows.is_empty() {
        return Ok(0);
    }
    let rows = compute_on(connection, queue, Value::Array(claim.rows)).await?;
    // No computation or other queue follows this statement before COMMIT.
    acknowledge_on(connection, queue, &rows).await
}

pub(super) async fn compute_on(
    connection: &mut PgConnection,
    queue: Queue,
    mut rows: Value,
) -> Result<Value, sqlx::Error> {
    sqlx::query("SET LOCAL jit=off")
        .execute(&mut *connection)
        .await?;
    match queue {
        Queue::Cycles => {
            let cycles: Vec<uuid::Uuid> = rows
                .as_array()
                .unwrap()
                .iter()
                .map(|r| serde_json::from_value(r["cycle_id"].clone()).expect("database UUID"))
                .collect();
            sqlx::query("DELETE FROM online_cycle_metrics m WHERE m.cycle_id=ANY($1) AND NOT EXISTS(SELECT 1 FROM online_samples s WHERE s.cycle_id=m.cycle_id AND (m.scope_node_id='' OR s.node_id=m.scope_node_id))")
                .bind(&cycles).execute(&mut *connection).await?;
            sqlx::query(include_str!("refresh_cycles.sql"))
                .bind(&cycles)
                .execute(&mut *connection)
                .await?;
        }
        Queue::Hours => {
            let nodes: Vec<String> = rows
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["node_id"].as_str().unwrap().to_owned())
                .collect();
            let available: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM nodes WHERE id=ANY($1::text[]) ORDER BY id FOR KEY SHARE SKIP LOCKED")
                .bind(&nodes).fetch_all(&mut *connection).await?;
            // Missing parents need cleanup, while locked existing parents must
            // retain their dirty keys. Test absence in this statement's snapshot.
            rows = sqlx::query_scalar("SELECT coalesce(jsonb_agg(item),'[]'::jsonb) FROM jsonb_array_elements($1::jsonb) item WHERE item->>'node_id'=ANY($2::text[]) OR NOT EXISTS(SELECT 1 FROM nodes n WHERE n.id=item->>'node_id')")
                .bind(&rows).bind(&available).fetch_one(&mut *connection).await?;
            rollup_on(connection, &rows, "traffic_hour_totals").await?;
        }
        Queue::Minutes => rollup_on(connection, &rows, "online_minute_totals").await?,
        Queue::Jobs => {
            let ids: Vec<String> = rows
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["job_id"].as_str().unwrap().to_owned())
                .collect();
            let roots: Vec<String> = sqlx::query_scalar(include_str!("retry_roots.sql"))
                .bind(&ids)
                .fetch_all(&mut *connection)
                .await?;
            // Distinct dirty jobs can describe one root. Acquire only worker
            // locks, in root order, without waiting on another refresher.
            let owned: Vec<String> = sqlx::query_scalar("SELECT root FROM unnest($1::text[]) root WHERE pg_try_advisory_xact_lock(hashtextextended(current_database()||':'||current_schema()||':analytics:retry-root:'||root,0)) ORDER BY root")
                .bind(&roots).fetch_all(&mut *connection).await?;
            if owned.len() != roots.len() {
                return Ok(Value::Array(vec![]));
            }
            sqlx::query("DELETE FROM job_retry_chain_state WHERE root_id=ANY($1)")
                .bind(&roots)
                .execute(&mut *connection)
                .await?;
            sqlx::query("DELETE FROM job_retry_chain_members WHERE root_id=ANY($1)")
                .bind(&roots)
                .execute(&mut *connection)
                .await?;
            sqlx::query(include_str!("refresh_retries.sql"))
                .bind(&roots)
                .execute(&mut *connection)
                .await?;
        }
    }
    Ok(rows)
}

async fn rollup_on(
    connection: &mut PgConnection,
    rows: &Value,
    table: &str,
) -> Result<(), sqlx::Error> {
    if rows.as_array().unwrap().is_empty() {
        return Ok(());
    }
    for statement in include_str!("refresh_rollups.sql").split(';') {
        if statement.contains(table) {
            sqlx::query(statement)
                .bind(rows)
                .execute(&mut *connection)
                .await?;
        }
    }
    Ok(())
}

async fn sweep(pool: &PgPool, queue: Queue, deadline: Instant) -> Result<u64, sqlx::Error> {
    let mut cursor: Option<Value> = None;
    let mut acknowledged = 0;
    while Instant::now() < deadline {
        let after = cursor.clone();
        let began = Instant::now();
        let (count, next, scanned) = db::write(pool, move |tx| {
            let after = after.clone();
            Box::pin(async move {
                let claim = claim_on(tx, queue, after.as_ref()).await?;
                let (next, scanned) = (claim.cursor.clone(), claim.scanned);
                let count = refresh_queue_on(tx, queue, claim).await?;
                Ok::<_, sqlx::Error>((count, next, scanned))
            })
        })
        .await?;
        tracing::debug!(
            queue = queue.table(),
            scanned,
            acknowledged = count,
            elapsed_ms = began.elapsed().as_millis() as u64,
            "analytics batch committed"
        );
        acknowledged += count;
        cursor = next;
        if scanned == 0 {
            break;
        }
    }
    Ok(acknowledged)
}

pub async fn refresh(pool: &PgPool) -> Result<(), sqlx::Error> {
    // Four queues, four independent transactions at most. A soft pass budget
    // prevents a continuously changing source from monopolizing the worker.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (a, b, c, d) = tokio::join!(
            sweep(pool, Queue::ALL[0], deadline),
            sweep(pool, Queue::ALL[1], deadline),
            sweep(pool, Queue::ALL[2], deadline),
            sweep(pool, Queue::ALL[3], deadline)
        );
        if a? + b? + c? + d? == 0 || Instant::now() >= deadline {
            return Ok(());
        }
    }
}
