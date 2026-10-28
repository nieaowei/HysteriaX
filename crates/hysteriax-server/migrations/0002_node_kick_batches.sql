-- Run maintenance with every API replica stopped. Old revocation obligations and
-- kick history are intentionally discarded; there is no old payload executor.
CREATE TEMP TABLE retired_node_kicks ON COMMIT DROP AS SELECT id FROM jobs WHERE kind='kick';

DELETE FROM execution_recheck_requests q USING execution_rechecks c,execution_operations o,jobs j
WHERE q.namespace=c.request_namespace AND q.request_digest=c.request_digest AND c.operation_id=o.operation_id AND o.work_id=j.id AND j.kind='kick';
DELETE FROM execution_rechecks c USING execution_operations o,jobs j
WHERE c.operation_id=o.operation_id AND o.work_id=j.id AND j.kind='kick';
DELETE FROM execution_operations o USING jobs j WHERE o.work_id=j.id AND j.kind='kick';
UPDATE resource_leases l SET owner=NULL,expires_at=NULL,work_id=NULL,generation=l.generation+1,blocked=EXISTS(SELECT 1 FROM execution_operations o WHERE o.resource_key=l.resource_key AND o.state IN ('dispatching','reconciling','needs_attention'))
WHERE l.work_id IN (SELECT id FROM retired_node_kicks);
DELETE FROM resource_leases l WHERE l.resource_key IN (SELECT 'job:'||id FROM retired_node_kicks) AND NOT EXISTS(SELECT 1 FROM execution_operations o WHERE o.resource_key=l.resource_key);
DELETE FROM kick_requests;
UPDATE jobs SET retry_of_job_id=NULL WHERE retry_of_job_id IN (SELECT id FROM jobs WHERE kind='kick');
DELETE FROM jobs WHERE kind='kick';
DELETE FROM job_retry_chain_members WHERE job_id IN (SELECT id FROM retired_node_kicks) OR root_id IN (SELECT id FROM retired_node_kicks);
DELETE FROM job_retry_chain_state WHERE leaf_id IN (SELECT id FROM retired_node_kicks) OR root_id IN (SELECT id FROM retired_node_kicks);
DELETE FROM job_retry_state_dirty WHERE job_id IN (SELECT id FROM retired_node_kicks);

ALTER TABLE kick_requests ADD COLUMN execution_generation bigint;
DROP INDEX jobs_active_kick_idx;
CREATE UNIQUE INDEX jobs_active_kick_idx ON jobs(node_id)
WHERE kind='kick' AND status IN ('queued','running');
CREATE INDEX kick_requests_active_job_idx ON kick_requests(node_id,latest_job_id,user_id)
WHERE state='active';
