CREATE TABLE credentials (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('ssh_private_key', 'ssh_password', 'tls_identity', 'certificate', 'private_key', 'ech_key', 'dns', 'api_token')),
    owner_user_id TEXT,
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    latest_version BIGINT NOT NULL DEFAULT 1 CHECK (latest_version > 0),
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    reminder_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE credential_versions (
    credential_id TEXT NOT NULL REFERENCES credentials(id) ON DELETE CASCADE,
    version BIGINT NOT NULL CHECK (version > 0),
    payload_enc TEXT NOT NULL,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Migrated resources keep their remote filename so old snapshots render identically.
    artifact_names JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (credential_id, version)
);

CREATE FUNCTION hysteriax_immutable_credential_version() RETURNS trigger
LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'credential versions are immutable'; END $$;
CREATE TRIGGER immutable_credential_version BEFORE UPDATE ON credential_versions
FOR EACH ROW EXECUTE FUNCTION hysteriax_immutable_credential_version();

ALTER TABLE nodes ADD COLUMN ssh_credential_id TEXT REFERENCES credentials(id);
ALTER TABLE nodes ADD COLUMN ssh_credential_version BIGINT;
ALTER TABLE nodes ADD CONSTRAINT nodes_ssh_credential_fkey FOREIGN KEY (ssh_credential_id, ssh_credential_version)
    REFERENCES credential_versions(credential_id, version);
ALTER TABLE node_assignments ADD COLUMN mtls_credential_id TEXT REFERENCES credentials(id);
ALTER TABLE node_assignments ADD COLUMN mtls_credential_version BIGINT;
ALTER TABLE node_assignments ADD CONSTRAINT assignment_mtls_credential_pair
    CHECK ((mtls_credential_id IS NULL) = (mtls_credential_version IS NULL));
ALTER TABLE node_assignments ADD CONSTRAINT assignment_mtls_credential_fkey FOREIGN KEY (mtls_credential_id, mtls_credential_version)
    REFERENCES credential_versions(credential_id, version);

CREATE TABLE credential_batches (
    id TEXT PRIMARY KEY,
    credential_id TEXT NOT NULL REFERENCES credentials(id),
    version BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (credential_id, version) REFERENCES credential_versions(credential_id, version)
);
CREATE TABLE credential_batch_items (
    batch_id TEXT NOT NULL REFERENCES credential_batches(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL,
    user_id TEXT NOT NULL DEFAULT '',
    expected_revision BIGINT NOT NULL,
    job_id TEXT NOT NULL REFERENCES jobs(id),
    applied_at TIMESTAMPTZ,
    apply_stage TEXT,
    CHECK ((applied_at IS NULL) = (apply_stage IS NULL)),
    PRIMARY KEY(batch_id, node_id, user_id)
);

CREATE TABLE credential_migrations (name TEXT PRIMARY KEY, completed_at TIMESTAMPTZ NOT NULL);
DO $$ DECLARE t text; BEGIN
    FOREACH t IN ARRAY ARRAY['credentials', 'credential_versions', 'credential_batches', 'credential_batch_items', 'credential_migrations'] LOOP
        EXECUTE format('CREATE TRIGGER hysteriax_serialize_writes BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_serialize_writes()', t);
    END LOOP;
END $$;
