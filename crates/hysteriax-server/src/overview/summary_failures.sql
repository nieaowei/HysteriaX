WITH RECURSIVE attempts AS (
            SELECT id,kind,node_name,status,error_message,finished_at,retry_of_job_id,
                   error_message last_error,finished_at last_failure_at
            FROM jobs WHERE status='failed' AND finished_at >= $1 AND EXISTS(SELECT 1 FROM job_retry_state_dirty)
            UNION ALL
            SELECT child.id,child.kind,child.node_name,child.status,child.error_message,child.finished_at,child.retry_of_job_id,
                   CASE WHEN child.status IN ('failed','rolled_back') THEN child.error_message ELSE a.last_error END,
                   CASE WHEN child.status IN ('failed','rolled_back') THEN child.finished_at ELSE a.last_failure_at END
            FROM jobs child JOIN attempts a ON child.retry_of_job_id=a.id
        )
SELECT * FROM (
        SELECT DISTINCT ON(a.id) a.id,COALESCE(a.node_name,a.kind) name,a.status,a.last_error error_message,a.last_failure_at finished_at
        FROM attempts a WHERE a.status!='succeeded' AND NOT EXISTS(SELECT 1 FROM jobs child WHERE child.retry_of_job_id=a.id)
        ORDER BY a.id,a.last_failure_at DESC

) raw_failures
UNION ALL
SELECT leaf_id,name,status,last_error,last_failure_at FROM job_retry_chain_state WHERE status<>'succeeded' AND failed_seed_at >= $1 AND NOT EXISTS(SELECT 1 FROM job_retry_state_dirty)
