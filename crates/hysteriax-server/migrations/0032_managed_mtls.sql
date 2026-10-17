-- Managed identities are independent of business user revisions and node
-- snapshots. Every relationship pins immutable credential versions.
ALTER TABLE credentials DROP CONSTRAINT credentials_kind_check;
ALTER TABLE credentials ADD CONSTRAINT credentials_kind_check CHECK (
    kind IN ('ssh_private_key', 'ssh_password', 'tls_identity', 'ca_certificate',
             'ca_identity', 'certificate', 'private_key', 'ech_key', 'dns', 'api_token')
);

CREATE TABLE mtls_authorities (
    credential_id TEXT PRIMARY KEY REFERENCES credentials(id) ON DELETE CASCADE,
    source TEXT NOT NULL CHECK (source IN ('generated','imported')),
    policy_revision BIGINT NOT NULL DEFAULT 1 CHECK (policy_revision > 0),
    client_validity_days INTEGER NOT NULL DEFAULT 180 CHECK (client_validity_days BETWEEN 1 AND 365),
    renew_before_hours INTEGER NOT NULL DEFAULT 720 CHECK (renew_before_hours >= 1 AND renew_before_hours < client_validity_days * 24),
    published_version BIGINT NOT NULL DEFAULT 1,
    next_action_at TIMESTAMPTZ,
    maintenance_error TEXT,
    FOREIGN KEY (credential_id,published_version) REFERENCES credential_versions(credential_id,version)
);
CREATE INDEX mtls_authorities_due ON mtls_authorities(next_action_at,credential_id) WHERE next_action_at IS NOT NULL;
CREATE TABLE mtls_ca_generations (
    ca_id TEXT NOT NULL REFERENCES mtls_authorities(credential_id) ON DELETE CASCADE,
    ca_version BIGINT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('preparing','published','retired')),
    expires_at TIMESTAMPTZ NOT NULL,
    published_at TIMESTAMPTZ,
    PRIMARY KEY(ca_id,ca_version),
    FOREIGN KEY(ca_id,ca_version) REFERENCES credential_versions(credential_id,version)
);
CREATE UNIQUE INDEX mtls_ca_one_publication ON mtls_ca_generations(ca_id) WHERE state='published';
CREATE UNIQUE INDEX mtls_ca_one_preparation ON mtls_ca_generations(ca_id) WHERE state='preparing';

CREATE TABLE mtls_user_identities (
    id TEXT PRIMARY KEY,
    -- Retain the stable owner ID after account deletion so still-valid archived
    -- leaves continue protecting their issuing CA/trust cleanup deadline.
    user_id TEXT NOT NULL,
    ca_id TEXT NOT NULL REFERENCES mtls_authorities(credential_id),
    credential_id TEXT UNIQUE REFERENCES credentials(id),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision>0),
    authorization_revision BIGINT NOT NULL DEFAULT 1 CHECK (authorization_revision>0),
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    next_action_at TIMESTAMPTZ,
    maintenance_error TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE(user_id,ca_id)
);
CREATE INDEX mtls_user_identities_due ON mtls_user_identities(next_action_at,id) WHERE NOT archived AND next_action_at IS NOT NULL;
CREATE INDEX mtls_user_identities_ca ON mtls_user_identities(ca_id,user_id);
CREATE TABLE mtls_user_versions (
    identity_id TEXT NOT NULL REFERENCES mtls_user_identities(id) ON DELETE CASCADE,
    credential_id TEXT NOT NULL,
    credential_version BIGINT NOT NULL,
    ca_id TEXT NOT NULL,
    ca_version BIGINT NOT NULL,
    not_before TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL CHECK(expires_at>not_before),
    next_action_at TIMESTAMPTZ NOT NULL,
    released BOOLEAN NOT NULL DEFAULT FALSE,
    discarded BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(identity_id,credential_version),
    FOREIGN KEY(credential_id,credential_version) REFERENCES credential_versions(credential_id,version),
    FOREIGN KEY(ca_id,ca_version) REFERENCES mtls_ca_generations(ca_id,ca_version)
);
CREATE INDEX mtls_user_versions_ca_expiry ON mtls_user_versions(ca_id,ca_version,expires_at);
CREATE INDEX mtls_user_versions_selection ON mtls_user_versions(identity_id,ca_version,credential_version DESC) WHERE released AND NOT discarded;
CREATE INDEX mtls_user_versions_credential ON mtls_user_versions(credential_id,credential_version);

CREATE TABLE mtls_probe_versions (
    -- Deleted nodes still contribute unexpired probe leaves to CA retention.
    node_id TEXT NOT NULL,
    credential_id TEXT NOT NULL,
    credential_version BIGINT NOT NULL,
    ca_id TEXT NOT NULL,
    ca_version BIGINT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    next_action_at TIMESTAMPTZ NOT NULL,
    discarded BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY(node_id,credential_id,credential_version),
    FOREIGN KEY(credential_id,credential_version) REFERENCES credential_versions(credential_id,version),
    FOREIGN KEY(ca_id,ca_version) REFERENCES mtls_ca_generations(ca_id,ca_version)
);
CREATE INDEX mtls_probe_versions_due ON mtls_probe_versions(next_action_at,node_id,ca_id,ca_version);
CREATE INDEX mtls_probe_versions_ca_expiry ON mtls_probe_versions(ca_id,ca_version,expires_at);
CREATE INDEX mtls_probe_versions_credential ON mtls_probe_versions(credential_id,credential_version);

