-- Existing nodes and batches retain cumulative-counter semantics on upgrade.
-- Only nodes created after the migration start in the explicit cutover state.
ALTER TABLE nodes ADD COLUMN traffic_capture_mode text NOT NULL DEFAULT 'cumulative'
    CHECK (traffic_capture_mode IN ('cumulative','cutover','reset','interval'));
ALTER TABLE nodes ALTER COLUMN traffic_capture_mode SET DEFAULT 'cutover';

ALTER TABLE traffic_sample_batches ADD COLUMN capture_mode text NOT NULL DEFAULT 'cumulative'
    CHECK (capture_mode IN ('cumulative','cutover','reset','interval'));
ALTER TABLE traffic_sample_batches ADD COLUMN cutover_checked boolean NOT NULL DEFAULT false;
ALTER TABLE traffic_sample_batches ADD COLUMN cutover_gap boolean NOT NULL DEFAULT false;
ALTER TABLE traffic_sample_batches ADD COLUMN interval_checked boolean NOT NULL DEFAULT false;
ALTER TABLE traffic_sample_batches ADD COLUMN interval_restart boolean NOT NULL DEFAULT false;
-- A marker is committed before the destructive read. These fields identify
-- both resource leases that authorized the one attempt, even after takeover.
ALTER TABLE traffic_sample_batches ADD COLUMN clear_requested_at timestamptz;
ALTER TABLE traffic_sample_batches ADD COLUMN clear_owner text;
ALTER TABLE traffic_sample_batches ADD COLUMN clear_work_generation bigint;
ALTER TABLE traffic_sample_batches ADD COLUMN clear_node_generation bigint;
ALTER TABLE traffic_sample_batches ADD COLUMN clear_epoch text;
ALTER TABLE traffic_sample_batches ADD CONSTRAINT traffic_clear_request_identity CHECK (
    (clear_requested_at IS NULL AND clear_owner IS NULL AND clear_work_generation IS NULL
        AND clear_node_generation IS NULL AND clear_epoch IS NULL)
    OR
    (clear_requested_at IS NOT NULL AND clear_owner IS NOT NULL AND clear_work_generation IS NOT NULL
        AND clear_node_generation IS NOT NULL AND clear_epoch IS NOT NULL AND capture_mode <> 'cumulative')
);

-- Progress for the explicitly invoked one-time historical cleanup. These
-- tables do not schedule cleanup and do not alter the billing ledger.
CREATE TABLE traffic_history_cleanup_runs (
    id text PRIMARY KEY,
    started_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    finished_at timestamptz,
    backup_sha256 text NOT NULL,
    candidates jsonb NOT NULL
);
CREATE TABLE traffic_history_cleanup_nodes (
    run_id text NOT NULL REFERENCES traffic_history_cleanup_runs(id),
    node_id text NOT NULL,
    high_time timestamptz NOT NULL,
    high_id text NOT NULL,
    cursor_time timestamptz,
    cursor_id text,
    deleted_count bigint NOT NULL DEFAULT 0,
    done boolean NOT NULL DEFAULT false,
    PRIMARY KEY(run_id,node_id),
    CHECK ((cursor_time IS NULL) = (cursor_id IS NULL))
);
