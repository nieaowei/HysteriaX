-- Snapshot identity is rotated only during explicit maintenance initialization.
CREATE TABLE cache_version_epoch(id boolean PRIMARY KEY CHECK(id),epoch text NOT NULL);
INSERT INTO cache_version_epoch VALUES(true,gen_random_uuid()::text);
CREATE TABLE cache_bucket_versions(
    domain text NOT NULL,bucket smallint NOT NULL CHECK(bucket>=0 AND bucket<64),
    generation bigint NOT NULL DEFAULT 0,PRIMARY KEY(domain,bucket)
);
INSERT INTO cache_bucket_versions(domain,bucket)
SELECT domain,bucket FROM (SELECT domain FROM cache_domain_versions UNION SELECT 'directory') d
CROSS JOIN generate_series(0,63) bucket;
CREATE FUNCTION hysteriax_cache_bucket(entity_id text) RETURNS smallint
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
 SELECT (get_byte(decode(md5(entity_id),'hex'),0)%64)::smallint
$$;
CREATE TABLE cache_entity_versions(
    kind text NOT NULL,entity_id text NOT NULL,generation bigint NOT NULL DEFAULT 0,
    deleted boolean NOT NULL DEFAULT false,bucket smallint NOT NULL,
    bucket_generation bigint NOT NULL DEFAULT 0,
    PRIMARY KEY(kind,entity_id)
);
CREATE INDEX cache_quota_changes ON cache_entity_versions(bucket,bucket_generation,entity_id) WHERE kind='quota_rank';
DO $$ DECLARE kind text; BEGIN
 FOREACH kind IN ARRAY ARRAY['nodes','users','credentials','authorization_groups','dns_connections','dns_zones','dns_records','config_resources','credential_batches','jobs','subscription_templates'] LOOP
  EXECUTE format('INSERT INTO cache_entity_versions(kind,entity_id,bucket) SELECT %L,id,hysteriax_cache_bucket(id) FROM %I',kind,kind);
 END LOOP;
END $$;
UPDATE cache_entity_versions v SET generation=j.generation FROM job_cache_versions j WHERE v.kind='jobs' AND v.entity_id=j.job_id;
UPDATE cache_entity_versions v SET generation=b.generation FROM batch_cache_versions b WHERE v.kind='credential_batches' AND v.entity_id=b.batch_id;
INSERT INTO cache_entity_versions(kind,entity_id,bucket,deleted)
SELECT 'quota_rank',n.id,hysteriax_cache_bucket(n.id),NOT EXISTS(
 SELECT 1 FROM node_packages p WHERE p.node_id=n.id AND (p.config->>'quota_bytes')::bigint>0
) FROM nodes n;

-- Pending invalidations belong to the writing transaction and disappear at
-- commit. The flush row has a unique transaction ID, never a shared domain key.
CREATE TABLE cache_bucket_pending(
    transaction_id bigint NOT NULL,domain text NOT NULL,bucket smallint NOT NULL,
    PRIMARY KEY(transaction_id,domain,bucket),
    FOREIGN KEY(domain,bucket) REFERENCES cache_bucket_versions(domain,bucket)
);
CREATE TABLE cache_entity_pending(
    transaction_id bigint NOT NULL,kind text NOT NULL,entity_id text NOT NULL,
    PRIMARY KEY(transaction_id,kind,entity_id)
);
CREATE TABLE cache_flush_pending(transaction_id bigint PRIMARY KEY);
CREATE FUNCTION hysteriax_cache_queue_flush() RETURNS void LANGUAGE plpgsql AS $$
DECLARE transaction_id bigint:=txid_current();
BEGIN
 IF current_setting('hysteriax.cache_flush_transaction',true) IS DISTINCT FROM transaction_id::text THEN
  PERFORM set_config('hysteriax.cache_flush_transaction',transaction_id::text,true);
  INSERT INTO cache_flush_pending VALUES(transaction_id) ON CONFLICT DO NOTHING;
 END IF;
END $$;
CREATE FUNCTION hysteriax_cache_touch_domain(domain_name text,entity_id text) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
 IF entity_id IS NULL THEN RETURN; END IF;
 INSERT INTO cache_bucket_pending VALUES(txid_current(),domain_name,hysteriax_cache_bucket(entity_id)) ON CONFLICT DO NOTHING;
 PERFORM hysteriax_cache_queue_flush();
END $$;
CREATE FUNCTION hysteriax_cache_touch_entity(entity_kind text,id text) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
 IF id IS NULL THEN RETURN; END IF;
 IF entity_kind<>ALL(ARRAY['nodes','users','credentials','authorization_groups','dns_connections','dns_zones','dns_records','config_resources','credential_batches','jobs','subscription_templates','quota_rank']) THEN
  RAISE EXCEPTION 'invalid cache entity kind';
 END IF;
 INSERT INTO cache_entity_pending VALUES(txid_current(),entity_kind,id) ON CONFLICT DO NOTHING;
 PERFORM hysteriax_cache_queue_flush();
