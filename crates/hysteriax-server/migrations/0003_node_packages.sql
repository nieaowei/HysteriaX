CREATE TABLE node_packages (
    node_id TEXT PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    config JSONB NOT NULL DEFAULT '{}'::jsonb,
    usage_bytes BIGINT NOT NULL DEFAULT 0 CHECK (usage_bytes >= 0),
    period_id TEXT NOT NULL,
    next_reset_at TIMESTAMPTZ,
    generation BIGINT NOT NULL DEFAULT 0,
    boot_id TEXT,
    interface TEXT,
    tx_total BIGINT,
    rx_total BIGINT,
    sampled_at TIMESTAMPTZ,
    gap_reason TEXT,
    restricted BOOLEAN NOT NULL DEFAULT FALSE
);
INSERT INTO node_packages (node_id, period_id) SELECT id, id FROM nodes;
CREATE TABLE node_network_samples (
    id BIGSERIAL PRIMARY KEY,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    period_id TEXT NOT NULL,
    delta_tx BIGINT NOT NULL CHECK (delta_tx >= 0),
    delta_rx BIGINT NOT NULL CHECK (delta_rx >= 0),
    gap_reason TEXT,
    sampled_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX node_network_samples_node_idx ON node_network_samples(node_id, sampled_at);
CREATE TABLE node_alerts (
    id TEXT PRIMARY KEY,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    scope TEXT NOT NULL,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE(node_id, kind, scope)
);

CREATE TRIGGER hysteriax_serialize_writes BEFORE INSERT OR UPDATE OR DELETE ON node_packages FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_serialize_writes();
CREATE TRIGGER hysteriax_serialize_writes BEFORE INSERT OR UPDATE OR DELETE ON node_network_samples FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_serialize_writes();
CREATE TRIGGER hysteriax_serialize_writes BEFORE INSERT OR UPDATE OR DELETE ON node_alerts FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_serialize_writes();
