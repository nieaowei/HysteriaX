//! Ordered lease sets keep WAN round trips independent of resource count.
use super::{Execution, PgConnection, Row};

pub(crate) async fn guard_all(
    connection: &mut PgConnection,
    leases: &[Execution],
) -> Result<(), sqlx::Error> {
    if leases.is_empty() {
        return Ok(());
    }
    if leases.len() == 1 {
        return leases[0].guard(connection).await;
    }
    // Aggregate the entire locked cohort before checking database time. An
    // earlier lease can expire while acquiring a later row's lock. Requested
    // identities are deliberately not deduplicated: mixed incarnations fail.
    let valid: bool = sqlx::query_scalar(
        r#"
        WITH requested AS MATERIALIZED (
            SELECT * FROM unnest($1::text[],$2::text[],$3::text[],$4::bigint[],$5::text[])
                r(resource_key,owner,work_id,generation,epoch)
        ), locked AS MATERIALIZED (
            SELECT l.* FROM resource_leases l
            WHERE EXISTS (SELECT 1 FROM requested r WHERE r.resource_key=l.resource_key
                AND r.owner=l.owner AND r.work_id=l.work_id
                AND r.generation=l.generation AND r.epoch=l.epoch)
              AND NOT l.blocked AND l.expires_at>clock_timestamp()
              AND l.epoch=hysteriax_execution_epoch()
            ORDER BY l.resource_key FOR UPDATE OF l
        ), checked AS MATERIALIZED (
            SELECT coalesce(bool_and(coalesce(l.owner=r.owner AND l.work_id=r.work_id
                AND l.generation=r.generation AND l.epoch=r.epoch
                AND NOT l.blocked AND l.expires_at IS NOT NULL,false)),false)
                AND count(DISTINCT l.epoch)=1 valid,
                min(l.expires_at) earliest_expiry,min(l.epoch) epoch
            FROM requested r LEFT JOIN locked l USING(resource_key)
        )
        SELECT coalesce(valid AND epoch=hysteriax_execution_epoch()
            AND earliest_expiry>clock_timestamp(),false) FROM checked
        "#,
    )
    .bind(
        leases
            .iter()
            .map(|l| l.resource.as_str())
            .collect::<Vec<_>>(),
    )
    .bind(leases.iter().map(|l| l.owner.as_str()).collect::<Vec<_>>())
    .bind(leases.iter().map(|l| l.work.as_str()).collect::<Vec<_>>())
    .bind(leases.iter().map(|l| l.generation).collect::<Vec<_>>())
    .bind(leases.iter().map(|l| l.epoch.as_str()).collect::<Vec<_>>())
    .fetch_one(connection)
    .await?;
    if !valid {
        return Err(sqlx::Error::Protocol("execution lease was lost".into()));
    }
    Ok(())
}

/// Caller owns the enclosing transaction/savepoint and rolls it back on None.
/// Lock rows in one statement, then inspect remote barriers in a NEW statement:
/// a lock holder may have committed an intent while the first statement waited.
pub(crate) async fn acquire_all(
    connection: &mut PgConnection,
    resources: &[String],
    owner: &str,
    work: &str,
) -> Result<Option<Vec<Execution>>, sqlx::Error> {
    let mut keys = resources.to_vec();
    keys.sort();
    keys.dedup();
    if keys.is_empty() {
        return Ok(Some(vec![]));
    }
    sqlx::query("INSERT INTO resource_leases(resource_key) SELECT key FROM unnest($1::text[]) key ORDER BY key ON CONFLICT DO NOTHING")
        .bind(&keys).execute(&mut *connection).await?;
    let rows = sqlx::query("SELECT resource_key,blocked,(epoch<>hysteriax_execution_epoch() OR expires_at IS NULL OR expires_at<=clock_timestamp()) AS available FROM resource_leases WHERE resource_key=ANY($1::text[]) ORDER BY resource_key FOR UPDATE SKIP LOCKED")
        .bind(&keys).fetch_all(&mut *connection).await?;
    if rows.len() != keys.len()
        || rows
            .iter()
            .any(|r| r.get::<bool, _>("blocked") || !r.get::<bool, _>("available"))
    {
        return Ok(None);
    }
    let uncertain: Vec<String> = sqlx::query_scalar("SELECT DISTINCT resource_key FROM execution_operations WHERE resource_key=ANY($1::text[]) AND state IN ('dispatching','reconciling','needs_attention')")
        .bind(&keys).fetch_all(&mut *connection).await?;
    if !uncertain.is_empty() {
        sqlx::query("UPDATE resource_leases SET blocked=true WHERE resource_key=ANY($1::text[])")
            .bind(&uncertain)
            .execute(&mut *connection)
            .await?;
        sqlx::query("UPDATE execution_operations SET state='reconciling',updated_at=clock_timestamp() WHERE resource_key=ANY($1::text[]) AND state='dispatching'")
            .bind(&uncertain).execute(&mut *connection).await?;
        return Ok(None);
    }
    let rows = sqlx::query("UPDATE resource_leases SET owner=$2,work_id=$3,epoch=hysteriax_execution_epoch(),generation=generation+1,expires_at=clock_timestamp()+make_interval(secs=>$4::double precision) WHERE resource_key=ANY($1::text[]) RETURNING resource_key,generation,epoch")
        .bind(&keys).bind(owner).bind(work).bind(crate::runtime_config::execution_lease_seconds() as f64)
        .fetch_all(connection).await?;
    let mut leases: Vec<_> = rows
        .into_iter()
        .map(|row| Execution {
            resource: row.get("resource_key"),
            owner: owner.into(),
            work: work.into(),
            generation: row.get("generation"),
            epoch: row.get("epoch"),
        })
        .collect();
    leases.sort_by(|a, b| a.resource.cmp(&b.resource));
    Ok(Some(leases))
}

#[cfg(test)]
#[path = "lease_set_tests.rs"]
mod tests;
