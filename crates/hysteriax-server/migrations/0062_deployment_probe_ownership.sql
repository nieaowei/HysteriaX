-- Existing credentials have no proof of an execution incarnation. Keep their
-- history, but only freshly issued, lease-bound credentials can authenticate.
ALTER TABLE deployment_probe_tokens
    ADD COLUMN owner TEXT,
    ADD COLUMN node_generation BIGINT,
    ADD COLUMN job_generation BIGINT,
    ADD CONSTRAINT deployment_probe_execution_scope CHECK (
        (owner IS NULL AND node_generation IS NULL AND job_generation IS NULL)
        OR (owner IS NOT NULL AND owner<>'' AND node_generation>0 AND job_generation>0
            AND node_generation IS NOT NULL AND job_generation IS NOT NULL)
    );
