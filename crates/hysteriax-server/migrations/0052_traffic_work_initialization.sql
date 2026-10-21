CREATE TABLE background_initialization (
    key TEXT PRIMARY KEY,
    completed_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX traffic_sample_batches_capture_expiry_idx
    ON traffic_sample_batches(capture_deadline,sample_id) WHERE state='capturing';
CREATE INDEX traffic_sample_batches_unfinished_order_idx
    ON traffic_sample_batches(sample_order) WHERE state IN ('capturing','ready');
