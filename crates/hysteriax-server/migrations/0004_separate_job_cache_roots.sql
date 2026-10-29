-- Keep task progress invalidation in the jobs domain. Do not also serialize
-- unrelated authorization writes on the same node entity version row.
CREATE OR REPLACE FUNCTION hysteriax_cache_row_changed() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE old_data jsonb; new_data jsonb; data jsonb; domains text[]; domain_name text; entity_key text;
 directory_changed boolean:=TG_ARGV[1]='true'; quota_changed boolean:=false; root record;
BEGIN
 IF TG_OP<>'INSERT' THEN old_data:=to_jsonb(OLD); END IF;
 IF TG_OP<>'DELETE' THEN new_data:=to_jsonb(NEW); END IF;
 IF TG_OP='UPDATE' AND old_data=new_data THEN RETURN NULL; END IF;
 domains:=string_to_array(nullif(TG_ARGV[0],''),',');
 IF TG_TABLE_NAME='nodes' THEN
  domains:=ARRAY['nodes'];
  IF TG_OP<>'UPDATE' OR old_data-ARRAY['last_seen_at','last_sample_at','updated_at','state'] IS DISTINCT FROM new_data-ARRAY['last_seen_at','last_sample_at','updated_at','state'] THEN domains:=domains||ARRAY['node_config']; END IF;
  directory_changed:=TG_OP<>'UPDATE' OR old_data-ARRAY['last_seen_at','last_sample_at','updated_at'] IS DISTINCT FROM new_data-ARRAY['last_seen_at','last_sample_at','updated_at'];
  quota_changed:=TG_OP<>'UPDATE' OR (old_data->'id',old_data->'name') IS DISTINCT FROM (new_data->'id',new_data->'name');
 ELSIF TG_TABLE_NAME='users' THEN
  domains:=ARRAY['users'];
  directory_changed:=TG_OP<>'UPDATE' OR old_data-ARRAY['usage_bytes','updated_at','access_kick_enqueued_at'] IS DISTINCT FROM new_data-ARRAY['usage_bytes','updated_at','access_kick_enqueued_at'];
  IF directory_changed THEN domains:=domains||ARRAY['user_config']; END IF;
 ELSIF TG_TABLE_NAME='node_packages' THEN
  domains:=ARRAY['nodes'];
  directory_changed:=TG_OP<>'UPDATE' OR (old_data->'config',old_data->'generation') IS DISTINCT FROM (new_data->'config',new_data->'generation');
  IF directory_changed THEN domains:=domains||ARRAY['node_config']; END IF;
  quota_changed:=TG_OP<>'UPDATE' OR (old_data->'usage_bytes',old_data->'next_reset_at',old_data#>'{config,quota_bytes}') IS DISTINCT FROM (new_data->'usage_bytes',new_data->'next_reset_at',new_data#>'{config,quota_bytes}');
 ELSIF TG_TABLE_NAME='admin_tokens' THEN
  IF TG_OP='UPDATE' AND old_data-'last_used_at'=new_data-'last_used_at' THEN RETURN NULL; END IF;
  domains:=ARRAY['credentials']; directory_changed:=true;
 END IF;
 FOREACH data IN ARRAY ARRAY[old_data,new_data] LOOP
  IF data IS NULL THEN CONTINUE; END IF;
  entity_key:=coalesce(data->>'node_id',data->>'user_id',data->>'credential_id',data->>'ca_id',data->>'identity_id',data->>'connection_id',data->>'zone_id',data->>'group_id',data->>'batch_id',data->>'job_id',data->>'id',data::text);
  FOREACH domain_name IN ARRAY coalesce(domains,ARRAY[]::text[]) LOOP
   PERFORM hysteriax_cache_touch_domain(domain_name,entity_key);
  END LOOP;
  IF directory_changed OR domains && ARRAY['node_config','user_config','credentials','resources','groups','dns'] THEN
   PERFORM hysteriax_cache_touch_domain('directory',entity_key);
  END IF;
  IF TG_TABLE_NAME=ANY(ARRAY['nodes','users','credentials','authorization_groups','dns_connections','dns_zones','dns_records','config_resources','credential_batches','jobs','subscription_templates']) THEN
   PERFORM hysteriax_cache_touch_entity(TG_TABLE_NAME,data->>'id');
  END IF;
  FOR root IN SELECT * FROM (VALUES ('nodes','node_id'),('users','user_id'),('users','owner_user_id'),('credentials','credential_id'),('authorization_groups','group_id'),('credential_batches','batch_id'),('jobs','job_id'),('jobs','retry_of_job_id'),('dns_connections','connection_id'),('dns_zones','zone_id')) mappings(kind,field) LOOP
   -- Node detail policies already depend on the jobs domain. Bumping the
   -- node's entity counter again makes progress race authorization commits.
   IF TG_TABLE_NAME='jobs' AND root.kind='nodes' THEN CONTINUE; END IF;
   PERFORM hysteriax_cache_touch_entity(root.kind,data->>root.field);
  END LOOP;
  IF quota_changed THEN
   entity_key:=CASE WHEN TG_TABLE_NAME='nodes' THEN data->>'id' ELSE data->>'node_id' END;
   PERFORM hysteriax_cache_touch_domain('quota_rank',entity_key);
   PERFORM hysteriax_cache_touch_entity('quota_rank',entity_key);
  END IF;
  IF TG_TABLE_NAME='jobs' THEN
   FOR root IN SELECT batch_id FROM credential_batch_items WHERE job_id=data->>'id' UNION SELECT batch_id FROM dns_credential_batch_items WHERE job_id=data->>'id' LOOP
    PERFORM hysteriax_cache_touch_entity('credential_batches',root.batch_id);
   END LOOP;
  END IF;
 END LOOP;
 RETURN NULL;
END $$;
