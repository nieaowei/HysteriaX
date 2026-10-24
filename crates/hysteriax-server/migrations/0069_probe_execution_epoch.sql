-- Existing probe credentials have no proof of their maintenance incarnation.
-- Preserve their bytes and require a fresh guarded issuance before acceptance.
ALTER TABLE deployment_probe_tokens ADD COLUMN epoch text;
ALTER TABLE monitoring_probe_tokens ADD COLUMN epoch text;