END $$;
CREATE FUNCTION hysteriax_cache_flush() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE pending record; present boolean; entity_bucket smallint; bucket_revision bigint; entity_revision bigint;
BEGIN
 -- Every transaction takes version rows in this one fixed order, once per key.
 FOR pending IN SELECT domain,bucket FROM cache_bucket_pending WHERE transaction_id=NEW.transaction_id ORDER BY domain,bucket LOOP
  UPDATE cache_bucket_versions SET generation=generation+1 WHERE domain=pending.domain AND bucket=pending.bucket;
 END LOOP;
 FOR pending IN SELECT kind,entity_id FROM cache_entity_pending WHERE transaction_id=NEW.transaction_id ORDER BY kind,entity_id LOOP
  entity_bucket:=hysteriax_cache_bucket(pending.entity_id);
  bucket_revision:=0;
  IF pending.kind='quota_rank' THEN
   SELECT EXISTS(SELECT 1 FROM nodes n JOIN node_packages p ON p.node_id=n.id WHERE n.id=pending.entity_id AND (p.config->>'quota_bytes')::bigint>0) INTO present;
   SELECT generation INTO bucket_revision FROM cache_bucket_versions WHERE domain='quota_rank' AND bucket=entity_bucket;
  ELSE
   EXECUTE format('SELECT EXISTS(SELECT 1 FROM %I WHERE id=$1)',pending.kind) INTO present USING pending.entity_id;
  END IF;
  INSERT INTO cache_entity_versions(kind,entity_id,generation,deleted,bucket,bucket_generation)
  VALUES(pending.kind,pending.entity_id,1,NOT present,entity_bucket,bucket_revision)
  ON CONFLICT(kind,entity_id) DO UPDATE SET generation=cache_entity_versions.generation+1,
    deleted=EXCLUDED.deleted,bucket_generation=EXCLUDED.bucket_generation
  RETURNING generation INTO entity_revision;
  IF pending.kind='quota_rank' THEN
   PERFORM pg_notify('hysteriax_quota_rank',json_build_object('schema',current_schema(),'id',pending.entity_id,'version',entity_revision)::text);
  END IF;
 END LOOP;
 DELETE FROM cache_bucket_pending WHERE transaction_id=NEW.transaction_id;
 DELETE FROM cache_entity_pending WHERE transaction_id=NEW.transaction_id;
 DELETE FROM cache_flush_pending WHERE transaction_id=NEW.transaction_id;
 PERFORM set_config('hysteriax.cache_flush_transaction','',true);
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER cache_commit_versions AFTER INSERT ON cache_flush_pending
DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_flush();

-- Preserve every existing domain assignment, including mTLS follow-up tables.
CREATE TEMP TABLE cache_source_domains ON COMMIT DROP AS
SELECT c.relname AS source,
 max(CASE WHEN p.proname='hysteriax_domain_changed' THEN split_part(encode(t.tgargs,'escape'),chr(92)||'000',1) END) AS domains,
 bool_or(p.proname IN ('hysteriax_configuration_changed','hysteriax_directory_row_changed','hysteriax_admin_cache_changed')) AS directory
FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid
WHERE NOT t.tgisinternal AND c.relnamespace=current_schema()::regnamespace
 AND p.proname=ANY(ARRAY['hysteriax_cache_changed','hysteriax_configuration_changed','hysteriax_directory_row_changed','hysteriax_admin_cache_changed','hysteriax_domain_changed','hysteriax_domain_row_changed','hysteriax_job_cache_changed','hysteriax_batch_cache_changed','hysteriax_quota_rank_changed'])
GROUP BY c.relname;
DO $$ DECLARE old record; BEGIN
 FOR old IN SELECT c.relname,t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_proc p ON p.oid=t.tgfoid
 WHERE NOT t.tgisinternal AND c.relnamespace=current_schema()::regnamespace
 AND p.proname=ANY(ARRAY['hysteriax_cache_changed','hysteriax_configuration_changed','hysteriax_directory_row_changed','hysteriax_admin_cache_changed','hysteriax_domain_changed','hysteriax_domain_row_changed','hysteriax_job_cache_changed','hysteriax_batch_cache_changed','hysteriax_quota_rank_changed']) LOOP
  EXECUTE format('DROP TRIGGER %I ON %I',old.tgname,old.relname);
 END LOOP;
END $$;

CREATE FUNCTION hysteriax_cache_row_changed() RETURNS trigger LANGUAGE plpgsql AS $$
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
CREATE FUNCTION hysteriax_cache_truncated() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source record; root record; domain_name text;
BEGIN
 FOR source IN SELECT domain,bucket FROM cache_bucket_versions ORDER BY domain,bucket LOOP
  INSERT INTO cache_bucket_pending VALUES(txid_current(),source.domain,source.bucket) ON CONFLICT DO NOTHING;
 END LOOP;
 FOR root IN SELECT kind,entity_id FROM cache_entity_versions ORDER BY kind,entity_id LOOP
  PERFORM hysteriax_cache_touch_entity(root.kind,root.entity_id);
 END LOOP;
 PERFORM hysteriax_cache_queue_flush();
 RETURN NULL;
END $$;
DO $$ DECLARE source record; BEGIN
 FOR source IN SELECT * FROM cache_source_domains LOOP
  EXECUTE format('CREATE TRIGGER cache_row_changed AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed(%L,%L)',source.source,coalesce(source.domains,''),source.directory::text);
  EXECUTE format('CREATE TRIGGER cache_truncated AFTER TRUNCATE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated()',source.source);
 END LOOP;
END $$;
DROP FUNCTION hysteriax_cache_changed(),hysteriax_configuration_changed(),hysteriax_directory_row_changed(),hysteriax_admin_cache_changed(),hysteriax_domain_changed(),hysteriax_domain_row_changed(),hysteriax_job_cache_changed(),hysteriax_batch_cache_changed(),hysteriax_quota_rank_changed();
DROP TABLE cache_generation,cache_domain_versions,job_cache_versions,batch_cache_versions;
CREATE VIEW job_cache_versions AS SELECT entity_id AS job_id,generation FROM cache_entity_versions WHERE kind='jobs';
CREATE VIEW batch_cache_versions AS SELECT entity_id AS batch_id,generation FROM cache_entity_versions WHERE kind='credential_batches';
