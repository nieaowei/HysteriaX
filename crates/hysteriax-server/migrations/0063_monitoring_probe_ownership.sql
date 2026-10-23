-- Retain legacy credentials without treating their expiry as execution proof.
ALTER TABLE monitoring_probe_tokens
    ADD COLUMN work_id TEXT,
    ADD COLUMN owner TEXT,
    ADD COLUMN node_generation BIGINT,
    ADD COLUMN work_generation BIGINT,
    ADD CONSTRAINT monitoring_probe_execution_scope CHECK (
        (work_id IS NULL AND owner IS NULL AND node_generation IS NULL AND work_generation IS NULL)
        OR (work_id IS NOT NULL AND work_id<>'' AND owner IS NOT NULL AND owner<>''
            AND node_generation IS NOT NULL AND node_generation>0
            AND work_generation IS NOT NULL AND work_generation>0)
    );
