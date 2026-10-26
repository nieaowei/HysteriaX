//! One database round trip for sampling ownership and deadline admission.
use super::*;

const ADMIT: &str = r#"
WITH requested AS MATERIALIZED (
    SELECT * FROM unnest($1::text[], $2::text[], $3::text[], $4::bigint[], $5::text[])
        r(resource_key,owner,work_id,generation,epoch)
), locked_leases AS MATERIALIZED (
    SELECT l.* FROM resource_leases l
    WHERE EXISTS (
        SELECT 1 FROM requested r
        WHERE l.resource_key=r.resource_key AND l.owner=r.owner
          AND l.work_id=r.work_id AND l.generation=r.generation AND l.epoch=r.epoch
    )
      AND l.epoch=hysteriax_execution_epoch()
      AND l.expires_at>clock_timestamp() AND NOT l.blocked
    ORDER BY l.resource_key
    FOR UPDATE OF l
), lease_check AS MATERIALIZED (
    SELECT NOT EXISTS (
        SELECT 1 FROM requested r WHERE NOT EXISTS (
            SELECT 1 FROM locked_leases l
            WHERE l.resource_key=r.resource_key AND l.owner=r.owner
              AND l.work_id=r.work_id AND l.generation=r.generation AND l.epoch=r.epoch
              AND l.epoch=hysteriax_execution_epoch()
              AND l.expires_at>clock_timestamp() AND NOT l.blocked
        )
    ) AS valid
), locked_job AS MATERIALIZED (
    SELECT j.id FROM jobs j CROSS JOIN lease_check c
    WHERE c.valid AND j.id=$6 AND j.status='running'
      AND NOT EXISTS(SELECT 1 FROM mtls_superseded_node_jobs s WHERE s.job_id=j.id)
    FOR UPDATE OF j
), job_check AS MATERIALIZED (
    SELECT $6::text IS NULL OR EXISTS(SELECT 1 FROM locked_job) AS valid
), locked_work AS MATERIALIZED (
    SELECT w.* FROM background_work w CROSS JOIN lease_check c CROSS JOIN job_check j
    WHERE c.valid AND j.valid AND w.id=ANY($7::text[])
    ORDER BY w.id
    FOR UPDATE OF w
)
SELECT c.valid,j.valid,
    ($8::text IS NULL OR EXISTS (
        SELECT 1 FROM locked_work w
        WHERE w.id=$8 AND w.owner=$9 AND w.generation=$10 AND w.state='running'
          AND (w.node_id IS NULL OR EXISTS (
              SELECT 1 FROM nodes n WHERE n.id=w.node_id
                AND n.state NOT IN ('deleting','delete_failed')
          ))
    )) AS work_valid,
    (SELECT w.deadline_at>clock_timestamp() OR (
        w.kind='traffic' AND EXISTS (
            SELECT 1 FROM traffic_sample_batches b
            WHERE b.sample_id=w.id AND b.payload_digest IS NOT NULL
        )
    ) FROM locked_work w WHERE w.id=$11) AS timely
FROM lease_check c CROSS JOIN job_check j
"#;

pub(crate) async fn admit_sampling_read(
    pool: &PgPool,
    work: &crate::background::Work,
) -> anyhow::Result<()> {
    let parent = CURRENT.try_with(Clone::clone).ok();
    // Validate both the inherited execution and the exact Work passed by the
    // caller. Different identities for the same resource must both match;
    // deduplicating by resource alone would hide a stale or mixed owner.
    let mut leases = parent
        .as_ref()
        .map(|s| s.leases.clone())
        .unwrap_or_default();
    let job = leases
        .iter()
        .find(|l| l.resource.starts_with("job:"))
        .cloned();
    let current_work = leases
        .iter()
        .find(|l| l.resource.starts_with("work:"))
        .cloned();
    leases.extend(work.leases.clone());
    let mut work_ids = vec![work.id.clone()];
    if let Some(current) = &current_work {
        work_ids.push(current.work.clone());
    }
    work_ids.sort();
    work_ids.dedup();
    let requested_work = work.id.clone();

    // This private scope encloses only the closed database check below, never
    // a caller future or external operation. The single query replaces, rather
    // than omits, guard_current and the explicit Work lease/deadline checks.
    // Like identity_scope acquisition, it prevents the generic wrapper from
    // locking a subset first and then rechecking the entire scope over the WAN.
    let closed_scope = Scope {
        pool: pool.clone(),
        leases: vec![],
        sequence: parent.map(|s| s.sequence).unwrap_or_default(),
    };
    CURRENT
        .scope(
            closed_scope,
            crate::db::write(pool, move |tx| {
                let (leases, job, current_work, work_ids, requested_work) = (
                    leases.clone(),
                    job.clone(),
                    current_work.clone(),
                    work_ids.clone(),
                    requested_work.clone(),
                );
                Box::pin(async move {
                    let keys: Vec<_> = leases.iter().map(|l| l.resource.clone()).collect();
                    let owners: Vec<_> = leases.iter().map(|l| l.owner.clone()).collect();
                    let works: Vec<_> = leases.iter().map(|l| l.work.clone()).collect();
                    let generations: Vec<_> = leases.iter().map(|l| l.generation).collect();
                    let epochs: Vec<_> = leases.iter().map(|l| l.epoch.clone()).collect();
                    let (leases_valid, job_valid, work_valid, timely): (
                        bool,
                        bool,
                        bool,
                        Option<bool>,
                    ) = sqlx::query_as(ADMIT)
                        .bind(keys)
                        .bind(owners)
                        .bind(works)
                        .bind(generations)
                        .bind(epochs)
                        .bind(job.as_ref().map(|l| l.work.as_str()))
                        .bind(work_ids)
                        .bind(current_work.as_ref().map(|l| l.work.as_str()))
                        .bind(current_work.as_ref().map(|l| l.owner.as_str()))
                        .bind(current_work.as_ref().map(|l| l.generation))
                        .bind(requested_work)
                        .fetch_one(&mut **tx)
                        .await?;
                    if !leases_valid {
                        return Err(sqlx::Error::Protocol("execution lease was lost".into()).into());
                    }
                    if !job_valid {
                        return Err(sqlx::Error::Protocol(
                            "job was cancelled or no longer belongs to a running execution".into(),
                        )
                        .into());
                    }
                    if !work_valid {
                        return Err(sqlx::Error::Protocol(
                            "background execution was cancelled or ownership changed".into(),
                        )
                        .into());
                    }
                    match timely {
                        Some(true) => Ok(()),
                        Some(false) => Err(crate::background::SlotExpired.into()),
                        None => Err(sqlx::Error::RowNotFound.into()),
                    }
                })
            }),
        )
        .await
}
