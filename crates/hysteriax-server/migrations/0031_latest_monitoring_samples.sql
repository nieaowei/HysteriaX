-- Match the latest-sample queries exactly, including null ordering, so a
-- node with no samples does not walk the global time index to exhaustion.
SET LOCAL lock_timeout = '5s';
CREATE INDEX online_samples_node_latest_idx
    ON online_samples(node_id, sampled_at DESC NULLS LAST, id DESC);
CREATE INDEX proxy_probe_samples_node_latest_idx
    ON proxy_probe_samples(node_id, sampled_at DESC NULLS LAST);
