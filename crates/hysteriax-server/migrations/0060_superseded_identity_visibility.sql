-- Never-published retired generations are superseded targets. Make their
-- staged leaves unusable in the same commit that retires the generation;
-- bounded work subsequently materializes the old physical discard flags.
-- Preserve explicit historic releases and every immutable certificate byte.
CREATE OR REPLACE VIEW mtls_user_version_publication AS
 SELECT v.identity_id,v.credential_id,v.credential_version,v.ca_id,v.ca_version,
        v.not_before,v.expires_at,v.next_action_at,
        (v.released OR g.published_at IS NOT NULL) AS released,
        (v.discarded OR (NOT v.released AND g.state='retired' AND g.published_at IS NULL)) AS discarded,
        v.created_at
 FROM mtls_user_versions v
 JOIN mtls_ca_generations g ON g.ca_id=v.ca_id AND g.ca_version=v.ca_version;
