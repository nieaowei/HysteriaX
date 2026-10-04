-- Monitoring history is independent of accounting records and quota baselines.
CREATE TABLE online_samples (
    id BIGSERIAL PRIMARY KEY,
    cycle_id UUID NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    sampled_at TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL,
    connections JSONB NOT NULL DEFAULT '{}'::jsonb,
    UNIQUE(cycle_id, node_id)
);
CREATE INDEX online_samples_time_idx ON online_samples(sampled_at);
CREATE INDEX online_samples_node_time_idx ON online_samples(node_id, sampled_at DESC);
CREATE TABLE proxy_probe_samples (
    id BIGSERIAL PRIMARY KEY,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL,
    sampled_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    status TEXT NOT NULL,
    external_status TEXT,
    connection_ms DOUBLE PRECISION,
    latency_ms DOUBLE PRECISION,
    reason TEXT,
    UNIQUE(node_id, sampled_at)
);
CREATE INDEX proxy_probe_samples_time_idx ON proxy_probe_samples(sampled_at);
CREATE INDEX proxy_probe_samples_node_time_idx ON proxy_probe_samples(node_id, sampled_at DESC);
CREATE TABLE monitoring_leases (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    owner UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE TABLE monitoring_probe_tokens (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX traffic_records_time_idx ON traffic_records(sampled_at);
CREATE INDEX node_network_samples_time_idx ON node_network_samples(sampled_at);
ALTER TABLE online_samples ADD COLUMN cycle_started_at TIMESTAMPTZ NOT NULL DEFAULT now();
ALTER TABLE online_samples ADD COLUMN expected_nodes BIGINT NOT NULL DEFAULT 1;
CREATE INDEX online_samples_cycle_time_idx ON online_samples(cycle_started_at);
CREATE TABLE monitoring_probe_state (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL,
    health TEXT NOT NULL DEFAULT 'checking',
    failures INTEGER NOT NULL DEFAULT 0,
    successes INTEGER NOT NULL DEFAULT 0,
    sampled_at TIMESTAMPTZ NOT NULL
);
-- Preserve billing deltas while identifying baseline observations for analytics.
ALTER TABLE traffic_records ADD COLUMN baseline_only BOOLEAN NOT NULL DEFAULT FALSE;
UPDATE traffic_records SET baseline_only=TRUE WHERE id IN (
    SELECT DISTINCT ON(node_id,user_id) id FROM traffic_records ORDER BY node_id,user_id,sampled_at,id
);
ALTER TABLE node_network_samples ADD COLUMN baseline_only BOOLEAN NOT NULL DEFAULT FALSE;
UPDATE node_network_samples SET baseline_only=TRUE WHERE id IN (
    SELECT min(id) FROM node_network_samples GROUP BY node_id,period_id
);
-- A successful empty traffic response is a measured zero, unlike a failed sample.
ALTER TABLE online_samples ADD COLUMN traffic_status TEXT NOT NULL DEFAULT 'unknown';
