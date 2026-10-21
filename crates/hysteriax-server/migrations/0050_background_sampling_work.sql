-- The first creator freezes the membership and denominator of each DB-time slot.
CREATE TABLE sampling_cycles (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL,
    period_start TIMESTAMPTZ NOT NULL,
    deadline_at TIMESTAMPTZ NOT NULL,
    expected_nodes BIGINT NOT NULL CHECK(expected_nodes>=0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    UNIQUE(kind,period_start),
    CHECK(deadline_at>period_start)
);
CREATE TABLE background_work (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    cycle_id UUID REFERENCES sampling_cycles(id),
    node_id TEXT,
    payload JSONB NOT NULL DEFAULT '{}',
    state TEXT NOT NULL DEFAULT 'queued' CHECK(state IN ('queued','running','complete','failed','expired')),
    owner TEXT,
    generation BIGINT NOT NULL DEFAULT 0,
    available_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    deadline_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    error_code TEXT,
    UNIQUE(cycle_id,node_id),
    CHECK((state='running')=(owner IS NOT NULL))
);
CREATE INDEX background_work_claim_idx ON background_work(kind,available_at,id) WHERE state IN ('queued','running');
CREATE INDEX background_work_expiry_idx ON background_work(deadline_at,id) WHERE state IN ('queued','running');
CREATE INDEX background_work_node_idx ON background_work(node_id,id);
CREATE INDEX sampling_cycles_retention_idx ON sampling_cycles(period_start,id);
ALTER TABLE online_samples ADD COLUMN pending BOOLEAN NOT NULL DEFAULT FALSE;