-- Keep both desired and remotely deployed membership until a successful switch.
CREATE TABLE mtls_node_trust (
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    ca_id TEXT NOT NULL,
    ca_version BIGINT NOT NULL,
    desired BOOLEAN NOT NULL DEFAULT FALSE,
    deployed BOOLEAN NOT NULL DEFAULT FALSE,
    revision BIGINT NOT NULL DEFAULT 1,
    PRIMARY KEY(node_id,ca_id,ca_version),
    FOREIGN KEY(ca_id,ca_version) REFERENCES mtls_ca_generations(ca_id,ca_version)
);
CREATE INDEX mtls_node_trust_ca ON mtls_node_trust(ca_id,ca_version,node_id);
CREATE TABLE mtls_rotations (
    id TEXT PRIMARY KEY,
    ca_id TEXT NOT NULL,
    ca_version BIGINT NOT NULL,
    phase TEXT NOT NULL CHECK(phase IN ('trust','identities','publish','probes','complete','failed')),
    revision BIGINT NOT NULL DEFAULT 1,
    batch_id TEXT NOT NULL REFERENCES credential_batches(id),
    next_action_at TIMESTAMPTZ NOT NULL,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE(ca_id,ca_version),
    FOREIGN KEY(ca_id,ca_version) REFERENCES mtls_ca_generations(ca_id,ca_version)
);
CREATE UNIQUE INDEX mtls_rotations_serial ON mtls_rotations(ca_id) WHERE phase NOT IN ('complete');
CREATE INDEX mtls_rotations_due ON mtls_rotations(next_action_at,id) WHERE phase<>'complete';
CREATE TABLE mtls_rotation_nodes (
    rotation_id TEXT NOT NULL REFERENCES mtls_rotations(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    expected_revision BIGINT NOT NULL,
    trust_prepared BOOLEAN NOT NULL DEFAULT FALSE,
    probe_switched BOOLEAN NOT NULL DEFAULT FALSE,
    job_id TEXT REFERENCES jobs(id),
    error_message TEXT,
    PRIMARY KEY(rotation_id,node_id)
);
CREATE TABLE mtls_trust_cleanup (
    ca_id TEXT NOT NULL,
    ca_version BIGINT NOT NULL,
    next_action_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ,
    PRIMARY KEY(ca_id,ca_version),
    FOREIGN KEY(ca_id,ca_version) REFERENCES mtls_ca_generations(ca_id,ca_version)
);
CREATE INDEX mtls_trust_cleanup_due ON mtls_trust_cleanup(next_action_at,ca_id,ca_version) WHERE completed_at IS NULL;
CREATE TABLE mtls_operations (
    actor_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL CHECK(length(idempotency_key) BETWEEN 1 AND 180),
    operation TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    receipt JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(actor_id,idempotency_key)
);

DO $$ DECLARE t text; BEGIN
    FOREACH t IN ARRAY ARRAY[
        'mtls_authorities','mtls_ca_generations','mtls_user_identities','mtls_user_versions',
        'mtls_probe_versions','mtls_node_trust','mtls_rotations','mtls_rotation_nodes',
        'mtls_trust_cleanup','mtls_operations'
    ] LOOP
        EXECUTE format('CREATE TRIGGER hysteriax_serialize_writes BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_serialize_writes()',t);
    END LOOP;
END $$;

ALTER TABLE mtls_user_identities ADD CONSTRAINT mtls_identity_ca_unique UNIQUE(id,ca_id);
ALTER TABLE mtls_user_identities ADD CONSTRAINT mtls_identity_credential_unique UNIQUE(id,credential_id);
ALTER TABLE mtls_user_versions ADD CONSTRAINT mtls_version_identity_ca FOREIGN KEY(identity_id,ca_id) REFERENCES mtls_user_identities(id,ca_id);
ALTER TABLE mtls_user_versions ADD CONSTRAINT mtls_version_identity_credential FOREIGN KEY(identity_id,credential_id) REFERENCES mtls_user_identities(id,credential_id);

DO $$ DECLARE pair text[]; BEGIN
 FOREACH pair SLICE 1 IN ARRAY ARRAY[
 ['mtls_authorities','credentials,user_config,node_config,groups'],
 ['mtls_ca_generations','credentials,user_config,node_config,groups'],
 ['mtls_user_identities','credentials,user_config,groups'],
 ['mtls_user_versions','credentials,user_config,groups'],
 ['mtls_probe_versions','credentials,node_config'],
 ['mtls_node_trust','credentials,node_config,user_config,groups'],
 ['mtls_rotations','credentials,node_config,groups'],
 ['mtls_rotation_nodes','credentials,node_config,groups']
 ] LOOP
 EXECUTE format('CREATE TRIGGER domain_changed AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_domain_changed(%L)',pair[1],pair[2]);
 END LOOP;
END $$;
CREATE INDEX mtls_user_pending ON mtls_user_identities(id) WHERE NOT archived AND next_action_at IS NULL;
