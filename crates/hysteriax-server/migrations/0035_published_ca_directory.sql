-- Prepared CA versions are not the operational certificate. Keep filtering,
-- ordering, expiry reminders and summaries aligned with the published generation.
CREATE OR REPLACE VIEW credential_directory AS
 SELECT c.id,c.name,c.kind,c.owner_user_id,c.created_at,
  hysteriax_directory_credential_status(c.archived,coalesce(hysteriax_rfc3339(v.metadata->>'expires_at'),c.reminder_at)) status,
  'managed'::text source,c.id entity_id,NULL::text node_id
 FROM credentials c LEFT JOIN mtls_authorities a ON a.credential_id=c.id AND c.kind='ca_identity' JOIN credential_versions v ON v.credential_id=c.id AND v.version=coalesce(a.published_version,c.latest_version)
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

-- Retire cached summaries written before this projection and response change.
UPDATE cache_domain_versions SET generation=generation+1
 WHERE domain IN ('credentials','node_config');
UPDATE cache_generation SET generation=generation+1,configuration_generation=configuration_generation+1;
