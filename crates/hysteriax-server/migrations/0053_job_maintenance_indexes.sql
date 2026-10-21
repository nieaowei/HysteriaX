-- Pending-only scans keep recurring reconciliation independent of old history.
CREATE INDEX jobs_running_recovery_idx ON jobs(updated_at,id) WHERE status='running';
CREATE INDEX execution_operations_recovery_idx ON execution_operations(work_id,state)
    WHERE state IN ('dispatching','reconciling','needs_attention');
CREATE INDEX resource_leases_work_expiry_idx ON resource_leases(work_id,expires_at)
    WHERE work_id IS NOT NULL;
CREATE INDEX kick_requests_schedule_idx ON kick_requests(updated_at,node_id,user_id)
    WHERE state IN ('active','waiting_recovery');
CREATE INDEX kick_requests_pending_node_idx ON kick_requests(node_id,user_id)
    WHERE state IN ('active','waiting_recovery');
