-- Stable bounded retention scans, including short-lived probe credentials.
CREATE INDEX monitoring_probe_tokens_expiry_idx ON monitoring_probe_tokens(expires_at,node_id);
CREATE INDEX online_samples_retention_idx ON online_samples(sampled_at,id);
CREATE INDEX proxy_probe_samples_retention_idx ON proxy_probe_samples(sampled_at,id);
