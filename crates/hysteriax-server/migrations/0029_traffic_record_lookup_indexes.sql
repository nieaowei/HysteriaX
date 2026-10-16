-- Apply during an API maintenance window. Lock contention fails atomically.
SET LOCAL lock_timeout = '5s';
CREATE INDEX traffic_records_user_node_time_idx ON traffic_records(user_id, node_id, sampled_at DESC);
CREATE INDEX traffic_records_node_time_idx ON traffic_records(node_id, sampled_at DESC);
