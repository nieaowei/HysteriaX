-- Publication is one generation transition. Staged material becomes available
-- atomically without rewriting every retained leaf's physical release flag.
-- Explicit historic release flags remain authoritative for retained snapshots.
CREATE VIEW mtls_user_version_publication AS
 SELECT v.identity_id,v.credential_id,v.credential_version,v.ca_id,v.ca_version,
        v.not_before,v.expires_at,v.next_action_at,
        (v.released OR g.published_at IS NOT NULL) AS released,
        v.discarded,v.created_at
 FROM mtls_user_versions v
 JOIN mtls_ca_generations g ON g.ca_id=v.ca_id AND g.ca_version=v.ca_version;
CREATE INDEX mtls_user_versions_current
 ON mtls_user_versions(identity_id,ca_version,credential_version DESC)
 WHERE NOT discarded;
-- A scheduling marker is derived metadata, not user authorization or a business
-- revision. No historical source rows or certificate bytes are backfilled.
ALTER TABLE mtls_user_identities ADD COLUMN scheduled_ca_version BIGINT
 CHECK(scheduled_ca_version>0);
ALTER TABLE mtls_user_identities ADD CONSTRAINT mtls_identity_scheduled_generation
 FOREIGN KEY(ca_id,scheduled_ca_version) REFERENCES mtls_ca_generations(ca_id,ca_version);
CREATE INDEX mtls_identity_schedule_pending
 ON mtls_user_identities(ca_id,scheduled_ca_version,user_id,id)
 WHERE NOT archived;
-- Keep the legacy physical flags in bounded pages too. This is a materialized
-- representation of the same release fact, never a second publication decision.
CREATE INDEX mtls_user_versions_release_pending
 ON mtls_user_versions(ca_id,ca_version,identity_id,credential_version)
 WHERE NOT released AND NOT discarded;
