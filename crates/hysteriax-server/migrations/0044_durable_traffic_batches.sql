-- Reserve ordering before contacting the node; late responses cannot roll a
-- counter baseline backwards across samples from the same service instance.
CREATE TABLE traffic_sample_batches (
    sample_id text PRIMARY KEY,
    sample_order bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
    node_id text NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    cycle_id uuid,
    instance_id text,
    state text NOT NULL DEFAULT 'capturing' CHECK (state IN ('capturing','ready','complete','discarded')),
    payload_digest text,
    item_count bigint NOT NULL DEFAULT 0,
    processed_count bigint NOT NULL DEFAULT 0,
    sampled_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    capture_deadline timestamptz NOT NULL DEFAULT clock_timestamp()+interval '60 seconds',
    completed_at timestamptz,
    UNIQUE(sample_id,node_id),
    CHECK (processed_count >= 0 AND processed_count <= item_count)
);
CREATE INDEX traffic_sample_batches_pending ON traffic_sample_batches(sample_order) WHERE state='ready';
CREATE INDEX traffic_sample_batches_node_pending ON traffic_sample_batches(node_id,sample_order) WHERE state IN ('capturing','ready');
CREATE INDEX traffic_sample_batches_capture_deadline ON traffic_sample_batches(capture_deadline) WHERE state='capturing';
CREATE INDEX traffic_sample_batches_cleanup ON traffic_sample_batches(node_id,sampled_at DESC,sample_id DESC);

CREATE TABLE traffic_sample_items (
    id text PRIMARY KEY DEFAULT gen_random_uuid()::text,
    sample_id text NOT NULL,
    node_id text NOT NULL,
    user_id text NOT NULL,
    tx_total bigint NOT NULL CHECK (tx_total >= 0),
    rx_total bigint NOT NULL CHECK (rx_total >= 0),
    processed_at timestamptz,
    sampled_at timestamptz NOT NULL,
    UNIQUE(sample_id,user_id),
    FOREIGN KEY(sample_id,node_id) REFERENCES traffic_sample_batches(sample_id,node_id) ON DELETE CASCADE
);
CREATE INDEX traffic_sample_items_pending ON traffic_sample_items(sample_id,user_id) WHERE processed_at IS NULL;
CREATE INDEX traffic_sample_items_cleanup ON traffic_sample_items(node_id,sampled_at DESC,id DESC);
ALTER TABLE traffic_baselines ADD COLUMN sample_order bigint NOT NULL DEFAULT 0;
ALTER TABLE traffic_records ADD COLUMN sample_id text;
CREATE UNIQUE INDEX traffic_records_sample_user ON traffic_records(sample_id,user_id) WHERE sample_id IS NOT NULL;
CREATE INDEX traffic_sample_batches_completed_node ON traffic_sample_batches(node_id,sample_order DESC) WHERE state='complete';
