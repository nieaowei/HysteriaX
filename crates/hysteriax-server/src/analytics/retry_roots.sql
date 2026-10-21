WITH RECURSIVE ancestors AS (
 SELECT j.id,j.retry_of_job_id FROM jobs j WHERE j.id=ANY($1::text[])
 UNION SELECT j.id,j.retry_of_job_id FROM ancestors a JOIN jobs j ON j.id=a.retry_of_job_id
)
SELECT id FROM ancestors WHERE retry_of_job_id IS NULL
UNION SELECT m.root_id FROM job_retry_chain_members m WHERE m.job_id=ANY($1::text[])
 ORDER BY 1
