-- Written only by the explicit maintenance migration command. Instances must
-- agree on the schema, management protocol and shared encryption key.
CREATE TABLE runtime_contract (
    id boolean PRIMARY KEY DEFAULT true CHECK (id),
    schema_version bigint NOT NULL,
    management_protocol integer NOT NULL,
    master_key_fingerprint text NOT NULL,
    initialized_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
