-- All API replicas must be stopped before applying this migration.
CREATE TABLE user_credentials (
    id text PRIMARY KEY,
    user_id text NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('connection', 'subscription')),
    secret_hash text NOT NULL,
    secret_enc text NOT NULL,
    created_at timestamptz NOT NULL,
    revoked_at timestamptz
);
CREATE UNIQUE INDEX user_credentials_active_user_kind ON user_credentials(user_id,kind) WHERE revoked_at IS NULL;
CREATE UNIQUE INDEX user_credentials_active_hash ON user_credentials(kind,secret_hash) WHERE revoked_at IS NULL;
CREATE INDEX user_credentials_user_history ON user_credentials(user_id,kind,created_at,id);

-- Copy encrypted material without changing either existing secret.
INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at)
 SELECT 'connection:'||id,id,'connection',connection_credential_hash,connection_credential_enc,created_at
 FROM users WHERE connection_credential_hash IS NOT NULL AND connection_credential_enc IS NOT NULL;
INSERT INTO user_credentials(id,user_id,kind,secret_hash,secret_enc,created_at,revoked_at)
 SELECT id,user_id,'subscription',token_hash,token_enc,created_at,revoked_at FROM subscription_credentials;

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
   FROM (user_credentials s
     JOIN users u ON ((u.id = s.user_id)))
 WHERE s.kind = 'subscription'
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
 WHERE EXISTS (SELECT 1 FROM user_credentials c WHERE c.user_id=u.id AND c.kind='connection' AND c.revoked_at IS NULL);

DROP TABLE subscription_credentials;
ALTER TABLE users DROP COLUMN connection_credential_hash, DROP COLUMN connection_credential_enc;

-- Both authentication and generated subscriptions must see committed rotations.
CREATE TRIGGER cache_row_changed AFTER INSERT OR UPDATE OR DELETE ON user_credentials
 FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials,user_config', 'true');
CREATE TRIGGER cache_truncated AFTER TRUNCATE ON user_credentials
 FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();
