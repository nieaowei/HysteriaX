CREATE TABLE admin_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT
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
    tls_skip_verify INTEGER NOT NULL DEFAULT 0 CHECK (tls_skip_verify IN (0, 1)),
    node_token_hash TEXT NOT NULL UNIQUE,
    node_token_enc TEXT NOT NULL,
    traffic_stats_secret_enc TEXT NOT NULL,
    desired_config_enc TEXT NOT NULL,
    desired_revision INTEGER NOT NULL DEFAULT 1,
    deployed_config_enc TEXT,
    deployed_revision INTEGER,
    deployed_content_sha256 TEXT,
    state TEXT NOT NULL DEFAULT 'new',
    last_seen_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE config_versions (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    config_enc TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    deployed_success INTEGER NOT NULL DEFAULT 0 CHECK (deployed_success IN (0, 1)),
    created_at TEXT NOT NULL,
    UNIQUE(node_id, revision)
);

CREATE TABLE config_resources (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    resource_kind TEXT NOT NULL CHECK (resource_kind IN ('certificate', 'private_key', 'ech_key', 'acl', 'geoip', 'geosite')),
    content_enc TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK (size_bytes BETWEEN 1 AND 20971520),
    created_at TEXT NOT NULL,
    UNIQUE(node_id, name)
);

CREATE INDEX config_resources_node_idx ON config_resources(node_id, resource_kind);

CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    expires_at TEXT,
    quota_bytes INTEGER CHECK (quota_bytes IS NULL OR quota_bytes >= 0),
    usage_bytes INTEGER NOT NULL DEFAULT 0 CHECK (usage_bytes >= 0),
    revision INTEGER NOT NULL DEFAULT 1,
    quota_reset_at TEXT,
    access_kick_enqueued_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE node_assignments (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    credential_hash TEXT NOT NULL,
    credential_enc TEXT NOT NULL,
    client_certificate_enc TEXT,
    client_private_key_enc TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY(user_id, node_id),
    UNIQUE(node_id, credential_hash)
);

CREATE TABLE subscription_credentials (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    token_enc TEXT NOT NULL,
    created_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE TABLE traffic_records (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    instance_id TEXT NOT NULL,
    baseline_tx INTEGER NOT NULL CHECK (baseline_tx >= 0),
    baseline_rx INTEGER NOT NULL CHECK (baseline_rx >= 0),
    delta_tx INTEGER NOT NULL CHECK (delta_tx >= 0),
    delta_rx INTEGER NOT NULL CHECK (delta_rx >= 0),
    gap_reason TEXT,
    sampled_at TEXT NOT NULL
);

CREATE TABLE traffic_baselines (
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    instance_id TEXT NOT NULL,
    tx_total INTEGER NOT NULL CHECK (tx_total >= 0),
    rx_total INTEGER NOT NULL CHECK (rx_total >= 0),
    sampled_at TEXT NOT NULL,
    PRIMARY KEY(node_id, user_id)
);

CREATE TABLE data_gaps (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    opened_at TEXT NOT NULL,
    resolved_at TEXT,
    reason TEXT NOT NULL
);

CREATE INDEX data_gaps_node_open_idx ON data_gaps(node_id, resolved_at);

CREATE TABLE jobs (
    id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL,
    node_id TEXT REFERENCES nodes(id) ON DELETE SET NULL,
    target_revision INTEGER,
    payload_json TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'rolled_back', 'cancelled')),
    stage TEXT NOT NULL,
    result_json TEXT,
    error_message TEXT,
    logs_json TEXT NOT NULL DEFAULT '[]',
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    available_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT
);

CREATE INDEX jobs_status_available_idx ON jobs(status, available_at, created_at);
CREATE INDEX jobs_node_idx ON jobs(node_id, created_at DESC);

CREATE TABLE job_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX job_events_job_id_idx ON job_events(job_id, id);

CREATE TABLE audit_records (
    id TEXT PRIMARY KEY NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    detail_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);
