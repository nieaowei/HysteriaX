-- Observations never mutate configuration revisions or mTLS activation state.
CREATE TABLE node_acme_observations (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    revision BIGINT NOT NULL,
    deployment_digest TEXT NOT NULL,
    policy_id TEXT NOT NULL,
    status JSONB NOT NULL,
    sampled_at TIMESTAMPTZ NOT NULL
);
