-- Keep the original application result when a batch item hands off to sync.
-- Historical applied items are preserved, without inventing execution proof.
CREATE TABLE credential_application_receipts (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id),
    source JSONB NOT NULL,
    execution JSONB NOT NULL,
    stage TEXT NOT NULL,
    result JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
