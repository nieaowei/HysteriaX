-- Match the complete cleanup ordering. A node/time index alone permits a
-- global time scan plus incremental sort, which can read other nodes' entire
-- recent history before reaching an old node. Pre-create these concurrently
-- before deploying to an existing large database.
CREATE INDEX IF NOT EXISTS traffic_records_node_cleanup_idx
    ON traffic_records(node_id, sampled_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS node_network_samples_node_cleanup_idx
    ON node_network_samples(node_id, sampled_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS proxy_probe_samples_node_cleanup_idx
    ON proxy_probe_samples(node_id, sampled_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS online_samples_node_cleanup_idx
    ON online_samples(node_id, sampled_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS user_node_usage_node_cleanup_idx
    ON user_node_usage(node_id, sampled_at DESC, user_id DESC);
