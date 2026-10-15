-- Lightweight directory headers; secret payloads and full summaries are hydrated
-- only after SQL filtering, ordering and pagination have selected a page.
CREATE FUNCTION hysteriax_rfc3339(value text) RETURNS timestamptz
LANGUAGE plpgsql STABLE STRICT AS $$
BEGIN
 IF value !~ '^\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2}(\.\d+)?([Zz]|[+-]\d{2}:\d{2})$' THEN RETURN NULL; END IF;
 RETURN value::timestamptz;
EXCEPTION WHEN invalid_datetime_format OR datetime_field_overflow THEN RETURN NULL;
END $$;
CREATE FUNCTION hysteriax_directory_user_status(enabled boolean,expires timestamptz,quota bigint,usage bigint) RETURNS text
LANGUAGE sql STABLE AS $$ SELECT CASE WHEN NOT enabled THEN 'disabled' WHEN expires<=now() THEN 'expired' WHEN usage>=quota THEN 'quota_exhausted' ELSE 'active' END $$;
CREATE FUNCTION hysteriax_directory_credential_status(archived boolean,expires timestamptz) RETURNS text
LANGUAGE sql STABLE AS $$ SELECT CASE WHEN archived THEN 'archived' WHEN floor(trunc(extract(epoch FROM expires-now()))/86400)<0 THEN 'expired' WHEN floor(trunc(extract(epoch FROM expires-now()))/86400)<=30 THEN 'expiring' ELSE 'active' END $$;
CREATE VIEW credential_directory AS
 SELECT c.id,c.name,c.kind,c.owner_user_id,c.created_at,
  hysteriax_directory_credential_status(c.archived,coalesce(hysteriax_rfc3339(v.metadata->>'expires_at'),c.reminder_at)) status,
  'managed'::text source,c.id entity_id,NULL::text node_id
 FROM credentials c JOIN credential_versions v ON v.credential_id=c.id AND v.version=c.latest_version
 UNION ALL
 SELECT 'admin:'||id,label,'admin_token',NULL,created_at,CASE WHEN revoked_at IS NULL THEN 'active' ELSE 'revoked' END,'admin',id,NULL FROM admin_tokens
 UNION ALL
 SELECT 'subscription:'||s.id,u.name||' · 订阅','subscription_token',s.user_id,s.created_at,
  CASE WHEN s.revoked_at IS NOT NULL THEN 'revoked' ELSE hysteriax_directory_user_status(u.enabled,u.expires_at,u.quota_bytes,u.usage_bytes) END,
  'subscription',s.id,NULL
 FROM subscription_credentials s JOIN users u ON u.id=s.user_id
 UNION ALL
 SELECT 'user:'||a.user_id||':'||a.node_id,u.name||' · '||n.name,'user_credential',a.user_id,a.created_at,
  hysteriax_directory_user_status(u.enabled,u.expires_at,u.quota_bytes,u.usage_bytes),'assignment',a.user_id,a.node_id
 FROM node_assignments a JOIN users u ON u.id=a.user_id JOIN nodes n ON n.id=a.node_id;
CREATE INDEX credential_name_page ON credentials(lower(name) COLLATE "C",name COLLATE "C",id COLLATE "C");
CREATE INDEX credential_kind_page ON credentials(lower(kind) COLLATE "C",lower(name) COLLATE "C",name COLLATE "C",id COLLATE "C");
CREATE INDEX credential_name_trgm ON credentials USING gin(lower(name) public.gin_trgm_ops);
