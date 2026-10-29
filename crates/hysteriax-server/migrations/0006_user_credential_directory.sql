-- Connection passwords belong to users, independently of node assignments.
CREATE OR REPLACE VIEW credential_directory AS
 SELECT c.id,
    c.name,
    c.kind,
    c.owner_user_id,
    c.created_at,
    hysteriax_directory_credential_status(c.archived, COALESCE(hysteriax_rfc3339((v.metadata ->> 'expires_at'::text)), c.reminder_at)) AS status,
    'managed'::text AS source,
    c.id AS entity_id,
    NULL::text AS node_id
   FROM ((credentials c
     LEFT JOIN mtls_authorities a ON (((a.credential_id = c.id) AND (c.kind = 'ca_identity'::text))))
     JOIN credential_versions v ON (((v.credential_id = c.id) AND (v.version = COALESCE(a.published_version, c.latest_version)))))
UNION ALL
 SELECT ('admin:'::text || admin_tokens.id) AS id,
    admin_tokens.label AS name,
    'admin_token'::text AS kind,
    NULL::text AS owner_user_id,
    admin_tokens.created_at,
        CASE
            WHEN (admin_tokens.revoked_at IS NULL) THEN 'active'::text
            ELSE 'revoked'::text
        END AS status,
    'admin'::text AS source,
    admin_tokens.id AS entity_id,
    NULL::text AS node_id
   FROM admin_tokens
UNION ALL
 SELECT ('subscription:'::text || s.id) AS id,
    (u.name || ' · 订阅'::text) AS name,
    'subscription_token'::text AS kind,
    s.user_id AS owner_user_id,
    s.created_at,
        CASE
            WHEN (s.revoked_at IS NOT NULL) THEN 'revoked'::text
            ELSE hysteriax_directory_user_status(u.enabled, u.expires_at, u.quota_bytes, u.usage_bytes)
        END AS status,
    'subscription'::text AS source,
    s.id AS entity_id,
    NULL::text AS node_id
   FROM (subscription_credentials s
     JOIN users u ON ((u.id = s.user_id)))
UNION ALL
 SELECT 'user:'::text || u.id AS id,
    u.name,
    'user_credential'::text AS kind,
    u.id AS owner_user_id,
    u.created_at,
    hysteriax_directory_user_status(u.enabled, u.expires_at, u.quota_bytes, u.usage_bytes) AS status,
    'user'::text AS source,
    u.id AS entity_id,
    NULL::text AS node_id
 FROM users u
 WHERE u.connection_credential_hash IS NOT NULL AND u.connection_credential_enc IS NOT NULL;
