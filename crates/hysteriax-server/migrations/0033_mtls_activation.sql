ALTER TABLE nodes ADD COLUMN mtls_state TEXT NOT NULL DEFAULT 'ready'
    CHECK(mtls_state IN ('ready','needs_configuration','activating','recovery_required'));
ALTER TABLE nodes ADD COLUMN mtls_error TEXT;
ALTER TABLE nodes ADD COLUMN mtls_generation BIGINT NOT NULL DEFAULT 0;
CREATE TABLE mtls_activations (
    id TEXT PRIMARY KEY,
    node_id TEXT NOT NULL,
    job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id),
    generation BIGINT NOT NULL,
    target_revision BIGINT NOT NULL,
    phase TEXT NOT NULL CHECK(phase IN ('activating','verified','committed','rolled_back','recovery_required')),
    previous_state TEXT NOT NULL,
    previous_digest TEXT,
    target_digest TEXT NOT NULL,
    snapshot_enc TEXT NOT NULL,
    trust JSONB NOT NULL,
    probe_credential_id TEXT,
    probe_credential_version BIGINT,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE(node_id,generation),
    CHECK((probe_credential_id IS NULL)=(probe_credential_version IS NULL)),
    FOREIGN KEY(probe_credential_id,probe_credential_version) REFERENCES credential_versions(credential_id,version)
);
CREATE INDEX mtls_activation_history ON mtls_activations(node_id,target_revision,generation DESC) WHERE phase='committed';
CREATE INDEX mtls_activation_recovery ON mtls_activations(node_id,phase) WHERE phase IN ('activating','verified','recovery_required');
ALTER TABLE nodes ADD COLUMN mtls_activation_id TEXT REFERENCES mtls_activations(id);
ALTER TABLE deployment_probe_tokens ADD COLUMN job_id TEXT REFERENCES jobs(id);
CREATE TABLE mtls_activation_trust (
    activation_id TEXT NOT NULL REFERENCES mtls_activations(id) ON DELETE CASCADE,
    ca_id TEXT NOT NULL,
    ca_version BIGINT NOT NULL,
    PRIMARY KEY(activation_id,ca_id,ca_version),
    FOREIGN KEY(ca_id,ca_version) REFERENCES mtls_ca_generations(ca_id,ca_version)
);
CREATE INDEX mtls_activation_ca ON mtls_activation_trust(ca_id,ca_version);
DO $$ DECLARE t text; BEGIN
 FOREACH t IN ARRAY ARRAY['mtls_activations','mtls_activation_trust'] LOOP
 EXECUTE format('CREATE TRIGGER hysteriax_serialize_writes BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_serialize_writes()',t);
 EXECUTE format('CREATE TRIGGER domain_changed AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_domain_changed(%L)',t,'node_config,user_config,credentials');
 END LOOP;
END $$;
ALTER TABLE nodes ADD COLUMN mtls_recovery_configured BOOLEAN NOT NULL DEFAULT TRUE;
