CREATE TABLE admin_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);

CREATE TABLE nodes (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    ssh_host TEXT NOT NULL,
    ssh_port INTEGER NOT NULL CHECK (ssh_port BETWEEN 1 AND 65535),
    ssh_username TEXT NOT NULL,
    ssh_auth_type TEXT NOT NULL CHECK (ssh_auth_type IN ('password', 'private_key')),
    ssh_secret_enc TEXT NOT NULL,
    ssh_passphrase_enc TEXT,
    ssh_host_fingerprint TEXT,
    public_host TEXT NOT NULL,
    public_port INTEGER NOT NULL CHECK (public_port BETWEEN 1 AND 65535),
    listen_addr TEXT NOT NULL,
    tls_sni TEXT,
    tls_skip_verify BOOLEAN NOT NULL DEFAULT FALSE,
    node_token_hash TEXT NOT NULL UNIQUE,
    node_token_enc TEXT NOT NULL,
    traffic_stats_secret_enc TEXT NOT NULL,
    desired_config_enc TEXT NOT NULL,
    desired_revision BIGINT NOT NULL DEFAULT 1,
    deployed_config_enc TEXT,
    deployed_revision BIGINT,
    deployed_content_sha256 TEXT,
    state TEXT NOT NULL DEFAULT 'new',
    last_seen_at TIMESTAMPTZ,
    last_sample_at TIMESTAMPTZ,
    proxy_probe_url TEXT,
    traffic_stats_port INTEGER NOT NULL DEFAULT 9780 CHECK (traffic_stats_port BETWEEN 1 AND 65535),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE config_versions (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL,
    config_enc TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    deployed_success BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE(node_id, revision)
);

CREATE TABLE config_resources (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    resource_kind TEXT NOT NULL CHECK (resource_kind IN ('certificate', 'private_key', 'ech_key', 'acl', 'geoip', 'geosite')),
    content_enc TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    size_bytes BIGINT NOT NULL CHECK (size_bytes BETWEEN 1 AND 20971520),
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE(node_id, name)
);

CREATE INDEX config_resources_node_idx ON config_resources(node_id, resource_kind);

CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    expires_at TIMESTAMPTZ,
    quota_bytes BIGINT CHECK (quota_bytes IS NULL OR quota_bytes >= 0),
    usage_bytes BIGINT NOT NULL DEFAULT 0 CHECK (usage_bytes >= 0),
    revision BIGINT NOT NULL DEFAULT 1,
    quota_reset_at TIMESTAMPTZ,
    access_kick_enqueued_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE node_assignments (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    credential_hash TEXT NOT NULL,
    credential_enc TEXT NOT NULL,
    client_certificate_enc TEXT,
    client_private_key_enc TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(user_id, node_id),
    UNIQUE(node_id, credential_hash)
);

CREATE TABLE subscription_credentials (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    token_enc TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ
);

CREATE TABLE traffic_records (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    instance_id TEXT NOT NULL,
    baseline_tx BIGINT NOT NULL CHECK (baseline_tx >= 0),
    baseline_rx BIGINT NOT NULL CHECK (baseline_rx >= 0),
    delta_tx BIGINT NOT NULL CHECK (delta_tx >= 0),
    delta_rx BIGINT NOT NULL CHECK (delta_rx >= 0),
    gap_reason TEXT,
    sampled_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE traffic_baselines (
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    instance_id TEXT NOT NULL,
    tx_total BIGINT NOT NULL CHECK (tx_total >= 0),
    rx_total BIGINT NOT NULL CHECK (rx_total >= 0),
    sampled_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(node_id, user_id)
);

CREATE TABLE data_gaps (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    opened_at TIMESTAMPTZ NOT NULL,
    resolved_at TIMESTAMPTZ,
    reason TEXT NOT NULL
);

CREATE INDEX data_gaps_node_open_idx ON data_gaps(node_id, resolved_at);

CREATE TABLE jobs (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    node_id TEXT REFERENCES nodes(id) ON DELETE SET NULL,
    target_revision BIGINT,
    payload_json JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(payload_json) = 'object'),
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'rolled_back', 'cancelled')),
    stage TEXT NOT NULL,
    result_json JSONB CHECK (result_json IS NULL OR jsonb_typeof(result_json) = 'object'),
    error_message TEXT,
    logs_json JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(logs_json) = 'array'),
    attempts BIGINT NOT NULL DEFAULT 0,
    max_attempts BIGINT NOT NULL DEFAULT 3,
    available_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ
);

CREATE INDEX jobs_status_available_idx ON jobs(status, available_at, created_at);
CREATE INDEX jobs_node_idx ON jobs(node_id, created_at DESC);

CREATE TABLE job_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    payload_json JSONB NOT NULL CHECK (jsonb_typeof(payload_json) = 'object'),
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX job_events_job_id_idx ON job_events(job_id, id);

CREATE TABLE audit_records (
    id TEXT PRIMARY KEY NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    detail_json JSONB NOT NULL CHECK (jsonb_typeof(detail_json) = 'object'),
    created_at TIMESTAMPTZ NOT NULL
);

CREATE OR REPLACE FUNCTION hysteriax_serialize_writes() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(62177345902811);
    RETURN NULL;
END;
$$;

CREATE TABLE deployment_probe_tokens (
    node_id TEXT PRIMARY KEY NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

DO $$
DECLARE
    table_name text;
BEGIN
    FOREACH table_name IN ARRAY ARRAY[
        'admin_tokens', 'nodes', 'config_versions', 'config_resources', 'users',
        'node_assignments', 'subscription_credentials', 'traffic_records',
        'traffic_baselines', 'data_gaps', 'jobs', 'job_events', 'audit_records',
        'deployment_probe_tokens'
    ] LOOP
        EXECUTE format(
            'CREATE TRIGGER hysteriax_serialize_writes BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_serialize_writes()',
            table_name
        );
    END LOOP;
END;
$$;
