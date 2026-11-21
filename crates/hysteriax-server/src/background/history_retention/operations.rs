use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use sqlx::{Acquire, PgConnection};

use super::{Candidate, Counts, Cursor, PAGE};

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

const MAX_CHAIN: usize = 256;

pub(super) async fn job_chain(
    connection: &mut PgConnection,
    cursor: &mut Cursor,
    cutoff: DateTime<Utc>,
) -> Result<Counts> {
    let after_time = cursor.after.as_ref().map(|(time, _)| *time);
    let after_id = cursor.after.as_ref().map(|(_, id)| id.as_str());
    let root: Option<Candidate> = if let Some(after_time) = after_time {
        sqlx::query_as(
            "SELECT finished_at,id FROM jobs WHERE retry_of_job_id IS NULL AND finished_at<$1
             AND status IN ('succeeded','failed','rolled_back','cancelled')
             AND (finished_at,id)>($2,$3) ORDER BY finished_at,id LIMIT 1",
        )
        .bind(cutoff)
        .bind(after_time)
        .bind(after_id)
        .fetch_optional(&mut *connection)
        .await?
    } else {
        sqlx::query_as(
            "SELECT finished_at,id FROM jobs WHERE retry_of_job_id IS NULL AND finished_at<$1
             AND status IN ('succeeded','failed','rolled_back','cancelled')
             ORDER BY finished_at,id LIMIT 1",
        )
        .bind(cutoff)
        .fetch_optional(&mut *connection)
        .await?
    };
    let Some(root) = root else {
        cursor.done = true;
        return Ok(Counts::default());
    };
    let mut counts = Counts::for_page(std::slice::from_ref(&root));
    let (at, root_id) = root;
    let ids: Vec<String> = sqlx::query_scalar(
        "WITH RECURSIVE chain(id,depth) AS (
             SELECT id,1 FROM jobs WHERE id=$1 AND retry_of_job_id IS NULL
             UNION ALL
             SELECT j.id,c.depth+1 FROM chain c JOIN jobs j ON j.retry_of_job_id=c.id WHERE c.depth<=$2
         ) SELECT id FROM chain ORDER BY id",
    ).bind(&root_id).bind(MAX_CHAIN as i32).fetch_all(&mut *connection).await?;
    cursor.after = Some((at, root_id.clone()));
    if ids.is_empty() {
        counts.skip("job_gone", 1);
        return Ok(counts);
    }
    if ids.len() > MAX_CHAIN {
        counts.skip("chain_too_long", 1);
        return Ok(counts);
    }
    counts.scanned = ids.len();
    let locked: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM jobs WHERE id=ANY($1) ORDER BY id FOR UPDATE SKIP LOCKED",
    )
    .bind(&ids)
    .fetch_all(&mut *connection)
    .await?;
    if locked != ids {
        counts.skip("chain_locked_or_changed", ids.len());
        return Ok(counts);
    }

    let reason: Option<String> = sqlx::query_scalar(include_str!("job_chain_protection.sql"))
        .bind(&ids)
        .bind(cutoff)
        .bind(&root_id)
        .fetch_one(&mut *connection)
        .await?;
    if let Some(reason) = reason {
        counts.skip(&reason, ids.len());
        return Ok(counts);
    }

    counts.eligible = ids.len();
    // Remove the complete chain in one statement. The self-FK's NO ACTION
    // checks see the entire set removed, and no intermediate chain is visible.
    let mut savepoint = connection.begin().await?;
    let result = sqlx::query("DELETE FROM jobs WHERE id=ANY($1)")
        .bind(&ids)
        .execute(&mut *savepoint)
        .await;
    match result {
        Ok(result) => {
            counts.deleted = result.rows_affected();
            ensure!(
                counts.deleted == ids.len() as u64,
                "job chain changed while locked"
            );
            savepoint.commit().await?;
        }
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|e| e.is_foreign_key_violation()) =>
        {
            savepoint.rollback().await?;
            counts.eligible = 0;
            counts.skip("additional_job_reference", ids.len());
        }
        Err(error) => return Err(error.into()),
    }
    Ok(counts)
}
