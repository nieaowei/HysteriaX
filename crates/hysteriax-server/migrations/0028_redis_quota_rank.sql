-- Reuse the existing version directory; no billing columns or aggregate tables.
INSERT INTO cache_domain_versions(domain) VALUES ('quota_rank');
CREATE FUNCTION hysteriax_quota_rank_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE node text; revision bigint;
BEGIN
 IF TG_OP='TRUNCATE' THEN node:=NULL;
 ELSIF TG_TABLE_NAME='nodes' THEN
  IF TG_OP='UPDATE' AND NEW.name=OLD.name AND NEW.id=OLD.id THEN RETURN NULL; END IF;
  node:=CASE WHEN TG_OP='DELETE' THEN OLD.id ELSE NEW.id END;
 ELSE
  IF TG_OP='UPDATE' AND NEW.usage_bytes=OLD.usage_bytes
     AND NEW.next_reset_at IS NOT DISTINCT FROM OLD.next_reset_at
     AND NEW.config->'quota_bytes' IS NOT DISTINCT FROM OLD.config->'quota_bytes'
     THEN RETURN NULL; END IF;
  node:=CASE WHEN TG_OP='DELETE' THEN OLD.node_id ELSE NEW.node_id END;
 END IF;
 UPDATE cache_domain_versions SET generation=generation+1 WHERE domain='quota_rank' RETURNING generation INTO revision;
 PERFORM pg_notify('hysteriax_quota_rank',json_build_object('schema',current_schema(),'id',node,'version',revision)::text);
 RETURN NULL;
END $$;
CREATE TRIGGER quota_rank_changed AFTER INSERT OR UPDATE OR DELETE ON nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_quota_rank_changed();
CREATE TRIGGER quota_rank_changed AFTER INSERT OR UPDATE OR DELETE ON node_packages FOR EACH ROW EXECUTE FUNCTION hysteriax_quota_rank_changed();
CREATE TRIGGER quota_rank_truncated AFTER TRUNCATE ON nodes FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_quota_rank_changed();
CREATE TRIGGER quota_rank_truncated AFTER TRUNCATE ON node_packages FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_quota_rank_changed();
