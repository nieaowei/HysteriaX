CREATE TABLE execution_recheck_requests (
    namespace text PRIMARY KEY,
    request_digest text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
-- A typed, idempotent remote-fence intent can be resumed after response loss.
-- Ordinary execution_operations remain non-replayable.
CREATE TABLE execution_rechecks (
    id text PRIMARY KEY,
    request_namespace text NOT NULL UNIQUE,
    request_digest text NOT NULL,
    operation_id text NOT NULL,
    prepared jsonb NOT NULL,
    state text NOT NULL CHECK(state IN ('prepared','dispatching','verified','needs_attention','obsolete')),
    proof jsonb,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE UNIQUE INDEX execution_rechecks_active ON execution_rechecks(operation_id) WHERE state IN ('prepared','dispatching');
CREATE INDEX execution_rechecks_history ON execution_rechecks(operation_id,created_at,id);

-- Keyset pages must not scan completed execution history.
CREATE INDEX execution_operations_recheck_page ON execution_operations(operation_id)
    WHERE state IN ('dispatching','reconciling','needs_attention');
CREATE INDEX execution_operations_work_recheck_page ON execution_operations(work_id,operation_id)
    WHERE state IN ('dispatching','reconciling','needs_attention');
