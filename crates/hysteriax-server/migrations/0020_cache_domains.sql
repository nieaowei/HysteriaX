CREATE TABLE cache_domain_versions(domain TEXT PRIMARY KEY,generation BIGINT NOT NULL DEFAULT 0);
INSERT INTO cache_domain_versions(domain) SELECT unnest(ARRAY['node_config','nodes','user_config','users','groups','credentials','resources','dns','jobs','audit','traffic','network','online','probe','gaps']);
CREATE FUNCTION hysteriax_domain_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 UPDATE cache_domain_versions SET generation=generation+1 WHERE domain=ANY(string_to_array(TG_ARGV[0],','));
 RETURN NULL;
END $$;
DO $$ DECLARE pair text[]; BEGIN
 FOREACH pair SLICE 1 IN ARRAY ARRAY[
 ['config_versions','node_config,credentials'],['config_resources','resources'],
 ['node_assignments','user_config,node_config,credentials'],['subscription_credentials','credentials,user_config'],
 ['credentials','credentials'],['credential_versions','credentials'],['credential_batches','credentials'],
 ['credential_batch_items','credentials'],['dns_credential_batch_items','credentials,dns'],
 ['authorization_groups','groups'],['authorization_group_users','groups,user_config'],['authorization_group_nodes','groups,node_config'],
 ['dns_connections','dns'],['dns_zones','dns'],['dns_records','dns'],['dns_bindings','dns,node_config'],
 ['node_alerts','nodes'],['traffic_records','traffic'],['traffic_baselines','traffic'],
 ['node_network_samples','network'],['online_samples','online'],['proxy_probe_samples','probe'],
 ['monitoring_probe_state','probe'],['data_gaps','gaps,traffic'],['jobs','jobs'],['job_events','jobs'],['audit_records','audit']
 ] LOOP
 EXECUTE format('CREATE TRIGGER domain_changed AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_domain_changed(%L)',pair[1],pair[2]);
 END LOOP;
END $$;
CREATE FUNCTION hysteriax_domain_row_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE domains text[]; old_config jsonb; new_config jsonb;
BEGIN
 IF TG_OP='UPDATE' AND to_jsonb(NEW)=to_jsonb(OLD) THEN RETURN NULL; END IF;
 IF TG_TABLE_NAME='nodes' THEN
  domains:=ARRAY['nodes'];
  IF TG_OP='UPDATE' THEN
   old_config:=to_jsonb(OLD)-ARRAY['last_seen_at','last_sample_at','updated_at','state'];
   new_config:=to_jsonb(NEW)-ARRAY['last_seen_at','last_sample_at','updated_at','state'];
  END IF;
  IF TG_OP<>'UPDATE' OR old_config<>new_config THEN domains:=domains||ARRAY['node_config']; END IF;
 ELSIF TG_TABLE_NAME='users' THEN
  domains:=ARRAY['users'];
  IF TG_OP='UPDATE' THEN
   old_config:=to_jsonb(OLD)-ARRAY['usage_bytes','updated_at','access_kick_enqueued_at'];
   new_config:=to_jsonb(NEW)-ARRAY['usage_bytes','updated_at','access_kick_enqueued_at'];
  END IF;
  IF TG_OP<>'UPDATE' OR old_config<>new_config THEN domains:=domains||ARRAY['user_config']; END IF;
 ELSIF TG_TABLE_NAME='node_packages' THEN
  domains:=ARRAY['nodes'];
  IF TG_OP<>'UPDATE' OR OLD.config<>NEW.config OR OLD.generation<>NEW.generation THEN domains:=domains||ARRAY['node_config']; END IF;
 ELSE
  domains:=ARRAY['credentials'];
  IF TG_OP='UPDATE' AND to_jsonb(NEW)-'last_used_at'=to_jsonb(OLD)-'last_used_at' THEN RETURN NULL; END IF;
 END IF;
 UPDATE cache_domain_versions SET generation=generation+1 WHERE domain=ANY(domains);
 RETURN NULL;
END $$;
CREATE TRIGGER domain_row_changed AFTER INSERT OR UPDATE OR DELETE ON nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_domain_row_changed();
CREATE TRIGGER domain_row_changed AFTER INSERT OR UPDATE OR DELETE ON users FOR EACH ROW EXECUTE FUNCTION hysteriax_domain_row_changed();
CREATE TRIGGER domain_row_changed AFTER INSERT OR UPDATE OR DELETE ON node_packages FOR EACH ROW EXECUTE FUNCTION hysteriax_domain_row_changed();
CREATE TRIGGER domain_row_changed AFTER INSERT OR UPDATE OR DELETE ON admin_tokens FOR EACH ROW EXECUTE FUNCTION hysteriax_domain_row_changed();
