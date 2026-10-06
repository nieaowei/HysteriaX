ALTER TABLE nodes ADD COLUMN published_connection JSONB;
UPDATE nodes SET published_connection = jsonb_build_object(
    'public_host', public_host, 'public_port', public_port, 'listen_addr', listen_addr,
    'tls_sni', tls_sni, 'tls_skip_verify', tls_skip_verify
) WHERE deployed_revision IS NOT NULL;

CREATE TABLE dns_connections (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    provider TEXT NOT NULL CHECK (provider = 'cloudflare'),
    credential_id TEXT NOT NULL,
    credential_version BIGINT NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1,
    status TEXT NOT NULL DEFAULT 'unverified',
    verified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (credential_id, credential_version) REFERENCES credential_versions(credential_id, version)
);
CREATE TABLE dns_zones (
    id TEXT PRIMARY KEY,
    connection_id TEXT NOT NULL REFERENCES dns_connections(id),
    provider_zone_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL UNIQUE,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    synced_at TIMESTAMPTZ,
    revision BIGINT NOT NULL DEFAULT 1
);
CREATE TABLE dns_records (
    id TEXT PRIMARY KEY,
    zone_id TEXT NOT NULL REFERENCES dns_zones(id),
    provider_record_id TEXT,
    name TEXT NOT NULL,
    record_type TEXT NOT NULL,
    content TEXT NOT NULL,
    ttl BIGINT NOT NULL DEFAULT 1,
    proxied BOOLEAN NOT NULL DEFAULT FALSE,
    origin TEXT NOT NULL DEFAULT 'external' CHECK (origin IN ('external', 'hysteriax')),
    revision BIGINT NOT NULL DEFAULT 1,
    remote_snapshot JSONB,
    desired JSONB,
    state TEXT NOT NULL DEFAULT 'pending',
    resolution_status TEXT NOT NULL DEFAULT 'unchecked',
    resolution_detail JSONB,
    checked_at TIMESTAMPTZ,
    deleted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (zone_id, provider_record_id)
);
CREATE INDEX dns_records_zone_name ON dns_records(zone_id, name);
CREATE TABLE dns_bindings (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    zone_id TEXT NOT NULL REFERENCES dns_zones(id),
    hostname TEXT NOT NULL UNIQUE,
    record_ids JSONB NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE dns_operations (
    id TEXT PRIMARY KEY,
    idempotency_key TEXT NOT NULL UNIQUE,
    request_sha256 TEXT NOT NULL,
    job_id TEXT REFERENCES jobs(id),
    connection_id TEXT NOT NULL REFERENCES dns_connections(id),
    credential_version BIGINT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    action TEXT NOT NULL,
    payload JSONB NOT NULL,
    result JSONB,
    applied_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE dns_credential_batch_items (
    batch_id TEXT NOT NULL REFERENCES credential_batches(id),
    connection_id TEXT NOT NULL REFERENCES dns_connections(id),
    job_id TEXT NOT NULL REFERENCES jobs(id),
    expected_revision BIGINT NOT NULL,
    applied_at TIMESTAMPTZ,
    PRIMARY KEY (batch_id, connection_id)
);
ALTER TABLE jobs ADD COLUMN resource_key TEXT;
ALTER TABLE jobs ADD COLUMN resource_type TEXT;
ALTER TABLE jobs ADD COLUMN resource_id TEXT;
ALTER TABLE jobs ADD COLUMN resource_name TEXT;
CREATE INDEX jobs_resource_active ON jobs(resource_key) WHERE status IN ('queued', 'running');

DO $$ DECLARE t text; BEGIN
    FOREACH t IN ARRAY ARRAY['dns_connections', 'dns_zones', 'dns_records', 'dns_bindings', 'dns_operations', 'dns_credential_batch_items'] LOOP
        EXECUTE format('CREATE TRIGGER hysteriax_serialize_writes BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_serialize_writes()', t);
    END LOOP;
END $$;
