-- A resource lease is retained after release so fencing generations never reset.
CREATE TABLE service_instances (
    instance_id text PRIMARY KEY,
    started_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    heartbeat_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE TABLE resource_leases (
    resource_key text PRIMARY KEY,
    owner text,
    generation bigint NOT NULL DEFAULT 0 CHECK (generation >= 0),
    expires_at timestamptz,
    work_id text,
    blocked boolean NOT NULL DEFAULT false,
    CHECK ((owner IS NULL) = (expires_at IS NULL))
);
CREATE INDEX resource_leases_expiry ON resource_leases(expires_at) WHERE owner IS NOT NULL;
CREATE TABLE execution_operations (
    operation_id text PRIMARY KEY,
    resource_key text NOT NULL REFERENCES resource_leases(resource_key),
    owner text NOT NULL,
    generation bigint NOT NULL,
    work_id text NOT NULL,
    step text NOT NULL,
    state text NOT NULL CHECK (state IN ('dispatching','verified','reconciling','needs_attention')),
    intent jsonb NOT NULL,
    evidence jsonb,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (work_id, step)
);
CREATE INDEX execution_operations_uncertain ON execution_operations(resource_key)
WHERE state IN ('dispatching','reconciling','needs_attention');
