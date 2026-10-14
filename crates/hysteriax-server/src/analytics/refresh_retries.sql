WITH RECURSIVE paths AS (
 SELECT j.id root_id,j.id job_id,0 distance FROM jobs j WHERE j.id=ANY($1::text[]) AND j.retry_of_job_id IS NULL
 UNION ALL SELECT p.root_id,j.id,p.distance+1 FROM paths p JOIN jobs j ON j.retry_of_job_id=p.job_id
), members AS (
 INSERT INTO job_retry_chain_members SELECT job_id,root_id FROM paths ON CONFLICT(job_id) DO UPDATE SET root_id=EXCLUDED.root_id RETURNING job_id
), seeds AS (
 SELECT p.root_id,max(j.finished_at) failed_seed_at FROM paths p JOIN jobs j ON j.id=p.job_id WHERE j.status='failed' GROUP BY p.root_id
), errors AS (
 SELECT DISTINCT ON(p.root_id) p.root_id,j.error_message,j.finished_at FROM paths p JOIN jobs j ON j.id=p.job_id WHERE j.status IN ('failed','rolled_back') ORDER BY p.root_id,p.distance DESC
)
INSERT INTO job_retry_chain_state
SELECT j.id,p.root_id,coalesce(j.node_name,j.kind),j.status,e.error_message,e.finished_at,s.failed_seed_at FROM paths p JOIN jobs j ON j.id=p.job_id JOIN seeds s USING(root_id) LEFT JOIN errors e USING(root_id) WHERE NOT EXISTS(SELECT 1 FROM jobs child WHERE child.retry_of_job_id=j.id)
