CREATE TABLE mtls_node_operations (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id),
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    ca_id TEXT NOT NULL,
    ca_version BIGINT NOT NULL,
    rotation_id TEXT REFERENCES mtls_rotations(id),
    stage TEXT NOT NULL CHECK(stage IN ('trust','cleanup')),
    desired_revision BIGINT NOT NULL,
    use_desired BOOLEAN NOT NULL DEFAULT FALSE,
    base_revision BIGINT,
    base_activation_id TEXT REFERENCES mtls_activations(id),
    base_digest TEXT,
    effective_version BIGINT,
    created_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY(ca_id,ca_version) REFERENCES mtls_ca_generations(ca_id,ca_version)
);
CREATE INDEX mtls_node_operations_phase ON mtls_node_operations(ca_id,ca_version,stage,node_id);
CREATE TABLE mtls_cleanup_nodes (
    ca_id TEXT NOT NULL,
    ca_version BIGINT NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    job_id TEXT REFERENCES jobs(id),
    completed BOOLEAN NOT NULL DEFAULT FALSE,
    error_message TEXT,
    PRIMARY KEY(ca_id,ca_version,node_id),
    FOREIGN KEY(ca_id,ca_version) REFERENCES mtls_trust_cleanup(ca_id,ca_version)
);
CREATE TABLE mtls_active_probes (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    ca_id TEXT NOT NULL,
    ca_version BIGINT NOT NULL,
    credential_id TEXT NOT NULL,
    credential_version BIGINT NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1,
    FOREIGN KEY(ca_id,ca_version) REFERENCES mtls_ca_generations(ca_id,ca_version),
    FOREIGN KEY(node_id,credential_id,credential_version) REFERENCES mtls_probe_versions(node_id,credential_id,credential_version)
);
CREATE TABLE mtls_deployment_snapshots (
    node_id TEXT NOT NULL,
    revision BIGINT NOT NULL,
    activation_id TEXT NOT NULL REFERENCES mtls_activations(id),
    snapshot_enc TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(node_id,revision,activation_id)
);
CREATE INDEX mtls_deployment_snapshot_history ON mtls_deployment_snapshots(node_id,revision,created_at DESC);
ALTER TABLE mtls_rotations ADD COLUMN actor_id TEXT NOT NULL DEFAULT 'system:mtls';
ALTER TABLE mtls_rotations ADD COLUMN published_at TIMESTAMPTZ;
ALTER TABLE mtls_rotations ADD COLUMN prepared_count BIGINT NOT NULL DEFAULT 0;
ALTER TABLE mtls_rotations ADD COLUMN retry_requested BOOLEAN NOT NULL DEFAULT FALSE;
DO $$ DECLARE t text; BEGIN
 FOREACH t IN ARRAY ARRAY['mtls_node_operations','mtls_cleanup_nodes','mtls_active_probes','mtls_deployment_snapshots'] LOOP
 EXECUTE format('CREATE TRIGGER hysteriax_serialize_writes BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_serialize_writes()',t);
 EXECUTE format('CREATE TRIGGER domain_changed AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_domain_changed(%L)',t,'node_config,user_config,credentials,groups');
 END LOOP;
END $$;
-- A newer attempt does not erase uncertainty about an earlier remote switch.
-- Only a later confirmed deployment supersedes those potential trust references.
CREATE VIEW mtls_live_activation_trust AS
 SELECT a.node_id,a.id AS activation_id,a.generation,t.ca_id,t.ca_version
 FROM mtls_activations a
 JOIN nodes n ON n.id=a.node_id
 JOIN mtls_activation_trust t ON t.activation_id=a.id
 WHERE a.phase IN ('activating','verified','recovery_required')
 AND NOT EXISTS(SELECT 1 FROM mtls_activations newer WHERE newer.node_id=a.node_id AND newer.phase='committed' AND newer.generation>a.generation);
ALTER TABLE mtls_rotations ADD COLUMN previous_version BIGINT;
ALTER TABLE mtls_node_operations ADD COLUMN recovery BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE mtls_rotations ADD COLUMN outcome TEXT;
ALTER TABLE mtls_activations ADD COLUMN previous_activation_id TEXT REFERENCES mtls_activations(id);

-- Global expiry scans drive bounded private-material retention batches.
CREATE INDEX mtls_user_versions_expired ON mtls_user_versions(expires_at,credential_id,credential_version);
CREATE INDEX mtls_probe_versions_expired ON mtls_probe_versions(expires_at,credential_id,credential_version);
CREATE INDEX mtls_rotations_batch ON mtls_rotations(batch_id);
