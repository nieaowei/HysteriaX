-- APIs must be stopped. Historical SQL checksums remain unchanged.
-- Empty databases need only final DDL. Populated legacy databases must first use
-- scripts/migrate-legacy-data.sh with their existing master key.
DO $$
BEGIN
 IF (EXISTS(SELECT 1 FROM nodes) OR EXISTS(SELECT 1 FROM credentials)
     OR EXISTS(SELECT 1 FROM config_resources) OR EXISTS(SELECT 1 FROM config_versions))
    AND (SELECT count(*) FROM credential_migrations
         WHERE name IN ('credentials_v2','tls_pairs_v1','managed_mtls_reset_v1')) <> 3 THEN
   RAISE EXCEPTION 'Legacy credential conversion is incomplete; run scripts/migrate-legacy-data.sh before upgrading';
 END IF;
 IF EXISTS(SELECT 1 FROM node_assignments WHERE mtls_credential_id IS NOT NULL OR mtls_credential_version IS NOT NULL) THEN
   RAISE EXCEPTION 'Obsolete per-node mTLS bindings remain; inspect and complete legacy mTLS reset before upgrading';
 END IF;
 IF EXISTS(SELECT 1 FROM users WHERE connection_credential_hash IS NULL OR connection_credential_enc IS NULL) THEN
   RAISE EXCEPTION 'User password backfill is incomplete; run scripts/migrate-legacy-data.sh before upgrading';
 END IF;
 IF EXISTS(SELECT 1 FROM traffic_sample_batches b WHERE b.state IN ('capturing','ready')
           AND NOT EXISTS(SELECT 1 FROM background_work w WHERE w.id=b.sample_id AND w.kind='traffic' AND w.node_id=b.node_id)) THEN
   RAISE EXCEPTION 'Legacy traffic batches have no execution work; run scripts/migrate-legacy-data.sh before upgrading';
 END IF;
END $$;

ALTER TABLE nodes DROP COLUMN IF EXISTS ssh_secret_enc,
 DROP COLUMN IF EXISTS ssh_passphrase_enc, DROP COLUMN IF EXISTS ssh_auth_type,
 ALTER COLUMN ssh_credential_id SET NOT NULL,
 ALTER COLUMN ssh_credential_version SET NOT NULL;
ALTER TABLE node_assignments DROP COLUMN IF EXISTS client_certificate_enc,
 DROP COLUMN IF EXISTS client_private_key_enc,
 DROP COLUMN mtls_credential_id, DROP COLUMN mtls_credential_version;
ALTER TABLE credentials DROP CONSTRAINT credentials_kind_check;
ALTER TABLE credentials ADD CONSTRAINT credentials_kind_check
 CHECK (kind IN ('ssh_private_key','ssh_password','tls_identity','ca_certificate','ca_identity','ech_key','dns','api_token'));
DROP TABLE credential_migrations;
DELETE FROM background_initialization WHERE key='legacy-traffic-work-v1';
