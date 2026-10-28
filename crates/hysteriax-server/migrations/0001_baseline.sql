-- Fresh-install baseline, consolidated from the final 0001–0074 SQL schema.
-- Existing migration histories are intentionally incompatible. Use an empty DB.
-- Keep definitions schema-relative so isolated PostgreSQL test schemas work.
-- Rust maintenance initialization still runs separately via --migrate-only.
-- Some SQL functions reference tables defined later in dependency order.
SET LOCAL check_function_bodies = false;

CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;

-- TABLE: jobs

CREATE TABLE jobs (
    id text NOT NULL,
    kind text NOT NULL,
    node_id text,
    target_revision bigint,
    payload_json jsonb DEFAULT '{}'::jsonb NOT NULL,
    status text NOT NULL,
    stage text NOT NULL,
    result_json jsonb,
    error_message text,
    logs_json jsonb DEFAULT '[]'::jsonb NOT NULL,
    attempts bigint DEFAULT 0 NOT NULL,
    max_attempts bigint DEFAULT 3 NOT NULL,
    available_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    started_at timestamp with time zone,
    finished_at timestamp with time zone,
    node_name text,
    retry_of_job_id text,
    resource_key text,
    resource_type text,
    resource_id text,
    resource_name text,
    CONSTRAINT jobs_logs_json_check CHECK ((jsonb_typeof(logs_json) = 'array'::text)),
    CONSTRAINT jobs_payload_json_check CHECK ((jsonb_typeof(payload_json) = 'object'::text)),
    CONSTRAINT jobs_result_json_check CHECK (((result_json IS NULL) OR (jsonb_typeof(result_json) = 'object'::text))),
    CONSTRAINT jobs_status_check CHECK ((status = ANY (ARRAY['queued'::text, 'running'::text, 'waiting_confirmation'::text, 'succeeded'::text, 'failed'::text, 'rolled_back'::text, 'cancelled'::text])))
);

-- FUNCTION: hysteriax_adjust_job(jobs, bigint)

CREATE FUNCTION hysteriax_adjust_job(j jobs, sign bigint) RETURNS void
    LANGUAGE plpgsql
    AS $$
DECLARE p bigint;pl bigint;f bigint;
BEGIN
 IF sign>0 THEN INSERT INTO job_status_totals VALUES(j.status,1) ON CONFLICT(status) DO UPDATE SET total=job_status_totals.total+1;
 ELSE UPDATE job_status_totals SET total=total-1 WHERE status=j.status; DELETE FROM job_status_totals WHERE status=j.status AND total=0; END IF;
 IF j.kind='kick' AND j.node_id IS NOT NULL THEN
  p:=(j.status IN ('queued','running'))::int;pl:=p*coalesce((j.payload_json->>'node_limit'='true')::int,0);f:=coalesce((j.status='failed' AND j.payload_json->>'node_limit'='true')::int,0);
  IF sign>0 THEN INSERT INTO job_node_kick_totals VALUES(j.node_id,p,pl,f) ON CONFLICT(node_id) DO UPDATE SET pending=job_node_kick_totals.pending+EXCLUDED.pending,pending_limit=job_node_kick_totals.pending_limit+EXCLUDED.pending_limit,failed_limit=job_node_kick_totals.failed_limit+EXCLUDED.failed_limit;
  ELSE UPDATE job_node_kick_totals SET pending=pending-p,pending_limit=pending_limit-pl,failed_limit=failed_limit-f WHERE node_id=j.node_id; END IF;
 END IF;
 IF j.status='failed' AND j.finished_at IS NOT NULL THEN
  IF sign>0 THEN INSERT INTO job_failed_minute_totals VALUES(hysteriax_minute(j.finished_at),1) ON CONFLICT(minute) DO UPDATE SET total=job_failed_minute_totals.total+1;
  ELSE UPDATE job_failed_minute_totals SET total=total-1 WHERE minute=hysteriax_minute(j.finished_at);DELETE FROM job_failed_minute_totals WHERE minute=hysteriax_minute(j.finished_at) AND total=0; END IF;
 END IF;
END $$;

-- FUNCTION: hysteriax_audit_deleted()

CREATE FUNCTION hysteriax_audit_deleted() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN UPDATE audit_totals SET total=total-(SELECT count(*) FROM removed); RETURN NULL; END $$;

-- FUNCTION: hysteriax_audit_inserted()

CREATE FUNCTION hysteriax_audit_inserted() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN UPDATE audit_totals SET total=total+(SELECT count(*) FROM added); RETURN NULL; END $$;

-- FUNCTION: hysteriax_audit_truncated()

CREATE FUNCTION hysteriax_audit_truncated() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN UPDATE audit_totals SET total=0; RETURN NULL; END $$;

-- FUNCTION: hysteriax_cache_bucket(text)

CREATE FUNCTION hysteriax_cache_bucket(entity_id text) RETURNS smallint
    LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
    AS $$
 SELECT (get_byte(decode(md5(entity_id),'hex'),0)%64)::smallint
$$;

-- FUNCTION: hysteriax_cache_flush()

CREATE FUNCTION hysteriax_cache_flush() RETURNS trigger
    LANGUAGE plpgsql
    AS $_$
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
END $_$;

-- FUNCTION: hysteriax_cache_queue_flush()

CREATE FUNCTION hysteriax_cache_queue_flush() RETURNS void
    LANGUAGE plpgsql
    AS $$
DECLARE transaction_id bigint:=txid_current();
BEGIN
 IF current_setting('hysteriax.cache_flush_transaction',true) IS DISTINCT FROM transaction_id::text THEN
  PERFORM set_config('hysteriax.cache_flush_transaction',transaction_id::text,true);
  INSERT INTO cache_flush_pending VALUES(transaction_id) ON CONFLICT DO NOTHING;
 END IF;
END $$;

-- FUNCTION: hysteriax_cache_row_changed()

CREATE FUNCTION hysteriax_cache_row_changed() RETURNS trigger
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

-- FUNCTION: hysteriax_cache_touch_domain(text, text)

CREATE FUNCTION hysteriax_cache_touch_domain(domain_name text, entity_id text) RETURNS void
    LANGUAGE plpgsql
    AS $$
BEGIN
 IF entity_id IS NULL THEN RETURN; END IF;
 INSERT INTO cache_bucket_pending VALUES(txid_current(),domain_name,hysteriax_cache_bucket(entity_id)) ON CONFLICT DO NOTHING;
 PERFORM hysteriax_cache_queue_flush();
END $$;

-- FUNCTION: hysteriax_cache_touch_entity(text, text)

CREATE FUNCTION hysteriax_cache_touch_entity(entity_kind text, id text) RETURNS void
    LANGUAGE plpgsql
    AS $$
BEGIN
 IF id IS NULL THEN RETURN; END IF;
 IF entity_kind<>ALL(ARRAY['nodes','users','credentials','authorization_groups','dns_connections','dns_zones','dns_records','config_resources','credential_batches','jobs','subscription_templates','quota_rank']) THEN
  RAISE EXCEPTION 'invalid cache entity kind';
 END IF;
 INSERT INTO cache_entity_pending VALUES(txid_current(),entity_kind,id) ON CONFLICT DO NOTHING;
 PERFORM hysteriax_cache_queue_flush();
END $$;

-- FUNCTION: hysteriax_cache_truncated()

CREATE FUNCTION hysteriax_cache_truncated() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
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

-- FUNCTION: hysteriax_cycle_dirty()

CREATE FUNCTION hysteriax_cycle_dirty() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE old_id uuid; new_id uuid; item uuid;
BEGIN
 IF TG_OP<>'INSERT' THEN old_id:=OLD.cycle_id; END IF;
 IF TG_OP<>'DELETE' THEN new_id:=NEW.cycle_id; END IF;
 FOR item IN SELECT DISTINCT id FROM unnest(ARRAY[old_id,new_id]) id WHERE id IS NOT NULL ORDER BY id LOOP
  PERFORM hysteriax_mark_dirty('analytics_dirty_cycles',jsonb_build_object('cycle_id',item));
 END LOOP;
 RETURN NULL;
END $$;

-- FUNCTION: hysteriax_directory_credential_status(boolean, timestamp with time zone)

CREATE FUNCTION hysteriax_directory_credential_status(archived boolean, expires timestamp with time zone) RETURNS text
    LANGUAGE sql STABLE
    AS $$ SELECT CASE WHEN archived THEN 'archived' WHEN floor(trunc(extract(epoch FROM expires-now()))/86400)<0 THEN 'expired' WHEN floor(trunc(extract(epoch FROM expires-now()))/86400)<=30 THEN 'expiring' ELSE 'active' END $$;

-- FUNCTION: hysteriax_directory_user_status(boolean, timestamp with time zone, bigint, bigint)

CREATE FUNCTION hysteriax_directory_user_status(enabled boolean, expires timestamp with time zone, quota bigint, usage bigint) RETURNS text
    LANGUAGE sql STABLE
    AS $$ SELECT CASE WHEN NOT enabled THEN 'disabled' WHEN expires<=now() THEN 'expired' WHEN usage>=quota THEN 'quota_exhausted' ELSE 'active' END $$;

-- FUNCTION: hysteriax_event_pending()

CREATE FUNCTION hysteriax_event_pending() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    INSERT INTO event_publication_pending(event_id) VALUES(NEW.id);
    RETURN NEW;
END $$;

-- FUNCTION: hysteriax_execution_epoch()

CREATE FUNCTION hysteriax_execution_epoch() RETURNS text
    LANGUAGE sql STABLE
    AS $$ SELECT epoch FROM cache_version_epoch WHERE id $$;

-- FUNCTION: hysteriax_flow_adjust(text, text, timestamp with time zone, bigint, bigint, bigint, bigint)

CREATE FUNCTION hysteriax_flow_adjust(s text, n text, at_time timestamp with time zone, tx_delta bigint, rx_delta bigint, bad bigint, sign bigint) RETURNS void
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF sign>0 THEN
        INSERT INTO traffic_minute_totals VALUES(s,n,hysteriax_minute(at_time),tx_delta,rx_delta,1,bad)
        ON CONFLICT(source,node_id,minute) DO UPDATE SET tx=traffic_minute_totals.tx+EXCLUDED.tx,
            rx=traffic_minute_totals.rx+EXCLUDED.rx,samples=traffic_minute_totals.samples+1,
            incomplete_samples=traffic_minute_totals.incomplete_samples+EXCLUDED.incomplete_samples;
    ELSE
        UPDATE traffic_minute_totals SET tx=tx-tx_delta,rx=rx-rx_delta,samples=samples-1,incomplete_samples=incomplete_samples-bad
        WHERE source=s AND node_id=n AND minute=hysteriax_minute(at_time);
        DELETE FROM traffic_minute_totals WHERE source=s AND node_id=n AND minute=hysteriax_minute(at_time) AND samples=0;
    END IF;
END $$;

-- FUNCTION: hysteriax_flow_totals()

CREATE FUNCTION hysteriax_flow_totals() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE s text;
BEGIN
    -- A deleted node's rollups have their own ON DELETE CASCADE. Updating them
    -- per sample only repeats work while holding the global writer lock.
    IF TG_OP='DELETE' AND (
        current_setting('hysteriax.history_cleanup_node',true)=OLD.node_id
        OR NOT EXISTS(SELECT 1 FROM nodes WHERE id=OLD.node_id)
    ) THEN
        RETURN NULL;
    END IF;
    s:=CASE WHEN TG_TABLE_NAME='traffic_records' THEN 'users' ELSE 'network' END;
    IF TG_OP<>'INSERT' THEN
        PERFORM hysteriax_flow_adjust(s,OLD.node_id,OLD.sampled_at,OLD.delta_tx,OLD.delta_rx,(OLD.gap_reason IS NOT NULL OR OLD.baseline_only)::integer,-1);
        IF s='users' THEN
            UPDATE user_node_usage SET tx=tx-OLD.delta_tx,rx=rx-OLD.delta_rx,samples=samples-1 WHERE user_id=OLD.user_id AND node_id=OLD.node_id;
            DELETE FROM user_node_usage WHERE user_id=OLD.user_id AND node_id=OLD.node_id AND samples=0;
            UPDATE user_node_usage SET sampled_at=coalesce((SELECT max(sampled_at) FROM traffic_records WHERE user_id=OLD.user_id AND node_id=OLD.node_id),sampled_at)
            WHERE user_id=OLD.user_id AND node_id=OLD.node_id AND sampled_at=OLD.sampled_at;
        END IF;
    END IF;
    IF TG_OP<>'DELETE' THEN
        PERFORM hysteriax_flow_adjust(s,NEW.node_id,NEW.sampled_at,NEW.delta_tx,NEW.delta_rx,(NEW.gap_reason IS NOT NULL OR NEW.baseline_only)::integer,1);
        IF s='users' THEN
            INSERT INTO user_node_usage VALUES(NEW.user_id,NEW.node_id,NEW.delta_tx,NEW.delta_rx,1,NEW.sampled_at)
            ON CONFLICT(user_id,node_id) DO UPDATE SET tx=user_node_usage.tx+EXCLUDED.tx,rx=user_node_usage.rx+EXCLUDED.rx,
                samples=user_node_usage.samples+1,sampled_at=greatest(user_node_usage.sampled_at,EXCLUDED.sampled_at);
        END IF;
    END IF;
    RETURN NULL;
END $$;

-- FUNCTION: hysteriax_hour(timestamp with time zone)

CREATE FUNCTION hysteriax_hour(v timestamp with time zone) RETURNS timestamp with time zone
    LANGUAGE sql IMMUTABLE
    AS $$ SELECT date_bin(interval '1 hour',v,'2000-01-01 00:00:00+00'::timestamptz) $$;

-- FUNCTION: hysteriax_hour_dirty()

CREATE FUNCTION hysteriax_hour_dirty() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE a_source text; a_node text; a_hour timestamptz; b_source text; b_node text; b_hour timestamptz; item record;
BEGIN
 IF TG_OP<>'INSERT' THEN a_source:=OLD.source; a_node:=OLD.node_id; a_hour:=hysteriax_hour(OLD.minute); END IF;
 IF TG_OP<>'DELETE' THEN b_source:=NEW.source; b_node:=NEW.node_id; b_hour:=hysteriax_hour(NEW.minute); END IF;
 FOR item IN SELECT DISTINCT source,node_id,hour FROM (VALUES(a_source,a_node,a_hour),(b_source,b_node,b_hour)) keys(source,node_id,hour) WHERE source IS NOT NULL ORDER BY source,node_id,hour LOOP
  PERFORM hysteriax_mark_dirty('traffic_dirty_hours',jsonb_build_object('source',item.source,'node_id',item.node_id,'hour',extract(epoch FROM item.hour)));
 END LOOP;
 RETURN NULL;
END $$;

-- FUNCTION: hysteriax_immutable_credential_version()

CREATE FUNCTION hysteriax_immutable_credential_version() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN RAISE EXCEPTION 'credential versions are immutable'; END $$;

-- FUNCTION: hysteriax_job_totals_changed()

CREATE FUNCTION hysteriax_job_totals_changed() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
 IF TG_OP='UPDATE' AND (OLD.status,OLD.kind,OLD.node_id,OLD.payload_json->>'node_limit',OLD.finished_at) IS NOT DISTINCT FROM (NEW.status,NEW.kind,NEW.node_id,NEW.payload_json->>'node_limit',NEW.finished_at) THEN RETURN NULL; END IF;
 IF TG_OP<>'INSERT' THEN PERFORM hysteriax_adjust_job(OLD,-1);END IF;
 IF TG_OP<>'DELETE' THEN PERFORM hysteriax_adjust_job(NEW,1);END IF;
 RETURN NULL;
END $$;

-- FUNCTION: hysteriax_management_receipt_complete()

CREATE FUNCTION hysteriax_management_receipt_complete() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    -- Read the final row, not the INSERT transition tuple with its placeholder.
    IF EXISTS (SELECT 1 FROM management_receipts
        WHERE actor_id=NEW.actor_id AND operation=NEW.operation
          AND idempotency_key=NEW.idempotency_key AND response_enc='') THEN
        RAISE EXCEPTION 'management receipt is incomplete' USING ERRCODE='23514';
    END IF;
    RETURN NULL;
END $$;

-- FUNCTION: hysteriax_mark_dirty(text, jsonb)

CREATE FUNCTION hysteriax_mark_dirty(queue_name text, keys jsonb) RETURNS void
    LANGUAGE plpgsql
    AS $$
DECLARE revision bigint;
BEGIN
 IF queue_name<>ALL(ARRAY['analytics_dirty_cycles','traffic_dirty_hours','online_dirty_minutes','job_retry_state_dirty','credential_reference_dirty']) THEN RAISE EXCEPTION 'invalid dirty queue'; END IF;
 INSERT INTO dirty_key_versions(queue,entity_key,generation) VALUES(queue_name,keys::text,1)
 ON CONFLICT(queue,entity_key) DO UPDATE SET generation=dirty_key_versions.generation+1 RETURNING generation INTO revision;
 CASE queue_name
 WHEN 'analytics_dirty_cycles' THEN
  INSERT INTO analytics_dirty_cycles(cycle_id,generation) VALUES((keys->>'cycle_id')::uuid,revision) ON CONFLICT(cycle_id) DO UPDATE SET generation=EXCLUDED.generation;
 WHEN 'traffic_dirty_hours' THEN
  INSERT INTO traffic_dirty_hours(source,node_id,hour,generation) VALUES(keys->>'source',keys->>'node_id',to_timestamp((keys->>'hour')::double precision),revision) ON CONFLICT(source,node_id,hour) DO UPDATE SET generation=EXCLUDED.generation;
 WHEN 'online_dirty_minutes' THEN
  INSERT INTO online_dirty_minutes(scope_node_id,minute,generation) VALUES(keys->>'scope_node_id',to_timestamp((keys->>'minute')::double precision),revision) ON CONFLICT(scope_node_id,minute) DO UPDATE SET generation=EXCLUDED.generation;
 WHEN 'job_retry_state_dirty' THEN
  INSERT INTO job_retry_state_dirty(job_id,generation) VALUES(keys->>'job_id',revision) ON CONFLICT(job_id) DO UPDATE SET generation=EXCLUDED.generation;
 WHEN 'credential_reference_dirty' THEN
  INSERT INTO credential_reference_dirty(kind,source_id,generation) VALUES(keys->>'kind',keys->>'source_id',revision) ON CONFLICT(kind,source_id) DO UPDATE SET generation=EXCLUDED.generation;
 END CASE;
END $$;

-- FUNCTION: hysteriax_minute(timestamp with time zone)

CREATE FUNCTION hysteriax_minute(v timestamp with time zone) RETURNS timestamp with time zone
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT date_bin(interval '1 minute',v,'2000-01-01 00:00:00+00'::timestamptz)
$$;

-- FUNCTION: hysteriax_mtls_job_pending()

CREATE FUNCTION hysteriax_mtls_job_pending() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
 UPDATE mtls_node_operations SET pending_job=NEW.status IN ('queued','waiting_confirmation')
 WHERE job_id=NEW.id AND pending_job IS DISTINCT FROM (NEW.status IN ('queued','waiting_confirmation'));
 RETURN NEW;
END $$;

-- FUNCTION: hysteriax_mtls_operation_pending_job()

CREATE FUNCTION hysteriax_mtls_operation_pending_job() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
 SELECT status IN ('queued','waiting_confirmation') INTO NEW.pending_job FROM jobs WHERE id=NEW.job_id;
 RETURN NEW;
END $$;

-- FUNCTION: hysteriax_node_authorization_changed()

CREATE FUNCTION hysteriax_node_authorization_changed() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE old_node text; new_node text; target text;
BEGIN
    IF TG_OP='UPDATE' AND (OLD.user_id,OLD.node_id) IS NOT DISTINCT FROM (NEW.user_id,NEW.node_id) THEN
        RETURN NULL;
    END IF;
    IF TG_OP<>'INSERT' THEN old_node:=OLD.node_id; END IF;
    IF TG_OP<>'DELETE' THEN new_node:=NEW.node_id; END IF;
    FOR target IN SELECT DISTINCT id FROM unnest(ARRAY[old_node,new_node]) id WHERE id IS NOT NULL ORDER BY id LOOP
        -- A cascading node deletion can already have removed its version row.
        UPDATE node_authorization_versions SET generation=generation+1 WHERE node_id=target;
    END LOOP;
    RETURN NULL;
END $$;

-- FUNCTION: hysteriax_node_authorization_created()

CREATE FUNCTION hysteriax_node_authorization_created() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    INSERT INTO node_authorization_versions(node_id) VALUES(NEW.id);
    RETURN NULL;
END $$;

-- FUNCTION: hysteriax_node_authorizations_truncated()

CREATE FUNCTION hysteriax_node_authorizations_truncated() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE target text;
BEGIN
    FOR target IN SELECT node_id FROM node_authorization_versions ORDER BY node_id LOOP
        UPDATE node_authorization_versions SET generation=generation+1 WHERE node_id=target;
    END LOOP;
    RETURN NULL;
END $$;

-- FUNCTION: hysteriax_online_cycle_values(uuid[])

CREATE FUNCTION hysteriax_online_cycle_values(ids uuid[]) RETURNS TABLE(cycle_id uuid, scope_node_id text, started_at timestamp with time zone, users bigint, connections numeric, covered bigint, expected bigint, incomplete boolean, traffic_covered bigint, traffic_incomplete boolean)
    LANGUAGE sql STABLE
    AS $$
WITH samples AS MATERIALIZED (
    SELECT * FROM online_samples WHERE cycle_id=ANY(ids)
), global_base AS (
    SELECT cycle_id,min(cycle_started_at) started_at,
        count(*) FILTER(WHERE status='ok') covered,max(expected_nodes) expected,
        bool_or(status!='ok') OR count(*) FILTER(WHERE status='ok')<max(expected_nodes) incomplete,
        count(*) FILTER(WHERE traffic_status='ok') traffic_covered,
        count(*) FILTER(WHERE traffic_status='ok')<max(expected_nodes) traffic_incomplete
    FROM samples GROUP BY cycle_id
), members AS MATERIALIZED (
    SELECT s.cycle_id,s.node_id,e.key,e.value::numeric connections
    FROM samples s LEFT JOIN LATERAL jsonb_each_text(s.connections) e ON true WHERE s.status='ok'
), global_counts AS (
    SELECT cycle_id,count(DISTINCT key) users,coalesce(sum(connections),0) connections FROM members GROUP BY cycle_id
), node_counts AS (
    SELECT cycle_id,node_id,count(DISTINCT key) users,coalesce(sum(connections),0) connections FROM members GROUP BY cycle_id,node_id
)
SELECT b.cycle_id,'',b.started_at,c.users,c.connections,b.covered,b.expected,b.incomplete,b.traffic_covered,b.traffic_incomplete
FROM global_base b LEFT JOIN global_counts c USING(cycle_id)
UNION ALL
SELECT s.cycle_id,s.node_id,s.cycle_started_at,c.users,c.connections,
    (s.status='ok')::integer,1,s.status!='ok',(s.traffic_status='ok')::integer,s.traffic_status!='ok'
FROM samples s LEFT JOIN node_counts c USING(cycle_id,node_id)
WHERE true

$$;

-- FUNCTION: hysteriax_online_minute_dirty()

CREATE FUNCTION hysteriax_online_minute_dirty() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE a_node text; a_minute timestamptz; b_node text; b_minute timestamptz; item record;
BEGIN
 IF TG_OP<>'INSERT' THEN a_node:=OLD.scope_node_id; a_minute:=hysteriax_minute(OLD.started_at); END IF;
 IF TG_OP<>'DELETE' THEN b_node:=NEW.scope_node_id; b_minute:=hysteriax_minute(NEW.started_at); END IF;
 FOR item IN SELECT DISTINCT scope_node_id,minute FROM (VALUES(a_node,a_minute),(b_node,b_minute)) keys(scope_node_id,minute) WHERE scope_node_id IS NOT NULL ORDER BY scope_node_id,minute LOOP
  PERFORM hysteriax_mark_dirty('online_dirty_minutes',jsonb_build_object('scope_node_id',item.scope_node_id,'minute',extract(epoch FROM item.minute)));
 END LOOP;
 RETURN NULL;
END $$;

-- FUNCTION: hysteriax_probe_totals()

CREATE FUNCTION hysteriax_probe_totals() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    -- A deleted node's rollups have their own ON DELETE CASCADE. Updating them
    -- per sample only repeats work while holding the global writer lock.
    IF TG_OP='DELETE' AND (
        current_setting('hysteriax.history_cleanup_node',true)=OLD.node_id
        OR NOT EXISTS(SELECT 1 FROM nodes WHERE id=OLD.node_id)
    ) THEN
        RETURN NULL;
    END IF;
    IF TG_OP<>'INSERT' AND OLD.status IN ('ok','failed') THEN
        UPDATE probe_minute_totals SET attempts=attempts-1,
            successes=successes-(OLD.status='ok' AND OLD.external_status IS DISTINCT FROM 'failed')::integer
        WHERE node_id=OLD.node_id AND minute=hysteriax_minute(OLD.sampled_at);
        DELETE FROM probe_minute_totals WHERE node_id=OLD.node_id AND minute=hysteriax_minute(OLD.sampled_at) AND attempts=0;
    END IF;
    IF TG_OP<>'DELETE' AND NEW.status IN ('ok','failed') THEN
        INSERT INTO probe_minute_totals VALUES(NEW.node_id,hysteriax_minute(NEW.sampled_at),1,(NEW.status='ok' AND NEW.external_status IS DISTINCT FROM 'failed')::integer)
        ON CONFLICT(node_id,minute) DO UPDATE SET attempts=probe_minute_totals.attempts+1,successes=probe_minute_totals.successes+EXCLUDED.successes;
    END IF;
    RETURN NULL;
END $$;

-- FUNCTION: hysteriax_reference_dirty()

CREATE FUNCTION hysteriax_reference_dirty() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE source_kind text; ids text[]:=ARRAY[]::text[]; item text;
BEGIN
 source_kind:=CASE WHEN TG_TABLE_NAME='nodes' THEN 'node' ELSE 'history' END;
 IF TG_OP='DELETE' THEN
  DELETE FROM credential_reference_edges WHERE kind=source_kind AND source_id=OLD.id;
  DELETE FROM credential_reference_dirty WHERE kind=source_kind AND source_id=OLD.id;
  RETURN NULL;
 END IF;
 IF TG_TABLE_NAME='config_versions' THEN
  IF TG_OP='UPDATE' THEN
   IF (OLD.id,OLD.node_id,OLD.revision,OLD.config_enc) IS NOT DISTINCT FROM (NEW.id,NEW.node_id,NEW.revision,NEW.config_enc) THEN RETURN NULL; END IF;
  END IF;
 END IF;
 IF TG_OP='UPDATE' AND OLD.id<>NEW.id THEN ids:=array_append(ids,OLD.id); END IF;
 ids:=array_append(ids,NEW.id);
 FOR item IN SELECT DISTINCT id FROM unnest(ids) id ORDER BY id LOOP
  PERFORM hysteriax_mark_dirty('credential_reference_dirty',jsonb_build_object('kind',source_kind,'source_id',item));
 END LOOP;
 RETURN NULL;
END $$;

-- FUNCTION: hysteriax_refresh_user_access_check(text, boolean, timestamp with time zone, bigint, bigint, timestamp with time zone)

CREATE FUNCTION hysteriax_refresh_user_access_check(subject text, active boolean, expiry timestamp with time zone, quota bigint, usage bigint, marker timestamp with time zone) RETURNS void
    LANGUAGE plpgsql
    AS $$
DECLARE due TIMESTAMPTZ;
BEGIN
    IF marker IS NOT NULL THEN
        DELETE FROM user_access_checks WHERE user_id=subject;
        RETURN;
    END IF;
    due := CASE WHEN NOT active OR usage>=quota THEN clock_timestamp() ELSE expiry END;
    IF due IS NULL THEN
        DELETE FROM user_access_checks WHERE user_id=subject;
    ELSE
        INSERT INTO user_access_checks(user_id,next_action_at) VALUES(subject,due)
        ON CONFLICT(user_id) DO UPDATE SET next_action_at=excluded.next_action_at
        WHERE user_access_checks.next_action_at IS DISTINCT FROM excluded.next_action_at;
    END IF;
END $$;

-- FUNCTION: hysteriax_require_transactional_references()

CREATE FUNCTION hysteriax_require_transactional_references() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE pending boolean; source_exists boolean;
BEGIN
 IF current_setting('hysteriax.reference_maintenance',true)='on' THEN RETURN NULL; END IF;
 SELECT EXISTS(SELECT 1 FROM credential_reference_dirty d WHERE d.kind=NEW.kind AND d.source_id=NEW.source_id AND d.generation>=NEW.generation) INTO pending;
 IF NOT pending THEN RETURN NULL; END IF;
 IF NEW.kind='node' THEN SELECT EXISTS(SELECT 1 FROM nodes WHERE id=NEW.source_id) INTO source_exists;
 ELSE SELECT EXISTS(SELECT 1 FROM config_versions WHERE id=NEW.source_id) INTO source_exists; END IF;
 IF source_exists THEN
  RAISE EXCEPTION 'configuration references must be indexed in the source transaction'
   USING ERRCODE='23514',CONSTRAINT='reference_source_transaction';
 END IF;
 RETURN NULL;
END $$;

-- FUNCTION: hysteriax_resource_source_deleted()

CREATE FUNCTION hysteriax_resource_source_deleted() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
 DELETE FROM config_resource_references WHERE kind=CASE WHEN TG_TABLE_NAME='nodes' THEN 'node' ELSE 'history' END AND source_id=OLD.id;
 RETURN NULL;
END $$;

-- FUNCTION: hysteriax_retry_state_changed()

CREATE FUNCTION hysteriax_retry_state_changed() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE ids text[]:=ARRAY[]::text[]; item text;
BEGIN
 IF TG_OP='UPDATE' AND jsonb_build_array(to_jsonb(OLD)->'retry_of_job_id',to_jsonb(OLD)->'status',to_jsonb(OLD)->'error_message',to_jsonb(OLD)->'finished_at',to_jsonb(OLD)->'node_name',to_jsonb(OLD)->'kind') = jsonb_build_array(to_jsonb(NEW)->'retry_of_job_id',to_jsonb(NEW)->'status',to_jsonb(NEW)->'error_message',to_jsonb(NEW)->'finished_at',to_jsonb(NEW)->'node_name',to_jsonb(NEW)->'kind') THEN RETURN NULL; END IF;
 IF TG_OP<>'INSERT' THEN ids:=ids||ARRAY[OLD.id,OLD.retry_of_job_id]; END IF;
 IF TG_OP<>'DELETE' THEN ids:=ids||ARRAY[NEW.id,NEW.retry_of_job_id]; END IF;
 FOR item IN SELECT DISTINCT id FROM unnest(ids) id WHERE id IS NOT NULL ORDER BY id LOOP
  PERFORM hysteriax_mark_dirty('job_retry_state_dirty',jsonb_build_object('job_id',item));
 END LOOP;
 RETURN NULL;
END $$;

-- FUNCTION: hysteriax_rfc3339(text)

CREATE FUNCTION hysteriax_rfc3339(value text) RETURNS timestamp with time zone
    LANGUAGE plpgsql STABLE STRICT
    AS $_$
BEGIN
 IF value !~ '^\d{4}-\d{2}-\d{2}[Tt]\d{2}:\d{2}:\d{2}(\.\d+)?([Zz]|[+-]\d{2}:\d{2})$' THEN RETURN NULL; END IF;
 RETURN value::timestamptz;
EXCEPTION WHEN invalid_datetime_format OR datetime_field_overflow THEN RETURN NULL;
END $_$;

-- FUNCTION: hysteriax_search_text(text[])

CREATE FUNCTION hysteriax_search_text(VARIADIC parts text[]) RETURNS text
    LANGUAGE sql IMMUTABLE PARALLEL SAFE
    AS $$ SELECT lower(array_to_string(parts,' ')) $$;

-- FUNCTION: hysteriax_track_access_restriction_epoch()

CREATE FUNCTION hysteriax_track_access_restriction_epoch() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN
    INSERT INTO user_access_restriction_epochs(user_id,generation) VALUES(NEW.id,1)
    ON CONFLICT(user_id) DO UPDATE SET generation=user_access_restriction_epochs.generation+1;
    RETURN NEW;
END $$;

-- FUNCTION: hysteriax_update_user_access_check()

CREATE FUNCTION hysteriax_update_user_access_check() RETURNS trigger
    LANGUAGE plpgsql
    AS $$ BEGIN
    PERFORM hysteriax_refresh_user_access_check(NEW.id,NEW.enabled,NEW.expires_at,NEW.quota_bytes,NEW.usage_bytes,NEW.access_kick_enqueued_at);
    RETURN NEW;
END $$;

-- TABLE: admin_tokens

CREATE TABLE admin_tokens (
    id text NOT NULL,
    token_hash text NOT NULL,
    label text NOT NULL,
    created_at timestamp with time zone NOT NULL,
    last_used_at timestamp with time zone,
    revoked_at timestamp with time zone
);

-- TABLE: analytics_dirty_cycles

CREATE TABLE analytics_dirty_cycles (
    cycle_id uuid NOT NULL,
    generation bigint DEFAULT 1 NOT NULL,
    CONSTRAINT analytics_dirty_cycles_generation_check CHECK ((generation > 0))
);

-- TABLE: analytics_retry_guards

CREATE TABLE analytics_retry_guards (
    root_id text NOT NULL
);

-- TABLE: audit_records

CREATE TABLE audit_records (
    id text NOT NULL,
    actor text NOT NULL,
    action text NOT NULL,
    entity_type text NOT NULL,
    entity_id text NOT NULL,
    detail_json jsonb NOT NULL,
    created_at timestamp with time zone NOT NULL,
    CONSTRAINT audit_records_detail_json_check CHECK ((jsonb_typeof(detail_json) = 'object'::text))
);

-- TABLE: audit_totals

CREATE TABLE audit_totals (
    id boolean DEFAULT true NOT NULL,
    total bigint NOT NULL,
    CONSTRAINT audit_totals_id_check CHECK (id),
    CONSTRAINT audit_totals_total_check CHECK ((total >= 0))
);

-- TABLE: authorization_group_nodes

CREATE TABLE authorization_group_nodes (
    group_id text NOT NULL,
    node_id text NOT NULL,
    created_at timestamp with time zone NOT NULL
);

-- TABLE: authorization_group_previews

CREATE TABLE authorization_group_previews (
    token text NOT NULL,
    action text NOT NULL,
    group_id text,
    user_id text,
    request_json jsonb NOT NULL,
    guard_json jsonb NOT NULL,
    result_json jsonb NOT NULL,
    created_at timestamp with time zone NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    consumed_at timestamp with time zone,
    CONSTRAINT authorization_group_previews_action_check CHECK ((action = ANY (ARRAY['create'::text, 'update'::text, 'delete'::text, 'update_memberships'::text]))),
    CONSTRAINT authorization_group_previews_check CHECK (((action = 'update_memberships'::text) = (user_id IS NOT NULL))),
    CONSTRAINT authorization_group_previews_check1 CHECK (((action = ANY (ARRAY['update'::text, 'delete'::text])) = (group_id IS NOT NULL))),
    CONSTRAINT authorization_group_previews_guard_json_check CHECK ((jsonb_typeof(guard_json) = 'object'::text)),
    CONSTRAINT authorization_group_previews_request_json_check CHECK ((jsonb_typeof(request_json) = 'object'::text)),
    CONSTRAINT authorization_group_previews_result_json_check CHECK ((jsonb_typeof(result_json) = 'object'::text))
);

-- TABLE: authorization_group_users

CREATE TABLE authorization_group_users (
    group_id text NOT NULL,
    user_id text NOT NULL,
    created_at timestamp with time zone NOT NULL
);

-- TABLE: authorization_groups

CREATE TABLE authorization_groups (
    id text NOT NULL,
    name text NOT NULL,
    revision bigint DEFAULT 1 NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    CONSTRAINT authorization_groups_name_check CHECK (((length(btrim(name)) >= 1) AND (length(btrim(name)) <= 180))),
    CONSTRAINT authorization_groups_revision_check CHECK ((revision > 0))
);

-- TABLE: background_initialization

CREATE TABLE background_initialization (
    key text NOT NULL,
    completed_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL
);

-- TABLE: background_work

CREATE TABLE background_work (
    id text NOT NULL,
    kind text NOT NULL,
    cycle_id uuid,
    node_id text,
    payload jsonb DEFAULT '{}'::jsonb NOT NULL,
    state text DEFAULT 'queued'::text NOT NULL,
    owner text,
    generation bigint DEFAULT 0 NOT NULL,
    available_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    deadline_at timestamp with time zone,
    started_at timestamp with time zone,
    finished_at timestamp with time zone,
    error_code text,
    CONSTRAINT background_work_check CHECK (((state = 'running'::text) = (owner IS NOT NULL))),
    CONSTRAINT background_work_state_check CHECK ((state = ANY (ARRAY['queued'::text, 'running'::text, 'complete'::text, 'failed'::text, 'expired'::text])))
);

-- TABLE: cache_entity_versions

CREATE TABLE cache_entity_versions (
    kind text NOT NULL,
    entity_id text NOT NULL,
    generation bigint DEFAULT 0 NOT NULL,
    deleted boolean DEFAULT false NOT NULL,
    bucket smallint NOT NULL,
    bucket_generation bigint DEFAULT 0 NOT NULL
);

-- VIEW: batch_cache_versions

CREATE VIEW batch_cache_versions AS
 SELECT entity_id AS batch_id,
    generation
   FROM cache_entity_versions
  WHERE (kind = 'credential_batches'::text);

-- TABLE: cache_bucket_pending

CREATE TABLE cache_bucket_pending (
    transaction_id bigint NOT NULL,
    domain text NOT NULL,
    bucket smallint NOT NULL
);

-- TABLE: cache_bucket_versions

CREATE TABLE cache_bucket_versions (
    domain text NOT NULL,
    bucket smallint NOT NULL,
    generation bigint DEFAULT 0 NOT NULL,
    CONSTRAINT cache_bucket_versions_bucket_check CHECK (((bucket >= 0) AND (bucket < 64)))
);

-- TABLE: cache_entity_pending

CREATE TABLE cache_entity_pending (
    transaction_id bigint NOT NULL,
    kind text NOT NULL,
    entity_id text NOT NULL
);

-- TABLE: cache_flush_pending

CREATE TABLE cache_flush_pending (
    transaction_id bigint NOT NULL
);

-- TABLE: cache_version_epoch

CREATE TABLE cache_version_epoch (
    id boolean NOT NULL,
    epoch text NOT NULL,
    CONSTRAINT cache_version_epoch_id_check CHECK (id)
);

-- TABLE: config_resource_references

CREATE TABLE config_resource_references (
    kind text NOT NULL,
    source_id text NOT NULL,
    node_id text NOT NULL,
    config_version_id text,
    config_revision bigint,
    source text NOT NULL,
    resource_id text NOT NULL,
    CONSTRAINT resource_reference_shape CHECK ((((kind = 'node'::text) AND (source = ANY (ARRAY['desired'::text, 'deployed'::text])) AND (source_id = node_id) AND (config_version_id IS NULL) AND (config_revision IS NULL)) OR ((kind = 'history'::text) AND (source = 'history'::text) AND (source_id = config_version_id) AND (config_version_id IS NOT NULL) AND (config_revision IS NOT NULL) AND (config_revision > 0))))
);

-- TABLE: config_resources

CREATE TABLE config_resources (
    id text NOT NULL,
    node_id text NOT NULL,
    name text NOT NULL,
    resource_kind text NOT NULL,
    content_enc text NOT NULL,
    content_sha256 text NOT NULL,
    size_bytes bigint NOT NULL,
    created_at timestamp with time zone NOT NULL,
    CONSTRAINT config_resources_resource_kind_check CHECK ((resource_kind = ANY (ARRAY['certificate'::text, 'private_key'::text, 'ech_key'::text, 'acl'::text, 'geoip'::text, 'geosite'::text]))),
    CONSTRAINT config_resources_size_bytes_check CHECK (((size_bytes >= 1) AND (size_bytes <= 20971520)))
);

-- TABLE: config_versions

CREATE TABLE config_versions (
    id text NOT NULL,
    node_id text NOT NULL,
    revision bigint NOT NULL,
    config_enc text NOT NULL,
    content_sha256 text NOT NULL,
    deployed_success boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone NOT NULL
);

-- TABLE: credential_application_receipts

CREATE TABLE credential_application_receipts (
    job_id text NOT NULL,
    source jsonb NOT NULL,
    execution jsonb NOT NULL,
    stage text NOT NULL,
    result jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL
);

-- TABLE: credential_batch_items

CREATE TABLE credential_batch_items (
    batch_id text NOT NULL,
    node_id text NOT NULL,
    user_id text DEFAULT ''::text NOT NULL,
    expected_revision bigint NOT NULL,
    job_id text NOT NULL,
    applied_at timestamp with time zone,
    apply_stage text,
    CONSTRAINT credential_batch_items_check CHECK (((applied_at IS NULL) = (apply_stage IS NULL)))
);

-- TABLE: credential_batches

CREATE TABLE credential_batches (
    id text NOT NULL,
    credential_id text NOT NULL,
    version bigint NOT NULL,
    created_at timestamp with time zone NOT NULL
);

-- TABLE: credential_versions

CREATE TABLE credential_versions (
    credential_id text NOT NULL,
    version bigint NOT NULL,
    payload_enc text NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb NOT NULL,
    artifact_names jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone NOT NULL,
    CONSTRAINT credential_versions_version_check CHECK ((version > 0))
);

-- TABLE: credentials

CREATE TABLE credentials (
    id text NOT NULL,
    name text NOT NULL,
    kind text NOT NULL,
    owner_user_id text,
    revision bigint DEFAULT 1 NOT NULL,
    latest_version bigint DEFAULT 1 NOT NULL,
    archived boolean DEFAULT false NOT NULL,
    reminder_at timestamp with time zone,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    CONSTRAINT credentials_kind_check CHECK ((kind = ANY (ARRAY['ssh_private_key'::text, 'ssh_password'::text, 'tls_identity'::text, 'ca_certificate'::text, 'ca_identity'::text, 'certificate'::text, 'private_key'::text, 'ech_key'::text, 'dns'::text, 'api_token'::text]))),
    CONSTRAINT credentials_latest_version_check CHECK ((latest_version > 0)),
    CONSTRAINT credentials_revision_check CHECK ((revision > 0))
);

-- TABLE: mtls_authorities

CREATE TABLE mtls_authorities (
    credential_id text NOT NULL,
    source text NOT NULL,
    policy_revision bigint DEFAULT 1 NOT NULL,
    client_validity_days integer DEFAULT 180 NOT NULL,
    renew_before_hours integer DEFAULT 720 NOT NULL,
    published_version bigint DEFAULT 1 NOT NULL,
    next_action_at timestamp with time zone,
    maintenance_error text,
    CONSTRAINT mtls_authorities_check CHECK (((renew_before_hours >= 1) AND (renew_before_hours < (client_validity_days * 24)))),
    CONSTRAINT mtls_authorities_client_validity_days_check CHECK (((client_validity_days >= 1) AND (client_validity_days <= 365))),
    CONSTRAINT mtls_authorities_policy_revision_check CHECK ((policy_revision > 0)),
    CONSTRAINT mtls_authorities_source_check CHECK ((source = ANY (ARRAY['generated'::text, 'imported'::text])))
);

-- TABLE: node_assignments

CREATE TABLE node_assignments (
    user_id text NOT NULL,
    node_id text NOT NULL,
    credential_hash text NOT NULL,
    credential_enc text NOT NULL,
    client_certificate_enc text,
    client_private_key_enc text,
    created_at timestamp with time zone NOT NULL,
    mtls_credential_id text,
    mtls_credential_version bigint,
    CONSTRAINT assignment_mtls_credential_pair CHECK (((mtls_credential_id IS NULL) = (mtls_credential_version IS NULL)))
);

-- TABLE: nodes

CREATE TABLE nodes (
    id text NOT NULL,
    name text NOT NULL,
    ssh_host text NOT NULL,
    ssh_port integer NOT NULL,
    ssh_username text NOT NULL,
    ssh_auth_type text NOT NULL,
    ssh_secret_enc text NOT NULL,
    ssh_passphrase_enc text,
    ssh_host_fingerprint text,
    public_host text NOT NULL,
    public_port integer NOT NULL,
    listen_addr text NOT NULL,
    tls_sni text,
    tls_skip_verify boolean DEFAULT false NOT NULL,
    node_token_hash text NOT NULL,
    node_token_enc text NOT NULL,
    traffic_stats_secret_enc text NOT NULL,
    desired_config_enc text NOT NULL,
    desired_revision bigint DEFAULT 1 NOT NULL,
    deployed_config_enc text,
    deployed_revision bigint,
    deployed_content_sha256 text,
    state text DEFAULT 'new'::text NOT NULL,
    last_seen_at timestamp with time zone,
    last_sample_at timestamp with time zone,
    proxy_probe_url text,
    traffic_stats_port integer DEFAULT 9780 NOT NULL,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    ssh_credential_id text,
    ssh_credential_version bigint,
    published_connection jsonb,
    mtls_state text DEFAULT 'ready'::text NOT NULL,
    mtls_error text,
    mtls_generation bigint DEFAULT 0 NOT NULL,
    mtls_activation_id text,
    mtls_recovery_configured boolean DEFAULT true NOT NULL,
    traffic_capture_mode text DEFAULT 'interval'::text NOT NULL,
    CONSTRAINT nodes_mtls_state_check CHECK ((mtls_state = ANY (ARRAY['ready'::text, 'needs_configuration'::text, 'activating'::text, 'recovery_required'::text]))),
    CONSTRAINT nodes_public_port_check CHECK (((public_port >= 1) AND (public_port <= 65535))),
    CONSTRAINT nodes_ssh_auth_type_check CHECK ((ssh_auth_type = ANY (ARRAY['password'::text, 'private_key'::text]))),
    CONSTRAINT nodes_ssh_port_check CHECK (((ssh_port >= 1) AND (ssh_port <= 65535))),
    CONSTRAINT nodes_traffic_capture_mode_check CHECK ((traffic_capture_mode = 'interval'::text)),
    CONSTRAINT nodes_traffic_stats_port_check CHECK (((traffic_stats_port >= 1) AND (traffic_stats_port <= 65535)))
);

-- TABLE: subscription_credentials

CREATE TABLE subscription_credentials (
    id text NOT NULL,
    user_id text NOT NULL,
    token_hash text NOT NULL,
    token_enc text NOT NULL,
    created_at timestamp with time zone NOT NULL,
    revoked_at timestamp with time zone
);

-- TABLE: users

CREATE TABLE users (
    id text NOT NULL,
    name text NOT NULL,
    enabled boolean DEFAULT true NOT NULL,
    expires_at timestamp with time zone,
    quota_bytes bigint,
    usage_bytes bigint DEFAULT 0 NOT NULL,
    revision bigint DEFAULT 1 NOT NULL,
    quota_reset_at timestamp with time zone,
    access_kick_enqueued_at timestamp with time zone,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    CONSTRAINT users_quota_bytes_check CHECK (((quota_bytes IS NULL) OR (quota_bytes >= 0))),
    CONSTRAINT users_usage_bytes_check CHECK ((usage_bytes >= 0))
);

-- VIEW: credential_directory

CREATE VIEW credential_directory AS
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
 SELECT ((('user:'::text || a.user_id) || ':'::text) || a.node_id) AS id,
    ((u.name || ' · '::text) || n.name) AS name,
    'user_credential'::text AS kind,
    a.user_id AS owner_user_id,
    a.created_at,
    hysteriax_directory_user_status(u.enabled, u.expires_at, u.quota_bytes, u.usage_bytes) AS status,
    'assignment'::text AS source,
    a.user_id AS entity_id,
    a.node_id
   FROM ((node_assignments a
     JOIN users u ON ((u.id = a.user_id)))
     JOIN nodes n ON ((n.id = a.node_id)));

-- TABLE: credential_migrations

CREATE TABLE credential_migrations (
    name text NOT NULL,
    completed_at timestamp with time zone NOT NULL
);

-- TABLE: credential_reference_dirty

CREATE TABLE credential_reference_dirty (
    kind text NOT NULL,
    source_id text NOT NULL,
    generation bigint DEFAULT 1 NOT NULL,
    CONSTRAINT credential_reference_dirty_generation_check CHECK ((generation > 0)),
    CONSTRAINT credential_reference_dirty_kind_check CHECK ((kind = ANY (ARRAY['node'::text, 'history'::text])))
);

-- TABLE: credential_reference_edges

CREATE TABLE credential_reference_edges (
    kind text NOT NULL,
    source_id text NOT NULL,
    node_id text NOT NULL,
    config_version_id text,
    config_revision bigint,
    source text NOT NULL,
    field text NOT NULL,
    credential_id text NOT NULL,
    version bigint NOT NULL
);

-- TABLE: data_gaps

CREATE TABLE data_gaps (
    id text NOT NULL,
    node_id text NOT NULL,
    opened_at timestamp with time zone NOT NULL,
    resolved_at timestamp with time zone,
    reason text NOT NULL
);

-- TABLE: deployment_probe_tokens

CREATE TABLE deployment_probe_tokens (
    node_id text NOT NULL,
    token_hash text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    job_id text,
    owner text,
    node_generation bigint,
    job_generation bigint,
    epoch text,
    CONSTRAINT deployment_probe_execution_scope CHECK ((((owner IS NULL) AND (node_generation IS NULL) AND (job_generation IS NULL)) OR ((owner IS NOT NULL) AND (owner <> ''::text) AND (node_generation > 0) AND (job_generation > 0) AND (node_generation IS NOT NULL) AND (job_generation IS NOT NULL))))
);

-- TABLE: dirty_key_versions

CREATE TABLE dirty_key_versions (
    queue text NOT NULL,
    entity_key text NOT NULL,
    generation bigint NOT NULL,
    CONSTRAINT dirty_key_versions_generation_check CHECK ((generation > 0))
);

-- TABLE: dns_bindings

CREATE TABLE dns_bindings (
    node_id text NOT NULL,
    zone_id text NOT NULL,
    hostname text NOT NULL,
    record_ids jsonb NOT NULL,
    revision bigint DEFAULT 1 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);

-- TABLE: dns_connections

CREATE TABLE dns_connections (
    id text NOT NULL,
    name text NOT NULL,
    provider text NOT NULL,
    credential_id text NOT NULL,
    credential_version bigint NOT NULL,
    revision bigint DEFAULT 1 NOT NULL,
    status text DEFAULT 'unverified'::text NOT NULL,
    verified_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT dns_connections_provider_check CHECK ((provider = 'cloudflare'::text))
);

-- TABLE: dns_credential_batch_items

CREATE TABLE dns_credential_batch_items (
    batch_id text NOT NULL,
    connection_id text NOT NULL,
    job_id text NOT NULL,
    expected_revision bigint NOT NULL,
    applied_at timestamp with time zone
);

-- TABLE: dns_operations

CREATE TABLE dns_operations (
    id text NOT NULL,
    idempotency_key text NOT NULL,
    request_sha256 text NOT NULL,
    job_id text,
    connection_id text NOT NULL,
    credential_version bigint NOT NULL,
    resource_type text NOT NULL,
    resource_id text NOT NULL,
    action text NOT NULL,
    payload jsonb NOT NULL,
    result jsonb,
    applied_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

-- TABLE: dns_records

CREATE TABLE dns_records (
    id text NOT NULL,
    zone_id text NOT NULL,
    provider_record_id text,
    name text NOT NULL,
    record_type text NOT NULL,
    content text NOT NULL,
    ttl bigint DEFAULT 1 NOT NULL,
    proxied boolean DEFAULT false NOT NULL,
    origin text DEFAULT 'external'::text NOT NULL,
    revision bigint DEFAULT 1 NOT NULL,
    remote_snapshot jsonb,
    desired jsonb,
    state text DEFAULT 'pending'::text NOT NULL,
    resolution_status text DEFAULT 'unchecked'::text NOT NULL,
    resolution_detail jsonb,
    checked_at timestamp with time zone,
    deleted_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT dns_records_origin_check CHECK ((origin = ANY (ARRAY['external'::text, 'hysteriax'::text])))
);

-- TABLE: dns_zones

CREATE TABLE dns_zones (
    id text NOT NULL,
    connection_id text NOT NULL,
    provider_zone_id text NOT NULL,
    name text NOT NULL,
    enabled boolean DEFAULT false NOT NULL,
    synced_at timestamp with time zone,
    revision bigint DEFAULT 1 NOT NULL
);

-- TABLE: event_publication_pending

CREATE TABLE event_publication_pending (
    event_id bigint NOT NULL
);

-- TABLE: event_publication_stream

CREATE TABLE event_publication_stream (
    id boolean DEFAULT true NOT NULL,
    epoch text NOT NULL,
    head bigint DEFAULT 0 NOT NULL,
    CONSTRAINT event_publication_stream_head_check CHECK ((head >= 0)),
    CONSTRAINT event_publication_stream_id_check CHECK (id)
);

-- TABLE: event_publications

CREATE TABLE event_publications (
    publication_seq bigint NOT NULL,
    event_id bigint NOT NULL
);

-- TABLE: execution_operations

CREATE TABLE execution_operations (
    operation_id text NOT NULL,
    resource_key text NOT NULL,
    owner text NOT NULL,
    generation bigint NOT NULL,
    work_id text NOT NULL,
    step text NOT NULL,
    state text NOT NULL,
    intent jsonb NOT NULL,
    evidence jsonb,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    updated_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    epoch text DEFAULT hysteriax_execution_epoch() NOT NULL,
    CONSTRAINT execution_operations_state_check CHECK ((state = ANY (ARRAY['dispatching'::text, 'verified'::text, 'reconciling'::text, 'needs_attention'::text])))
);

-- TABLE: execution_recheck_requests

CREATE TABLE execution_recheck_requests (
    namespace text NOT NULL,
    request_digest text NOT NULL,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL
);

-- TABLE: execution_rechecks

CREATE TABLE execution_rechecks (
    id text NOT NULL,
    request_namespace text NOT NULL,
    request_digest text NOT NULL,
    operation_id text NOT NULL,
    prepared jsonb NOT NULL,
    state text NOT NULL,
    proof jsonb,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    updated_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT execution_rechecks_state_check CHECK ((state = ANY (ARRAY['prepared'::text, 'dispatching'::text, 'verified'::text, 'needs_attention'::text, 'obsolete'::text])))
);

-- VIEW: job_cache_versions

CREATE VIEW job_cache_versions AS
 SELECT entity_id AS job_id,
    generation
   FROM cache_entity_versions
  WHERE (kind = 'jobs'::text);

-- TABLE: job_events

CREATE TABLE job_events (
    id bigint NOT NULL,
    job_id text NOT NULL,
    event_type text NOT NULL,
    payload_json jsonb NOT NULL,
    created_at timestamp with time zone NOT NULL,
    CONSTRAINT job_events_payload_json_check CHECK ((jsonb_typeof(payload_json) = 'object'::text))
);

-- SEQUENCE: job_events_id_seq

ALTER TABLE job_events ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME job_events_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

-- TABLE: job_failed_minute_totals

CREATE TABLE job_failed_minute_totals (
    minute timestamp with time zone NOT NULL,
    total bigint NOT NULL
);

-- TABLE: job_node_kick_totals

CREATE TABLE job_node_kick_totals (
    node_id text NOT NULL,
    pending bigint NOT NULL,
    pending_limit bigint NOT NULL,
    failed_limit bigint NOT NULL
);

-- TABLE: job_retry_chain_members

CREATE TABLE job_retry_chain_members (
    job_id text NOT NULL,
    root_id text NOT NULL
);

-- TABLE: job_retry_chain_state

CREATE TABLE job_retry_chain_state (
    leaf_id text NOT NULL,
    root_id text NOT NULL,
    name text NOT NULL,
    status text NOT NULL,
    last_error text,
    last_failure_at timestamp with time zone,
    failed_seed_at timestamp with time zone
);

-- TABLE: job_retry_state_dirty

CREATE TABLE job_retry_state_dirty (
    job_id text NOT NULL,
    generation bigint DEFAULT 1 NOT NULL,
    CONSTRAINT job_retry_state_dirty_generation_check CHECK ((generation > 0))
);

-- TABLE: job_status_totals

CREATE TABLE job_status_totals (
    status text NOT NULL,
    total bigint NOT NULL
);

-- TABLE: kick_requests

CREATE TABLE kick_requests (
    node_id text NOT NULL,
    user_id text NOT NULL,
    reasons jsonb NOT NULL,
    generation bigint DEFAULT 1 NOT NULL,
    state text NOT NULL,
    latest_job_id text,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT kick_requests_state_check CHECK ((state = ANY (ARRAY['active'::text, 'waiting_recovery'::text, 'needs_attention'::text, 'completed'::text, 'cancelled'::text])))
);

-- TABLE: management_receipts

CREATE TABLE management_receipts (
    actor_id text NOT NULL,
    operation text NOT NULL,
    idempotency_key text NOT NULL,
    request_digest text NOT NULL,
    response_enc text NOT NULL,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT management_receipts_idempotency_key_check CHECK (((octet_length(idempotency_key) >= 1) AND (octet_length(idempotency_key) <= 200))),
    CONSTRAINT management_receipts_request_digest_check CHECK ((length(request_digest) = 64))
);

-- TABLE: monitoring_leases

CREATE TABLE monitoring_leases (
    node_id text NOT NULL,
    owner uuid NOT NULL,
    expires_at timestamp with time zone NOT NULL
);

-- TABLE: monitoring_probe_state

CREATE TABLE monitoring_probe_state (
    node_id text NOT NULL,
    revision bigint NOT NULL,
    health text DEFAULT 'checking'::text NOT NULL,
    failures integer DEFAULT 0 NOT NULL,
    successes integer DEFAULT 0 NOT NULL,
    sampled_at timestamp with time zone NOT NULL
);

-- TABLE: monitoring_probe_tokens

CREATE TABLE monitoring_probe_tokens (
    node_id text NOT NULL,
    token_hash text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    work_id text,
    owner text,
    node_generation bigint,
    work_generation bigint,
    epoch text,
    CONSTRAINT monitoring_probe_execution_scope CHECK ((((work_id IS NULL) AND (owner IS NULL) AND (node_generation IS NULL) AND (work_generation IS NULL)) OR ((work_id IS NOT NULL) AND (work_id <> ''::text) AND (owner IS NOT NULL) AND (owner <> ''::text) AND (node_generation IS NOT NULL) AND (node_generation > 0) AND (work_generation IS NOT NULL) AND (work_generation > 0))))
);

-- TABLE: mtls_activation_trust

CREATE TABLE mtls_activation_trust (
    activation_id text NOT NULL,
    ca_id text NOT NULL,
    ca_version bigint NOT NULL
);

-- TABLE: mtls_activations

CREATE TABLE mtls_activations (
    id text NOT NULL,
    node_id text NOT NULL,
    job_id text NOT NULL,
    generation bigint NOT NULL,
    target_revision bigint NOT NULL,
    phase text NOT NULL,
    previous_state text NOT NULL,
    previous_digest text,
    target_digest text NOT NULL,
    snapshot_enc text NOT NULL,
    trust jsonb NOT NULL,
    probe_credential_id text,
    probe_credential_version bigint,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    previous_activation_id text,
    CONSTRAINT mtls_activations_check CHECK (((probe_credential_id IS NULL) = (probe_credential_version IS NULL))),
    CONSTRAINT mtls_activations_phase_check CHECK ((phase = ANY (ARRAY['activating'::text, 'verified'::text, 'committed'::text, 'rolled_back'::text, 'recovery_required'::text])))
);

-- TABLE: mtls_active_probes

CREATE TABLE mtls_active_probes (
    node_id text NOT NULL,
    ca_id text NOT NULL,
    ca_version bigint NOT NULL,
    credential_id text NOT NULL,
    credential_version bigint NOT NULL,
    revision bigint DEFAULT 1 NOT NULL
);

-- TABLE: mtls_ca_generations

CREATE TABLE mtls_ca_generations (
    ca_id text NOT NULL,
    ca_version bigint NOT NULL,
    state text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    published_at timestamp with time zone,
    CONSTRAINT mtls_ca_generations_state_check CHECK ((state = ANY (ARRAY['preparing'::text, 'published'::text, 'retired'::text])))
);

-- TABLE: mtls_cleanup_nodes

CREATE TABLE mtls_cleanup_nodes (
    ca_id text NOT NULL,
    ca_version bigint NOT NULL,
    node_id text NOT NULL,
    job_id text,
    completed boolean DEFAULT false NOT NULL,
    error_message text
);

-- TABLE: mtls_deployment_snapshots

CREATE TABLE mtls_deployment_snapshots (
    node_id text NOT NULL,
    revision bigint NOT NULL,
    activation_id text NOT NULL,
    snapshot_enc text NOT NULL,
    created_at timestamp with time zone NOT NULL
);

-- VIEW: mtls_live_activation_trust

CREATE VIEW mtls_live_activation_trust AS
 SELECT a.node_id,
    a.id AS activation_id,
    a.generation,
    t.ca_id,
    t.ca_version
   FROM ((mtls_activations a
     JOIN nodes n ON ((n.id = a.node_id)))
     JOIN mtls_activation_trust t ON ((t.activation_id = a.id)))
  WHERE ((a.phase = ANY (ARRAY['activating'::text, 'verified'::text, 'recovery_required'::text])) AND (NOT (EXISTS ( SELECT 1
           FROM mtls_activations newer
          WHERE ((newer.node_id = a.node_id) AND (newer.phase = 'committed'::text) AND (newer.generation > a.generation))))));

-- TABLE: mtls_node_operations

CREATE TABLE mtls_node_operations (
    job_id text NOT NULL,
    node_id text NOT NULL,
    ca_id text NOT NULL,
    ca_version bigint NOT NULL,
    rotation_id text,
    stage text NOT NULL,
    desired_revision bigint NOT NULL,
    use_desired boolean DEFAULT false NOT NULL,
    base_revision bigint,
    base_activation_id text,
    base_digest text,
    effective_version bigint,
    created_at timestamp with time zone NOT NULL,
    recovery boolean DEFAULT false NOT NULL,
    pending_job boolean DEFAULT false NOT NULL,
    CONSTRAINT mtls_node_operations_stage_check CHECK ((stage = ANY (ARRAY['trust'::text, 'cleanup'::text])))
);

-- TABLE: mtls_node_trust

CREATE TABLE mtls_node_trust (
    node_id text NOT NULL,
    ca_id text NOT NULL,
    ca_version bigint NOT NULL,
    desired boolean DEFAULT false NOT NULL,
    deployed boolean DEFAULT false NOT NULL,
    revision bigint DEFAULT 1 NOT NULL
);

-- TABLE: mtls_operations

CREATE TABLE mtls_operations (
    actor_id text NOT NULL,
    idempotency_key text NOT NULL,
    operation text NOT NULL,
    request_digest text NOT NULL,
    receipt jsonb NOT NULL,
    created_at timestamp with time zone NOT NULL,
    CONSTRAINT mtls_operations_idempotency_key_check CHECK (((length(idempotency_key) >= 1) AND (length(idempotency_key) <= 180)))
);

-- TABLE: mtls_probe_versions

CREATE TABLE mtls_probe_versions (
    node_id text NOT NULL,
    credential_id text NOT NULL,
    credential_version bigint NOT NULL,
    ca_id text NOT NULL,
    ca_version bigint NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    next_action_at timestamp with time zone NOT NULL,
    discarded boolean DEFAULT false NOT NULL
);

-- TABLE: mtls_rotation_nodes

CREATE TABLE mtls_rotation_nodes (
    rotation_id text NOT NULL,
    node_id text NOT NULL,
    expected_revision bigint NOT NULL,
    trust_prepared boolean DEFAULT false NOT NULL,
    probe_switched boolean DEFAULT false NOT NULL,
    job_id text,
    error_message text
);

-- TABLE: mtls_rotations

CREATE TABLE mtls_rotations (
    id text NOT NULL,
    ca_id text NOT NULL,
    ca_version bigint NOT NULL,
    phase text NOT NULL,
    revision bigint DEFAULT 1 NOT NULL,
    batch_id text NOT NULL,
    next_action_at timestamp with time zone NOT NULL,
    error_message text,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    actor_id text DEFAULT 'system:mtls'::text NOT NULL,
    published_at timestamp with time zone,
    prepared_count bigint DEFAULT 0 NOT NULL,
    retry_requested boolean DEFAULT false NOT NULL,
    previous_version bigint,
    outcome text,
    CONSTRAINT mtls_rotations_phase_check CHECK ((phase = ANY (ARRAY['trust'::text, 'identities'::text, 'publish'::text, 'probes'::text, 'complete'::text, 'failed'::text])))
);

-- VIEW: mtls_superseded_node_jobs

CREATE VIEW mtls_superseded_node_jobs AS
 SELECT o.job_id
   FROM (mtls_node_operations o
     JOIN mtls_rotations r ON ((r.id = o.rotation_id)))
  WHERE (r.outcome IS NOT NULL);

-- TABLE: mtls_trust_cleanup

CREATE TABLE mtls_trust_cleanup (
    ca_id text NOT NULL,
    ca_version bigint NOT NULL,
    next_action_at timestamp with time zone NOT NULL,
    completed_at timestamp with time zone
);

-- TABLE: mtls_user_identities

CREATE TABLE mtls_user_identities (
    id text NOT NULL,
    user_id text NOT NULL,
    ca_id text NOT NULL,
    credential_id text,
    revision bigint DEFAULT 1 NOT NULL,
    authorization_revision bigint DEFAULT 1 NOT NULL,
    archived boolean DEFAULT false NOT NULL,
    next_action_at timestamp with time zone,
    maintenance_error text,
    created_at timestamp with time zone NOT NULL,
    updated_at timestamp with time zone NOT NULL,
    scheduled_ca_version bigint,
    CONSTRAINT mtls_user_identities_authorization_revision_check CHECK ((authorization_revision > 0)),
    CONSTRAINT mtls_user_identities_revision_check CHECK ((revision > 0)),
    CONSTRAINT mtls_user_identities_scheduled_ca_version_check CHECK ((scheduled_ca_version > 0))
);

-- TABLE: mtls_user_versions

CREATE TABLE mtls_user_versions (
    identity_id text NOT NULL,
    credential_id text NOT NULL,
    credential_version bigint NOT NULL,
    ca_id text NOT NULL,
    ca_version bigint NOT NULL,
    not_before timestamp with time zone NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    next_action_at timestamp with time zone NOT NULL,
    released boolean DEFAULT false NOT NULL,
    discarded boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone NOT NULL,
    CONSTRAINT mtls_user_versions_check CHECK ((expires_at > not_before))
);

-- VIEW: mtls_user_version_publication

CREATE VIEW mtls_user_version_publication AS
 SELECT v.identity_id,
    v.credential_id,
    v.credential_version,
    v.ca_id,
    v.ca_version,
    v.not_before,
    v.expires_at,
    v.next_action_at,
    (v.released OR (g.published_at IS NOT NULL)) AS released,
    (v.discarded OR ((NOT v.released) AND (g.state = 'retired'::text) AND (g.published_at IS NULL))) AS discarded,
    v.created_at
   FROM (mtls_user_versions v
     JOIN mtls_ca_generations g ON (((g.ca_id = v.ca_id) AND (g.ca_version = v.ca_version))));

-- TABLE: node_acme_observations

CREATE TABLE node_acme_observations (
    node_id text NOT NULL,
    revision bigint NOT NULL,
    deployment_digest text NOT NULL,
    policy_id text NOT NULL,
    status jsonb NOT NULL,
    sampled_at timestamp with time zone NOT NULL
);

-- TABLE: node_alerts

CREATE TABLE node_alerts (
    id text NOT NULL,
    node_id text NOT NULL,
    kind text NOT NULL,
    scope text NOT NULL,
    active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone NOT NULL
);

-- TABLE: node_authorization_versions

CREATE TABLE node_authorization_versions (
    node_id text NOT NULL,
    generation bigint DEFAULT 0 NOT NULL,
    CONSTRAINT node_authorization_versions_generation_check CHECK ((generation >= 0))
);

-- TABLE: node_limit_epochs

CREATE TABLE node_limit_epochs (
    node_id text NOT NULL,
    generation bigint NOT NULL,
    CONSTRAINT node_limit_epochs_generation_check CHECK ((generation > 0))
);

-- TABLE: node_network_samples

CREATE TABLE node_network_samples (
    id bigint NOT NULL,
    node_id text NOT NULL,
    period_id text NOT NULL,
    delta_tx bigint NOT NULL,
    delta_rx bigint NOT NULL,
    gap_reason text,
    sampled_at timestamp with time zone NOT NULL,
    baseline_only boolean DEFAULT false NOT NULL,
    CONSTRAINT node_network_samples_delta_rx_check CHECK ((delta_rx >= 0)),
    CONSTRAINT node_network_samples_delta_tx_check CHECK ((delta_tx >= 0))
);

-- SEQUENCE: node_network_samples_id_seq

CREATE SEQUENCE node_network_samples_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

-- SEQUENCE OWNED BY: node_network_samples_id_seq

ALTER SEQUENCE node_network_samples_id_seq OWNED BY node_network_samples.id;

-- TABLE: node_packages

CREATE TABLE node_packages (
    node_id text NOT NULL,
    config jsonb DEFAULT '{}'::jsonb NOT NULL,
    usage_bytes bigint DEFAULT 0 NOT NULL,
    period_id text NOT NULL,
    next_reset_at timestamp with time zone,
    generation bigint DEFAULT 0 NOT NULL,
    boot_id text,
    interface text,
    tx_total bigint,
    rx_total bigint,
    sampled_at timestamp with time zone,
    gap_reason text,
    restricted boolean DEFAULT false NOT NULL,
    CONSTRAINT node_packages_usage_bytes_check CHECK ((usage_bytes >= 0))
);

-- TABLE: online_cycle_metrics

CREATE TABLE online_cycle_metrics (
    cycle_id uuid NOT NULL,
    scope_node_id text NOT NULL,
    started_at timestamp with time zone NOT NULL,
    users bigint,
    connections numeric,
    covered bigint NOT NULL,
    expected bigint NOT NULL,
    incomplete boolean NOT NULL,
    traffic_covered bigint NOT NULL,
    traffic_incomplete boolean NOT NULL
);

-- TABLE: online_dirty_minutes

CREATE TABLE online_dirty_minutes (
    scope_node_id text NOT NULL,
    minute timestamp with time zone NOT NULL,
    generation bigint DEFAULT 1 NOT NULL,
    CONSTRAINT online_dirty_minutes_generation_check CHECK ((generation > 0))
);

-- TABLE: online_minute_totals

CREATE TABLE online_minute_totals (
    scope_node_id text NOT NULL,
    minute timestamp with time zone NOT NULL,
    samples bigint NOT NULL,
    user_cycles bigint NOT NULL,
    users_sum numeric NOT NULL,
    users_peak bigint,
    connection_cycles bigint NOT NULL,
    connections_sum numeric NOT NULL,
    connections_peak numeric,
    covered bigint NOT NULL,
    expected bigint NOT NULL,
    incomplete boolean NOT NULL,
    traffic_covered bigint NOT NULL,
    traffic_incomplete boolean NOT NULL
);

-- TABLE: online_samples

CREATE TABLE online_samples (
    id bigint NOT NULL,
    cycle_id uuid NOT NULL,
    node_id text NOT NULL,
    sampled_at timestamp with time zone NOT NULL,
    status text NOT NULL,
    connections jsonb DEFAULT '{}'::jsonb NOT NULL,
    cycle_started_at timestamp with time zone DEFAULT now() NOT NULL,
    expected_nodes bigint DEFAULT 1 NOT NULL,
    traffic_status text DEFAULT 'unknown'::text NOT NULL,
    pending boolean DEFAULT false NOT NULL
);

-- SEQUENCE: online_samples_id_seq

CREATE SEQUENCE online_samples_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

-- SEQUENCE OWNED BY: online_samples_id_seq

ALTER SEQUENCE online_samples_id_seq OWNED BY online_samples.id;

-- TABLE: probe_minute_totals

CREATE TABLE probe_minute_totals (
    node_id text NOT NULL,
    minute timestamp with time zone NOT NULL,
    attempts bigint NOT NULL,
    successes bigint NOT NULL
);

-- TABLE: proxy_probe_samples

CREATE TABLE proxy_probe_samples (
    id bigint NOT NULL,
    node_id text NOT NULL,
    revision bigint NOT NULL,
    sampled_at timestamp with time zone DEFAULT now() NOT NULL,
    status text NOT NULL,
    external_status text,
    connection_ms double precision,
    latency_ms double precision,
    reason text
);

-- SEQUENCE: proxy_probe_samples_id_seq

CREATE SEQUENCE proxy_probe_samples_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

-- SEQUENCE OWNED BY: proxy_probe_samples_id_seq

ALTER SEQUENCE proxy_probe_samples_id_seq OWNED BY proxy_probe_samples.id;

-- TABLE: resource_leases

CREATE TABLE resource_leases (
    resource_key text NOT NULL,
    owner text,
    generation bigint DEFAULT 0 NOT NULL,
    expires_at timestamp with time zone,
    work_id text,
    blocked boolean DEFAULT false NOT NULL,
    epoch text DEFAULT hysteriax_execution_epoch() NOT NULL,
    CONSTRAINT resource_leases_check CHECK (((owner IS NULL) = (expires_at IS NULL))),
    CONSTRAINT resource_leases_generation_check CHECK ((generation >= 0))
);

-- TABLE: runtime_contract

CREATE TABLE runtime_contract (
    id boolean DEFAULT true NOT NULL,
    schema_version bigint NOT NULL,
    management_protocol integer NOT NULL,
    master_key_fingerprint text NOT NULL,
    initialized_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT runtime_contract_id_check CHECK (id)
);

-- TABLE: sampling_cycles

CREATE TABLE sampling_cycles (
    id uuid NOT NULL,
    kind text NOT NULL,
    period_start timestamp with time zone NOT NULL,
    deadline_at timestamp with time zone NOT NULL,
    expected_nodes bigint NOT NULL,
    created_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    CONSTRAINT sampling_cycles_check CHECK ((deadline_at > period_start)),
    CONSTRAINT sampling_cycles_expected_nodes_check CHECK ((expected_nodes >= 0))
);

-- TABLE: service_instances

CREATE TABLE service_instances (
    instance_id text NOT NULL,
    started_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    heartbeat_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL
);

-- TABLE: subscription_template_initialization

CREATE TABLE subscription_template_initialization (
    id integer NOT NULL,
    CONSTRAINT subscription_template_initialization_id_check CHECK ((id = 1))
);

-- TABLE: subscription_template_versions

CREATE TABLE subscription_template_versions (
    template_id text NOT NULL,
    version bigint NOT NULL,
    document_enc text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

-- TABLE: subscription_templates

CREATE TABLE subscription_templates (
    id text NOT NULL,
    name text NOT NULL,
    format text NOT NULL,
    revision bigint DEFAULT 1 NOT NULL,
    archived boolean DEFAULT false NOT NULL,
    is_default boolean DEFAULT false NOT NULL,
    published_version bigint,
    draft_enc text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT subscription_templates_check CHECK (((NOT archived) OR (NOT is_default))),
    CONSTRAINT subscription_templates_check1 CHECK (((NOT is_default) OR (published_version IS NOT NULL))),
    CONSTRAINT subscription_templates_format_check CHECK ((format = ANY (ARRAY['mihomo'::text, 'singbox'::text])))
);

-- TABLE: traffic_baselines

CREATE TABLE traffic_baselines (
    node_id text NOT NULL,
    user_id text NOT NULL,
    instance_id text NOT NULL,
    tx_total bigint NOT NULL,
    rx_total bigint NOT NULL,
    sampled_at timestamp with time zone NOT NULL,
    sample_order bigint DEFAULT 0 NOT NULL,
    CONSTRAINT traffic_baselines_rx_total_check CHECK ((rx_total >= 0)),
    CONSTRAINT traffic_baselines_tx_total_check CHECK ((tx_total >= 0))
);

-- TABLE: traffic_dirty_hours

CREATE TABLE traffic_dirty_hours (
    source text NOT NULL,
    node_id text NOT NULL,
    hour timestamp with time zone NOT NULL,
    generation bigint DEFAULT 1 NOT NULL,
    CONSTRAINT traffic_dirty_hours_generation_check CHECK ((generation > 0))
);

-- TABLE: traffic_hour_totals

CREATE TABLE traffic_hour_totals (
    source text NOT NULL,
    node_id text NOT NULL,
    hour timestamp with time zone NOT NULL,
    tx numeric NOT NULL,
    rx numeric NOT NULL,
    samples bigint NOT NULL,
    incomplete_samples bigint NOT NULL
);

-- TABLE: traffic_minute_totals

CREATE TABLE traffic_minute_totals (
    source text NOT NULL,
    node_id text NOT NULL,
    minute timestamp with time zone NOT NULL,
    tx numeric NOT NULL,
    rx numeric NOT NULL,
    samples bigint NOT NULL,
    incomplete_samples bigint NOT NULL,
    CONSTRAINT traffic_minute_totals_source_check CHECK ((source = ANY (ARRAY['users'::text, 'network'::text])))
);

-- TABLE: traffic_records

CREATE TABLE traffic_records (
    id text NOT NULL,
    node_id text NOT NULL,
    user_id text NOT NULL,
    instance_id text NOT NULL,
    baseline_tx bigint NOT NULL,
    baseline_rx bigint NOT NULL,
    delta_tx bigint NOT NULL,
    delta_rx bigint NOT NULL,
    gap_reason text,
    sampled_at timestamp with time zone NOT NULL,
    baseline_only boolean DEFAULT false NOT NULL,
    sample_id text,
    CONSTRAINT traffic_records_baseline_rx_check CHECK ((baseline_rx >= 0)),
    CONSTRAINT traffic_records_baseline_tx_check CHECK ((baseline_tx >= 0)),
    CONSTRAINT traffic_records_delta_rx_check CHECK ((delta_rx >= 0)),
    CONSTRAINT traffic_records_delta_tx_check CHECK ((delta_tx >= 0))
);

-- TABLE: traffic_sample_batches

CREATE TABLE traffic_sample_batches (
    sample_id text NOT NULL,
    sample_order bigint NOT NULL,
    node_id text NOT NULL,
    cycle_id uuid,
    instance_id text,
    state text DEFAULT 'capturing'::text NOT NULL,
    payload_digest text,
    item_count bigint DEFAULT 0 NOT NULL,
    processed_count bigint DEFAULT 0 NOT NULL,
    sampled_at timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
    capture_deadline timestamp with time zone DEFAULT (clock_timestamp() + '00:01:00'::interval) NOT NULL,
    completed_at timestamp with time zone,
    capture_mode text DEFAULT 'interval'::text NOT NULL,
    interval_checked boolean DEFAULT false NOT NULL,
    interval_restart boolean DEFAULT false NOT NULL,
    clear_requested_at timestamp with time zone,
    clear_owner text,
    clear_work_generation bigint,
    clear_node_generation bigint,
    clear_epoch text,
    clear_instance_verified boolean DEFAULT false,
    CONSTRAINT traffic_clear_request_identity CHECK ((((clear_requested_at IS NULL) AND (clear_owner IS NULL) AND (clear_work_generation IS NULL) AND (clear_node_generation IS NULL) AND (clear_epoch IS NULL)) OR ((clear_requested_at IS NOT NULL) AND (clear_owner IS NOT NULL) AND (clear_work_generation IS NOT NULL) AND (clear_node_generation IS NOT NULL) AND (clear_epoch IS NOT NULL) AND (capture_mode <> 'cumulative'::text)))),
    CONSTRAINT traffic_pending_interval_only CHECK (((capture_mode = 'interval'::text) OR (state = ANY (ARRAY['complete'::text, 'discarded'::text])))),
    CONSTRAINT traffic_ready_instance_observed CHECK (((state <> 'ready'::text) OR (clear_instance_verified IS NOT NULL))),
    CONSTRAINT traffic_sample_batches_capture_mode_check CHECK ((capture_mode = ANY (ARRAY['cumulative'::text, 'cutover'::text, 'reset'::text, 'interval'::text]))),
    CONSTRAINT traffic_sample_batches_check CHECK (((processed_count >= 0) AND (processed_count <= item_count))),
    CONSTRAINT traffic_sample_batches_state_check CHECK ((state = ANY (ARRAY['capturing'::text, 'ready'::text, 'complete'::text, 'discarded'::text])))
);

-- SEQUENCE: traffic_sample_batches_sample_order_seq

ALTER TABLE traffic_sample_batches ALTER COLUMN sample_order ADD GENERATED ALWAYS AS IDENTITY (
    SEQUENCE NAME traffic_sample_batches_sample_order_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1
);

-- TABLE: traffic_sample_items

CREATE TABLE traffic_sample_items (
    id text DEFAULT (gen_random_uuid())::text NOT NULL,
    sample_id text NOT NULL,
    node_id text NOT NULL,
    user_id text NOT NULL,
    tx_total bigint NOT NULL,
    rx_total bigint NOT NULL,
    processed_at timestamp with time zone,
    sampled_at timestamp with time zone NOT NULL,
    CONSTRAINT traffic_sample_items_rx_total_check CHECK ((rx_total >= 0)),
    CONSTRAINT traffic_sample_items_tx_total_check CHECK ((tx_total >= 0))
);

-- TABLE: user_access_checks

CREATE TABLE user_access_checks (
    user_id text NOT NULL,
    next_action_at timestamp with time zone NOT NULL
);

-- TABLE: user_access_restriction_epochs

CREATE TABLE user_access_restriction_epochs (
    user_id text NOT NULL,
    generation bigint NOT NULL,
    CONSTRAINT user_access_restriction_epochs_generation_check CHECK ((generation > 0))
);

-- TABLE: user_node_usage

CREATE TABLE user_node_usage (
    user_id text NOT NULL,
    node_id text NOT NULL,
    tx numeric NOT NULL,
    rx numeric NOT NULL,
    samples bigint NOT NULL,
    sampled_at timestamp with time zone NOT NULL
);

-- DEFAULT: node_network_samples id

ALTER TABLE ONLY node_network_samples ALTER COLUMN id SET DEFAULT nextval('node_network_samples_id_seq'::regclass);

-- DEFAULT: online_samples id

ALTER TABLE ONLY online_samples ALTER COLUMN id SET DEFAULT nextval('online_samples_id_seq'::regclass);

-- DEFAULT: proxy_probe_samples id

ALTER TABLE ONLY proxy_probe_samples ALTER COLUMN id SET DEFAULT nextval('proxy_probe_samples_id_seq'::regclass);

-- CONSTRAINT: admin_tokens admin_tokens_pkey

ALTER TABLE ONLY admin_tokens
    ADD CONSTRAINT admin_tokens_pkey PRIMARY KEY (id);

-- CONSTRAINT: admin_tokens admin_tokens_token_hash_key

ALTER TABLE ONLY admin_tokens
    ADD CONSTRAINT admin_tokens_token_hash_key UNIQUE (token_hash);

-- CONSTRAINT: analytics_dirty_cycles analytics_dirty_cycles_pkey

ALTER TABLE ONLY analytics_dirty_cycles
    ADD CONSTRAINT analytics_dirty_cycles_pkey PRIMARY KEY (cycle_id);

-- CONSTRAINT: analytics_retry_guards analytics_retry_guards_pkey

ALTER TABLE ONLY analytics_retry_guards
    ADD CONSTRAINT analytics_retry_guards_pkey PRIMARY KEY (root_id);

-- CONSTRAINT: audit_records audit_records_pkey

ALTER TABLE ONLY audit_records
    ADD CONSTRAINT audit_records_pkey PRIMARY KEY (id);

-- CONSTRAINT: audit_totals audit_totals_pkey

ALTER TABLE ONLY audit_totals
    ADD CONSTRAINT audit_totals_pkey PRIMARY KEY (id);

-- CONSTRAINT: authorization_group_nodes authorization_group_nodes_pkey

ALTER TABLE ONLY authorization_group_nodes
    ADD CONSTRAINT authorization_group_nodes_pkey PRIMARY KEY (group_id, node_id);

-- CONSTRAINT: authorization_group_previews authorization_group_previews_pkey

ALTER TABLE ONLY authorization_group_previews
    ADD CONSTRAINT authorization_group_previews_pkey PRIMARY KEY (token);

-- CONSTRAINT: authorization_group_users authorization_group_users_pkey

ALTER TABLE ONLY authorization_group_users
    ADD CONSTRAINT authorization_group_users_pkey PRIMARY KEY (group_id, user_id);

-- CONSTRAINT: authorization_groups authorization_groups_pkey

ALTER TABLE ONLY authorization_groups
    ADD CONSTRAINT authorization_groups_pkey PRIMARY KEY (id);

-- CONSTRAINT: background_initialization background_initialization_pkey

ALTER TABLE ONLY background_initialization
    ADD CONSTRAINT background_initialization_pkey PRIMARY KEY (key);

-- CONSTRAINT: background_work background_work_cycle_id_node_id_key

ALTER TABLE ONLY background_work
    ADD CONSTRAINT background_work_cycle_id_node_id_key UNIQUE (cycle_id, node_id);

-- CONSTRAINT: background_work background_work_pkey

ALTER TABLE ONLY background_work
    ADD CONSTRAINT background_work_pkey PRIMARY KEY (id);

-- CONSTRAINT: cache_bucket_pending cache_bucket_pending_pkey

ALTER TABLE ONLY cache_bucket_pending
    ADD CONSTRAINT cache_bucket_pending_pkey PRIMARY KEY (transaction_id, domain, bucket);

-- CONSTRAINT: cache_bucket_versions cache_bucket_versions_pkey

ALTER TABLE ONLY cache_bucket_versions
    ADD CONSTRAINT cache_bucket_versions_pkey PRIMARY KEY (domain, bucket);

-- CONSTRAINT: cache_entity_pending cache_entity_pending_pkey

ALTER TABLE ONLY cache_entity_pending
    ADD CONSTRAINT cache_entity_pending_pkey PRIMARY KEY (transaction_id, kind, entity_id);

-- CONSTRAINT: cache_entity_versions cache_entity_versions_pkey

ALTER TABLE ONLY cache_entity_versions
    ADD CONSTRAINT cache_entity_versions_pkey PRIMARY KEY (kind, entity_id);

-- CONSTRAINT: cache_flush_pending cache_flush_pending_pkey

ALTER TABLE ONLY cache_flush_pending
    ADD CONSTRAINT cache_flush_pending_pkey PRIMARY KEY (transaction_id);

-- CONSTRAINT: cache_version_epoch cache_version_epoch_pkey

ALTER TABLE ONLY cache_version_epoch
    ADD CONSTRAINT cache_version_epoch_pkey PRIMARY KEY (id);

-- CONSTRAINT: config_resource_references config_resource_references_pkey

ALTER TABLE ONLY config_resource_references
    ADD CONSTRAINT config_resource_references_pkey PRIMARY KEY (kind, source_id, source, resource_id);

-- CONSTRAINT: config_resources config_resources_node_id_name_key

ALTER TABLE ONLY config_resources
    ADD CONSTRAINT config_resources_node_id_name_key UNIQUE (node_id, name);

-- CONSTRAINT: config_resources config_resources_pkey

ALTER TABLE ONLY config_resources
    ADD CONSTRAINT config_resources_pkey PRIMARY KEY (id);

-- CONSTRAINT: config_versions config_versions_node_id_revision_key

ALTER TABLE ONLY config_versions
    ADD CONSTRAINT config_versions_node_id_revision_key UNIQUE (node_id, revision);

-- CONSTRAINT: config_versions config_versions_pkey

ALTER TABLE ONLY config_versions
    ADD CONSTRAINT config_versions_pkey PRIMARY KEY (id);

-- CONSTRAINT: credential_application_receipts credential_application_receipts_pkey

ALTER TABLE ONLY credential_application_receipts
    ADD CONSTRAINT credential_application_receipts_pkey PRIMARY KEY (job_id);

-- CONSTRAINT: credential_batch_items credential_batch_items_pkey

ALTER TABLE ONLY credential_batch_items
    ADD CONSTRAINT credential_batch_items_pkey PRIMARY KEY (batch_id, node_id, user_id);

-- CONSTRAINT: credential_batches credential_batches_pkey

ALTER TABLE ONLY credential_batches
    ADD CONSTRAINT credential_batches_pkey PRIMARY KEY (id);

-- CONSTRAINT: credential_migrations credential_migrations_pkey

ALTER TABLE ONLY credential_migrations
    ADD CONSTRAINT credential_migrations_pkey PRIMARY KEY (name);

-- CONSTRAINT: credential_reference_dirty credential_reference_dirty_pkey

ALTER TABLE ONLY credential_reference_dirty
    ADD CONSTRAINT credential_reference_dirty_pkey PRIMARY KEY (kind, source_id);

-- CONSTRAINT: credential_reference_edges credential_reference_edges_pkey

ALTER TABLE ONLY credential_reference_edges
    ADD CONSTRAINT credential_reference_edges_pkey PRIMARY KEY (kind, source_id, source, field, credential_id, version);

-- CONSTRAINT: credential_versions credential_versions_pkey

ALTER TABLE ONLY credential_versions
    ADD CONSTRAINT credential_versions_pkey PRIMARY KEY (credential_id, version);

-- CONSTRAINT: credentials credentials_pkey

ALTER TABLE ONLY credentials
    ADD CONSTRAINT credentials_pkey PRIMARY KEY (id);

-- CONSTRAINT: data_gaps data_gaps_pkey

ALTER TABLE ONLY data_gaps
    ADD CONSTRAINT data_gaps_pkey PRIMARY KEY (id);

-- CONSTRAINT: deployment_probe_tokens deployment_probe_tokens_pkey

ALTER TABLE ONLY deployment_probe_tokens
    ADD CONSTRAINT deployment_probe_tokens_pkey PRIMARY KEY (node_id);

-- CONSTRAINT: dirty_key_versions dirty_key_versions_pkey

ALTER TABLE ONLY dirty_key_versions
    ADD CONSTRAINT dirty_key_versions_pkey PRIMARY KEY (queue, entity_key);

-- CONSTRAINT: dns_bindings dns_bindings_hostname_key

ALTER TABLE ONLY dns_bindings
    ADD CONSTRAINT dns_bindings_hostname_key UNIQUE (hostname);

-- CONSTRAINT: dns_bindings dns_bindings_pkey

ALTER TABLE ONLY dns_bindings
    ADD CONSTRAINT dns_bindings_pkey PRIMARY KEY (node_id);

-- CONSTRAINT: dns_connections dns_connections_pkey

ALTER TABLE ONLY dns_connections
    ADD CONSTRAINT dns_connections_pkey PRIMARY KEY (id);

-- CONSTRAINT: dns_credential_batch_items dns_credential_batch_items_pkey

ALTER TABLE ONLY dns_credential_batch_items
    ADD CONSTRAINT dns_credential_batch_items_pkey PRIMARY KEY (batch_id, connection_id);

-- CONSTRAINT: dns_operations dns_operations_idempotency_key_key

ALTER TABLE ONLY dns_operations
    ADD CONSTRAINT dns_operations_idempotency_key_key UNIQUE (idempotency_key);

-- CONSTRAINT: dns_operations dns_operations_pkey

ALTER TABLE ONLY dns_operations
    ADD CONSTRAINT dns_operations_pkey PRIMARY KEY (id);

-- CONSTRAINT: dns_records dns_records_pkey

ALTER TABLE ONLY dns_records
    ADD CONSTRAINT dns_records_pkey PRIMARY KEY (id);

-- CONSTRAINT: dns_records dns_records_zone_id_provider_record_id_key

ALTER TABLE ONLY dns_records
    ADD CONSTRAINT dns_records_zone_id_provider_record_id_key UNIQUE (zone_id, provider_record_id);

-- CONSTRAINT: dns_zones dns_zones_name_key

ALTER TABLE ONLY dns_zones
    ADD CONSTRAINT dns_zones_name_key UNIQUE (name);

-- CONSTRAINT: dns_zones dns_zones_pkey

ALTER TABLE ONLY dns_zones
    ADD CONSTRAINT dns_zones_pkey PRIMARY KEY (id);

-- CONSTRAINT: dns_zones dns_zones_provider_zone_id_key

ALTER TABLE ONLY dns_zones
    ADD CONSTRAINT dns_zones_provider_zone_id_key UNIQUE (provider_zone_id);

-- CONSTRAINT: event_publication_pending event_publication_pending_pkey

ALTER TABLE ONLY event_publication_pending
    ADD CONSTRAINT event_publication_pending_pkey PRIMARY KEY (event_id);

-- CONSTRAINT: event_publication_stream event_publication_stream_pkey

ALTER TABLE ONLY event_publication_stream
    ADD CONSTRAINT event_publication_stream_pkey PRIMARY KEY (id);

-- CONSTRAINT: event_publications event_publications_event_id_key

ALTER TABLE ONLY event_publications
    ADD CONSTRAINT event_publications_event_id_key UNIQUE (event_id);

-- CONSTRAINT: event_publications event_publications_pkey

ALTER TABLE ONLY event_publications
    ADD CONSTRAINT event_publications_pkey PRIMARY KEY (publication_seq);

-- CONSTRAINT: execution_operations execution_operations_pkey

ALTER TABLE ONLY execution_operations
    ADD CONSTRAINT execution_operations_pkey PRIMARY KEY (operation_id);

-- CONSTRAINT: execution_operations execution_operations_work_id_step_key

ALTER TABLE ONLY execution_operations
    ADD CONSTRAINT execution_operations_work_id_step_key UNIQUE (work_id, step);

-- CONSTRAINT: execution_recheck_requests execution_recheck_requests_pkey

ALTER TABLE ONLY execution_recheck_requests
    ADD CONSTRAINT execution_recheck_requests_pkey PRIMARY KEY (namespace);

-- CONSTRAINT: execution_rechecks execution_rechecks_pkey

ALTER TABLE ONLY execution_rechecks
    ADD CONSTRAINT execution_rechecks_pkey PRIMARY KEY (id);

-- CONSTRAINT: execution_rechecks execution_rechecks_request_namespace_key

ALTER TABLE ONLY execution_rechecks
    ADD CONSTRAINT execution_rechecks_request_namespace_key UNIQUE (request_namespace);

-- CONSTRAINT: job_events job_events_pkey

ALTER TABLE ONLY job_events
    ADD CONSTRAINT job_events_pkey PRIMARY KEY (id);

-- CONSTRAINT: job_failed_minute_totals job_failed_minute_totals_pkey

ALTER TABLE ONLY job_failed_minute_totals
    ADD CONSTRAINT job_failed_minute_totals_pkey PRIMARY KEY (minute);

-- CONSTRAINT: job_node_kick_totals job_node_kick_totals_pkey

ALTER TABLE ONLY job_node_kick_totals
    ADD CONSTRAINT job_node_kick_totals_pkey PRIMARY KEY (node_id);

-- CONSTRAINT: job_retry_chain_members job_retry_chain_members_pkey

ALTER TABLE ONLY job_retry_chain_members
    ADD CONSTRAINT job_retry_chain_members_pkey PRIMARY KEY (job_id);

-- CONSTRAINT: job_retry_chain_state job_retry_chain_state_pkey

ALTER TABLE ONLY job_retry_chain_state
    ADD CONSTRAINT job_retry_chain_state_pkey PRIMARY KEY (leaf_id);

-- CONSTRAINT: job_retry_state_dirty job_retry_state_dirty_pkey

ALTER TABLE ONLY job_retry_state_dirty
    ADD CONSTRAINT job_retry_state_dirty_pkey PRIMARY KEY (job_id);

-- CONSTRAINT: job_status_totals job_status_totals_pkey

ALTER TABLE ONLY job_status_totals
    ADD CONSTRAINT job_status_totals_pkey PRIMARY KEY (status);

-- CONSTRAINT: jobs jobs_pkey

ALTER TABLE ONLY jobs
    ADD CONSTRAINT jobs_pkey PRIMARY KEY (id);

-- CONSTRAINT: kick_requests kick_requests_pkey

ALTER TABLE ONLY kick_requests
    ADD CONSTRAINT kick_requests_pkey PRIMARY KEY (node_id, user_id);

-- CONSTRAINT: management_receipts management_receipts_pkey

ALTER TABLE ONLY management_receipts
    ADD CONSTRAINT management_receipts_pkey PRIMARY KEY (actor_id, operation, idempotency_key);

-- CONSTRAINT: monitoring_leases monitoring_leases_pkey

ALTER TABLE ONLY monitoring_leases
    ADD CONSTRAINT monitoring_leases_pkey PRIMARY KEY (node_id);

-- CONSTRAINT: monitoring_probe_state monitoring_probe_state_pkey

ALTER TABLE ONLY monitoring_probe_state
    ADD CONSTRAINT monitoring_probe_state_pkey PRIMARY KEY (node_id);

-- CONSTRAINT: monitoring_probe_tokens monitoring_probe_tokens_pkey

ALTER TABLE ONLY monitoring_probe_tokens
    ADD CONSTRAINT monitoring_probe_tokens_pkey PRIMARY KEY (node_id);

-- CONSTRAINT: mtls_activation_trust mtls_activation_trust_pkey

ALTER TABLE ONLY mtls_activation_trust
    ADD CONSTRAINT mtls_activation_trust_pkey PRIMARY KEY (activation_id, ca_id, ca_version);

-- CONSTRAINT: mtls_activations mtls_activations_job_id_key

ALTER TABLE ONLY mtls_activations
    ADD CONSTRAINT mtls_activations_job_id_key UNIQUE (job_id);

-- CONSTRAINT: mtls_activations mtls_activations_node_id_generation_key

ALTER TABLE ONLY mtls_activations
    ADD CONSTRAINT mtls_activations_node_id_generation_key UNIQUE (node_id, generation);

-- CONSTRAINT: mtls_activations mtls_activations_pkey

ALTER TABLE ONLY mtls_activations
    ADD CONSTRAINT mtls_activations_pkey PRIMARY KEY (id);

-- CONSTRAINT: mtls_active_probes mtls_active_probes_pkey

ALTER TABLE ONLY mtls_active_probes
    ADD CONSTRAINT mtls_active_probes_pkey PRIMARY KEY (node_id);

-- CONSTRAINT: mtls_authorities mtls_authorities_pkey

ALTER TABLE ONLY mtls_authorities
    ADD CONSTRAINT mtls_authorities_pkey PRIMARY KEY (credential_id);

-- CONSTRAINT: mtls_ca_generations mtls_ca_generations_pkey

ALTER TABLE ONLY mtls_ca_generations
    ADD CONSTRAINT mtls_ca_generations_pkey PRIMARY KEY (ca_id, ca_version);

-- CONSTRAINT: mtls_cleanup_nodes mtls_cleanup_nodes_pkey

ALTER TABLE ONLY mtls_cleanup_nodes
    ADD CONSTRAINT mtls_cleanup_nodes_pkey PRIMARY KEY (ca_id, ca_version, node_id);

-- CONSTRAINT: mtls_deployment_snapshots mtls_deployment_snapshots_pkey

ALTER TABLE ONLY mtls_deployment_snapshots
    ADD CONSTRAINT mtls_deployment_snapshots_pkey PRIMARY KEY (node_id, revision, activation_id);

-- CONSTRAINT: mtls_user_identities mtls_identity_ca_unique

ALTER TABLE ONLY mtls_user_identities
    ADD CONSTRAINT mtls_identity_ca_unique UNIQUE (id, ca_id);

-- CONSTRAINT: mtls_user_identities mtls_identity_credential_unique

ALTER TABLE ONLY mtls_user_identities
    ADD CONSTRAINT mtls_identity_credential_unique UNIQUE (id, credential_id);

-- CONSTRAINT: mtls_node_operations mtls_node_operations_pkey

ALTER TABLE ONLY mtls_node_operations
    ADD CONSTRAINT mtls_node_operations_pkey PRIMARY KEY (job_id);

-- CONSTRAINT: mtls_node_trust mtls_node_trust_pkey

ALTER TABLE ONLY mtls_node_trust
    ADD CONSTRAINT mtls_node_trust_pkey PRIMARY KEY (node_id, ca_id, ca_version);

-- CONSTRAINT: mtls_operations mtls_operations_pkey

ALTER TABLE ONLY mtls_operations
    ADD CONSTRAINT mtls_operations_pkey PRIMARY KEY (actor_id, idempotency_key);

-- CONSTRAINT: mtls_probe_versions mtls_probe_versions_pkey

ALTER TABLE ONLY mtls_probe_versions
    ADD CONSTRAINT mtls_probe_versions_pkey PRIMARY KEY (node_id, credential_id, credential_version);

-- CONSTRAINT: mtls_rotation_nodes mtls_rotation_nodes_pkey

ALTER TABLE ONLY mtls_rotation_nodes
    ADD CONSTRAINT mtls_rotation_nodes_pkey PRIMARY KEY (rotation_id, node_id);

-- CONSTRAINT: mtls_rotations mtls_rotations_ca_id_ca_version_key

ALTER TABLE ONLY mtls_rotations
    ADD CONSTRAINT mtls_rotations_ca_id_ca_version_key UNIQUE (ca_id, ca_version);

-- CONSTRAINT: mtls_rotations mtls_rotations_pkey

ALTER TABLE ONLY mtls_rotations
    ADD CONSTRAINT mtls_rotations_pkey PRIMARY KEY (id);

-- CONSTRAINT: mtls_trust_cleanup mtls_trust_cleanup_pkey

ALTER TABLE ONLY mtls_trust_cleanup
    ADD CONSTRAINT mtls_trust_cleanup_pkey PRIMARY KEY (ca_id, ca_version);

-- CONSTRAINT: mtls_user_identities mtls_user_identities_credential_id_key

ALTER TABLE ONLY mtls_user_identities
    ADD CONSTRAINT mtls_user_identities_credential_id_key UNIQUE (credential_id);

-- CONSTRAINT: mtls_user_identities mtls_user_identities_pkey

ALTER TABLE ONLY mtls_user_identities
    ADD CONSTRAINT mtls_user_identities_pkey PRIMARY KEY (id);

-- CONSTRAINT: mtls_user_identities mtls_user_identities_user_id_ca_id_key

ALTER TABLE ONLY mtls_user_identities
    ADD CONSTRAINT mtls_user_identities_user_id_ca_id_key UNIQUE (user_id, ca_id);

-- CONSTRAINT: mtls_user_versions mtls_user_versions_pkey

ALTER TABLE ONLY mtls_user_versions
    ADD CONSTRAINT mtls_user_versions_pkey PRIMARY KEY (identity_id, credential_version);

-- CONSTRAINT: node_acme_observations node_acme_observations_pkey

ALTER TABLE ONLY node_acme_observations
    ADD CONSTRAINT node_acme_observations_pkey PRIMARY KEY (node_id);

-- CONSTRAINT: node_alerts node_alerts_node_id_kind_scope_key

ALTER TABLE ONLY node_alerts
    ADD CONSTRAINT node_alerts_node_id_kind_scope_key UNIQUE (node_id, kind, scope);

-- CONSTRAINT: node_alerts node_alerts_pkey

ALTER TABLE ONLY node_alerts
    ADD CONSTRAINT node_alerts_pkey PRIMARY KEY (id);

-- CONSTRAINT: node_assignments node_assignments_node_id_credential_hash_key

ALTER TABLE ONLY node_assignments
    ADD CONSTRAINT node_assignments_node_id_credential_hash_key UNIQUE (node_id, credential_hash);

-- CONSTRAINT: node_assignments node_assignments_pkey

ALTER TABLE ONLY node_assignments
    ADD CONSTRAINT node_assignments_pkey PRIMARY KEY (user_id, node_id);

-- CONSTRAINT: node_authorization_versions node_authorization_versions_pkey

ALTER TABLE ONLY node_authorization_versions
    ADD CONSTRAINT node_authorization_versions_pkey PRIMARY KEY (node_id);

-- CONSTRAINT: node_limit_epochs node_limit_epochs_pkey

ALTER TABLE ONLY node_limit_epochs
    ADD CONSTRAINT node_limit_epochs_pkey PRIMARY KEY (node_id);

-- CONSTRAINT: node_network_samples node_network_samples_pkey

ALTER TABLE ONLY node_network_samples
    ADD CONSTRAINT node_network_samples_pkey PRIMARY KEY (id);

-- CONSTRAINT: node_packages node_packages_pkey

ALTER TABLE ONLY node_packages
    ADD CONSTRAINT node_packages_pkey PRIMARY KEY (node_id);

-- CONSTRAINT: nodes nodes_node_token_hash_key

ALTER TABLE ONLY nodes
    ADD CONSTRAINT nodes_node_token_hash_key UNIQUE (node_token_hash);

-- CONSTRAINT: nodes nodes_pkey

ALTER TABLE ONLY nodes
    ADD CONSTRAINT nodes_pkey PRIMARY KEY (id);

-- CONSTRAINT: online_cycle_metrics online_cycle_metrics_pkey

ALTER TABLE ONLY online_cycle_metrics
    ADD CONSTRAINT online_cycle_metrics_pkey PRIMARY KEY (cycle_id, scope_node_id);

-- CONSTRAINT: online_dirty_minutes online_dirty_minutes_pkey

ALTER TABLE ONLY online_dirty_minutes
    ADD CONSTRAINT online_dirty_minutes_pkey PRIMARY KEY (scope_node_id, minute);

-- CONSTRAINT: online_minute_totals online_minute_totals_pkey

ALTER TABLE ONLY online_minute_totals
    ADD CONSTRAINT online_minute_totals_pkey PRIMARY KEY (scope_node_id, minute);

-- CONSTRAINT: online_samples online_samples_cycle_id_node_id_key

ALTER TABLE ONLY online_samples
    ADD CONSTRAINT online_samples_cycle_id_node_id_key UNIQUE (cycle_id, node_id);

-- CONSTRAINT: online_samples online_samples_pkey

ALTER TABLE ONLY online_samples
    ADD CONSTRAINT online_samples_pkey PRIMARY KEY (id);

-- CONSTRAINT: probe_minute_totals probe_minute_totals_pkey

ALTER TABLE ONLY probe_minute_totals
    ADD CONSTRAINT probe_minute_totals_pkey PRIMARY KEY (node_id, minute);

-- CONSTRAINT: proxy_probe_samples proxy_probe_samples_node_id_sampled_at_key

ALTER TABLE ONLY proxy_probe_samples
    ADD CONSTRAINT proxy_probe_samples_node_id_sampled_at_key UNIQUE (node_id, sampled_at);

-- CONSTRAINT: proxy_probe_samples proxy_probe_samples_pkey

ALTER TABLE ONLY proxy_probe_samples
    ADD CONSTRAINT proxy_probe_samples_pkey PRIMARY KEY (id);

-- CHECK CONSTRAINT: credential_reference_edges reference_edge_shape

ALTER TABLE credential_reference_edges
    ADD CONSTRAINT reference_edge_shape CHECK ((((kind = 'node'::text) AND (source = ANY (ARRAY['desired'::text, 'deployed'::text])) AND (source_id = node_id) AND (config_version_id IS NULL) AND (config_revision IS NULL)) OR ((kind = 'history'::text) AND (source = 'history'::text) AND (source_id = config_version_id) AND (config_version_id IS NOT NULL) AND (config_revision IS NOT NULL) AND (config_revision > 0)))) NOT VALID;

-- CONSTRAINT: resource_leases resource_leases_pkey

ALTER TABLE ONLY resource_leases
    ADD CONSTRAINT resource_leases_pkey PRIMARY KEY (resource_key);

-- CONSTRAINT: runtime_contract runtime_contract_pkey

ALTER TABLE ONLY runtime_contract
    ADD CONSTRAINT runtime_contract_pkey PRIMARY KEY (id);

-- CONSTRAINT: sampling_cycles sampling_cycles_kind_period_start_key

ALTER TABLE ONLY sampling_cycles
    ADD CONSTRAINT sampling_cycles_kind_period_start_key UNIQUE (kind, period_start);

-- CONSTRAINT: sampling_cycles sampling_cycles_pkey

ALTER TABLE ONLY sampling_cycles
    ADD CONSTRAINT sampling_cycles_pkey PRIMARY KEY (id);

-- CONSTRAINT: service_instances service_instances_pkey

ALTER TABLE ONLY service_instances
    ADD CONSTRAINT service_instances_pkey PRIMARY KEY (instance_id);

-- CONSTRAINT: subscription_credentials subscription_credentials_pkey

ALTER TABLE ONLY subscription_credentials
    ADD CONSTRAINT subscription_credentials_pkey PRIMARY KEY (id);

-- CONSTRAINT: subscription_credentials subscription_credentials_token_hash_key

ALTER TABLE ONLY subscription_credentials
    ADD CONSTRAINT subscription_credentials_token_hash_key UNIQUE (token_hash);

-- CONSTRAINT: subscription_template_initialization subscription_template_initialization_pkey

ALTER TABLE ONLY subscription_template_initialization
    ADD CONSTRAINT subscription_template_initialization_pkey PRIMARY KEY (id);

-- CONSTRAINT: subscription_template_versions subscription_template_versions_pkey

ALTER TABLE ONLY subscription_template_versions
    ADD CONSTRAINT subscription_template_versions_pkey PRIMARY KEY (template_id, version);

-- CONSTRAINT: subscription_templates subscription_templates_pkey

ALTER TABLE ONLY subscription_templates
    ADD CONSTRAINT subscription_templates_pkey PRIMARY KEY (id);

-- CONSTRAINT: traffic_baselines traffic_baselines_pkey

ALTER TABLE ONLY traffic_baselines
    ADD CONSTRAINT traffic_baselines_pkey PRIMARY KEY (node_id, user_id);

-- CONSTRAINT: traffic_dirty_hours traffic_dirty_hours_pkey

ALTER TABLE ONLY traffic_dirty_hours
    ADD CONSTRAINT traffic_dirty_hours_pkey PRIMARY KEY (source, node_id, hour);

-- CONSTRAINT: traffic_hour_totals traffic_hour_totals_pkey

ALTER TABLE ONLY traffic_hour_totals
    ADD CONSTRAINT traffic_hour_totals_pkey PRIMARY KEY (source, node_id, hour);

-- CONSTRAINT: traffic_minute_totals traffic_minute_totals_pkey

ALTER TABLE ONLY traffic_minute_totals
    ADD CONSTRAINT traffic_minute_totals_pkey PRIMARY KEY (source, node_id, minute);

-- CONSTRAINT: traffic_records traffic_records_pkey

ALTER TABLE ONLY traffic_records
    ADD CONSTRAINT traffic_records_pkey PRIMARY KEY (id);

-- CONSTRAINT: traffic_sample_batches traffic_sample_batches_pkey

ALTER TABLE ONLY traffic_sample_batches
    ADD CONSTRAINT traffic_sample_batches_pkey PRIMARY KEY (sample_id);

-- CONSTRAINT: traffic_sample_batches traffic_sample_batches_sample_id_node_id_key

ALTER TABLE ONLY traffic_sample_batches
    ADD CONSTRAINT traffic_sample_batches_sample_id_node_id_key UNIQUE (sample_id, node_id);

-- CONSTRAINT: traffic_sample_batches traffic_sample_batches_sample_order_key

ALTER TABLE ONLY traffic_sample_batches
    ADD CONSTRAINT traffic_sample_batches_sample_order_key UNIQUE (sample_order);

-- CONSTRAINT: traffic_sample_items traffic_sample_items_pkey

ALTER TABLE ONLY traffic_sample_items
    ADD CONSTRAINT traffic_sample_items_pkey PRIMARY KEY (id);

-- CONSTRAINT: traffic_sample_items traffic_sample_items_sample_id_user_id_key

ALTER TABLE ONLY traffic_sample_items
    ADD CONSTRAINT traffic_sample_items_sample_id_user_id_key UNIQUE (sample_id, user_id);

-- CONSTRAINT: user_access_checks user_access_checks_pkey

ALTER TABLE ONLY user_access_checks
    ADD CONSTRAINT user_access_checks_pkey PRIMARY KEY (user_id);

-- CONSTRAINT: user_access_restriction_epochs user_access_restriction_epochs_pkey

ALTER TABLE ONLY user_access_restriction_epochs
    ADD CONSTRAINT user_access_restriction_epochs_pkey PRIMARY KEY (user_id);

-- CONSTRAINT: user_node_usage user_node_usage_pkey

ALTER TABLE ONLY user_node_usage
    ADD CONSTRAINT user_node_usage_pkey PRIMARY KEY (user_id, node_id);

-- CONSTRAINT: users users_pkey

ALTER TABLE ONLY users
    ADD CONSTRAINT users_pkey PRIMARY KEY (id);

-- INDEX: audit_action_page

CREATE INDEX audit_action_page ON audit_records USING btree (action, id);

-- INDEX: audit_actor_page

CREATE INDEX audit_actor_page ON audit_records USING btree (actor, id);

-- INDEX: audit_entity_page

CREATE INDEX audit_entity_page ON audit_records USING btree (entity_id, id);

-- INDEX: audit_records_created_id_idx

CREATE INDEX audit_records_created_id_idx ON audit_records USING btree (created_at DESC, id DESC);

-- INDEX: audit_search_trgm

CREATE INDEX audit_search_trgm ON audit_records USING gin (hysteriax_search_text(VARIADIC ARRAY[id, action, actor, entity_type, entity_id]) public.gin_trgm_ops);

-- INDEX: audit_type_page

CREATE INDEX audit_type_page ON audit_records USING btree (entity_type, id);

-- INDEX: authorization_group_nodes_node_idx

CREATE INDEX authorization_group_nodes_node_idx ON authorization_group_nodes USING btree (node_id, group_id);

-- INDEX: authorization_group_previews_expiry_cursor_idx

CREATE INDEX authorization_group_previews_expiry_cursor_idx ON authorization_group_previews USING btree (expires_at, token);

-- INDEX: authorization_group_users_user_idx

CREATE INDEX authorization_group_users_user_idx ON authorization_group_users USING btree (user_id, group_id);

-- INDEX: authorization_groups_name_idx

CREATE INDEX authorization_groups_name_idx ON authorization_groups USING btree (lower(name), name, id);

-- INDEX: background_work_claim_idx

CREATE INDEX background_work_claim_idx ON background_work USING btree (kind, available_at, id) WHERE (state = ANY (ARRAY['queued'::text, 'running'::text]));

-- INDEX: background_work_expiry_idx

CREATE INDEX background_work_expiry_idx ON background_work USING btree (deadline_at, id) WHERE (state = ANY (ARRAY['queued'::text, 'running'::text]));

-- INDEX: background_work_node_idx

CREATE INDEX background_work_node_idx ON background_work USING btree (node_id, id);

-- INDEX: background_work_rotation_identity_due

CREATE INDEX background_work_rotation_identity_due ON background_work USING btree (kind, ((payload ->> 'rotation_id'::text)), available_at, id) WHERE ((cycle_id IS NULL) AND (node_id IS NULL) AND (state = ANY (ARRAY['queued'::text, 'running'::text])));

-- INDEX: background_work_rotation_status

CREATE INDEX background_work_rotation_status ON background_work USING btree (kind, ((payload ->> 'rotation_id'::text)), state, id) WHERE ((cycle_id IS NULL) AND (node_id IS NULL));

-- INDEX: cache_quota_changes

CREATE INDEX cache_quota_changes ON cache_entity_versions USING btree (bucket, bucket_generation, entity_id) WHERE (kind = 'quota_rank'::text);

-- INDEX: config_resource_reference_identity

CREATE UNIQUE INDEX config_resource_reference_identity ON config_resources USING btree (id, node_id);

-- INDEX: config_resources_node_idx

CREATE INDEX config_resources_node_idx ON config_resources USING btree (node_id, resource_kind);

-- INDEX: config_version_reference_identity

CREATE UNIQUE INDEX config_version_reference_identity ON config_versions USING btree (id, node_id, revision);

-- INDEX: credential_batch_items_job

CREATE INDEX credential_batch_items_job ON credential_batch_items USING btree (job_id);

-- INDEX: credential_kind_page

CREATE INDEX credential_kind_page ON credentials USING btree (lower(kind) COLLATE "C", lower(name) COLLATE "C", name COLLATE "C", id COLLATE "C");

-- INDEX: credential_name_page

CREATE INDEX credential_name_page ON credentials USING btree (lower(name) COLLATE "C", name COLLATE "C", id COLLATE "C");

-- INDEX: credential_name_trgm

CREATE INDEX credential_name_trgm ON credentials USING gin (lower(name) public.gin_trgm_ops);

-- INDEX: credential_reference_history

CREATE INDEX credential_reference_history ON credential_reference_edges USING btree (config_version_id, node_id, config_revision) WHERE (config_version_id IS NOT NULL);

-- INDEX: credential_reference_lookup

CREATE INDEX credential_reference_lookup ON credential_reference_edges USING btree (credential_id, source, node_id);

-- INDEX: credential_reference_node

CREATE INDEX credential_reference_node ON credential_reference_edges USING btree (node_id, kind, source_id);

-- INDEX: data_gaps_node_open_idx

CREATE INDEX data_gaps_node_open_idx ON data_gaps USING btree (node_id, resolved_at);

-- INDEX: dns_batch_items_job

CREATE INDEX dns_batch_items_job ON dns_credential_batch_items USING btree (job_id);

-- INDEX: dns_records_name_type_id_idx

CREATE INDEX dns_records_name_type_id_idx ON dns_records USING btree (name COLLATE "C", record_type COLLATE "C", id);

-- INDEX: dns_records_zone_name

CREATE INDEX dns_records_zone_name ON dns_records USING btree (zone_id, name);

-- INDEX: dns_records_zone_name_type_id_idx

CREATE INDEX dns_records_zone_name_type_id_idx ON dns_records USING btree (zone_id, name COLLATE "C", record_type COLLATE "C", id);

-- INDEX: execution_operations_recheck_page

CREATE INDEX execution_operations_recheck_page ON execution_operations USING btree (operation_id) WHERE (state = ANY (ARRAY['dispatching'::text, 'reconciling'::text, 'needs_attention'::text]));

-- INDEX: execution_operations_recovery_idx

CREATE INDEX execution_operations_recovery_idx ON execution_operations USING btree (work_id, state) WHERE (state = ANY (ARRAY['dispatching'::text, 'reconciling'::text, 'needs_attention'::text]));

-- INDEX: execution_operations_uncertain

CREATE INDEX execution_operations_uncertain ON execution_operations USING btree (resource_key) WHERE (state = ANY (ARRAY['dispatching'::text, 'reconciling'::text, 'needs_attention'::text]));

-- INDEX: execution_operations_work_recheck_page

CREATE INDEX execution_operations_work_recheck_page ON execution_operations USING btree (work_id, operation_id) WHERE (state = ANY (ARRAY['dispatching'::text, 'reconciling'::text, 'needs_attention'::text]));

-- INDEX: execution_rechecks_active

CREATE UNIQUE INDEX execution_rechecks_active ON execution_rechecks USING btree (operation_id) WHERE (state = ANY (ARRAY['prepared'::text, 'dispatching'::text]));

-- INDEX: execution_rechecks_history

CREATE INDEX execution_rechecks_history ON execution_rechecks USING btree (operation_id, created_at, id);

-- INDEX: job_events_job_id_idx

CREATE INDEX job_events_job_id_idx ON job_events USING btree (job_id, id);

-- INDEX: jobs_active_kick_idx

CREATE UNIQUE INDEX jobs_active_kick_idx ON jobs USING btree (node_id, ((payload_json ->> 'user_id'::text))) WHERE ((kind = 'kick'::text) AND (status = ANY (ARRAY['queued'::text, 'running'::text])));

-- INDEX: jobs_created_id_idx

CREATE INDEX jobs_created_id_idx ON jobs USING btree (created_at DESC, id DESC);

-- INDEX: jobs_failed_finished

CREATE INDEX jobs_failed_finished ON jobs USING btree (finished_at, id) WHERE (status = 'failed'::text);

-- INDEX: jobs_kind_page

CREATE INDEX jobs_kind_page ON jobs USING btree (kind, id);

-- INDEX: jobs_node_idx

CREATE INDEX jobs_node_idx ON jobs USING btree (node_id, created_at DESC);

-- INDEX: jobs_node_page

CREATE INDEX jobs_node_page ON jobs USING btree (COALESCE(resource_name, node_name, node_id, ''::text), id);

-- INDEX: jobs_resource_active

CREATE INDEX jobs_resource_active ON jobs USING btree (resource_key) WHERE (status = ANY (ARRAY['queued'::text, 'running'::text]));

-- INDEX: jobs_retry_parent_idx

CREATE UNIQUE INDEX jobs_retry_parent_idx ON jobs USING btree (retry_of_job_id) WHERE (retry_of_job_id IS NOT NULL);

-- INDEX: jobs_running_recovery_idx

CREATE INDEX jobs_running_recovery_idx ON jobs USING btree (updated_at, id) WHERE (status = 'running'::text);

-- INDEX: jobs_search_trgm

CREATE INDEX jobs_search_trgm ON jobs USING gin (hysteriax_search_text(VARIADIC ARRAY[id, kind, stage, status, node_name, node_id, resource_name, resource_id, error_message]) public.gin_trgm_ops);

-- INDEX: jobs_stage_page

CREATE INDEX jobs_stage_page ON jobs USING btree (stage, id);

-- INDEX: jobs_status_available_idx

CREATE INDEX jobs_status_available_idx ON jobs USING btree (status, available_at, created_at);

-- INDEX: jobs_status_page

CREATE INDEX jobs_status_page ON jobs USING btree (status, id);

-- INDEX: kick_requests_node_limit_page_idx

CREATE INDEX kick_requests_node_limit_page_idx ON kick_requests USING btree (node_id, user_id) WHERE ((reasons ? 'node_limit'::text) AND (state <> ALL (ARRAY['completed'::text, 'cancelled'::text])));

-- INDEX: kick_requests_pending_node_idx

CREATE INDEX kick_requests_pending_node_idx ON kick_requests USING btree (node_id, user_id) WHERE (state = ANY (ARRAY['active'::text, 'waiting_recovery'::text]));

-- INDEX: kick_requests_schedule_idx

CREATE INDEX kick_requests_schedule_idx ON kick_requests USING btree (updated_at, node_id, user_id) WHERE (state = ANY (ARRAY['active'::text, 'waiting_recovery'::text]));

-- INDEX: monitoring_probe_tokens_expiry_idx

CREATE INDEX monitoring_probe_tokens_expiry_idx ON monitoring_probe_tokens USING btree (expires_at, node_id);

-- INDEX: mtls_activation_ca

CREATE INDEX mtls_activation_ca ON mtls_activation_trust USING btree (ca_id, ca_version);

-- INDEX: mtls_activation_history

CREATE INDEX mtls_activation_history ON mtls_activations USING btree (node_id, target_revision, generation DESC) WHERE (phase = 'committed'::text);

-- INDEX: mtls_activation_recovery

CREATE INDEX mtls_activation_recovery ON mtls_activations USING btree (node_id, phase) WHERE (phase = ANY (ARRAY['activating'::text, 'verified'::text, 'recovery_required'::text]));

-- INDEX: mtls_authorities_due

CREATE INDEX mtls_authorities_due ON mtls_authorities USING btree (next_action_at, credential_id) WHERE (next_action_at IS NOT NULL);

-- INDEX: mtls_ca_one_preparation

CREATE UNIQUE INDEX mtls_ca_one_preparation ON mtls_ca_generations USING btree (ca_id) WHERE (state = 'preparing'::text);

-- INDEX: mtls_ca_one_publication

CREATE UNIQUE INDEX mtls_ca_one_publication ON mtls_ca_generations USING btree (ca_id) WHERE (state = 'published'::text);

-- INDEX: mtls_deployment_snapshot_history

CREATE INDEX mtls_deployment_snapshot_history ON mtls_deployment_snapshots USING btree (node_id, revision, created_at DESC);

-- INDEX: mtls_identity_schedule_pending

CREATE INDEX mtls_identity_schedule_pending ON mtls_user_identities USING btree (ca_id, scheduled_ca_version, user_id, id) WHERE (NOT archived);

-- INDEX: mtls_node_operations_phase

CREATE INDEX mtls_node_operations_phase ON mtls_node_operations USING btree (ca_id, ca_version, stage, node_id);

-- INDEX: mtls_node_trust_ca

CREATE INDEX mtls_node_trust_ca ON mtls_node_trust USING btree (ca_id, ca_version, node_id);

-- INDEX: mtls_probe_current_scope_idx

CREATE INDEX mtls_probe_current_scope_idx ON mtls_probe_versions USING btree (node_id, ca_id, ca_version, credential_version DESC) WHERE (NOT discarded);

-- INDEX: mtls_probe_versions_ca_expiry

CREATE INDEX mtls_probe_versions_ca_expiry ON mtls_probe_versions USING btree (ca_id, ca_version, expires_at);

-- INDEX: mtls_probe_versions_credential

CREATE INDEX mtls_probe_versions_credential ON mtls_probe_versions USING btree (credential_id, credential_version);

-- INDEX: mtls_probe_versions_due

CREATE INDEX mtls_probe_versions_due ON mtls_probe_versions USING btree (next_action_at, node_id, ca_id, ca_version);

-- INDEX: mtls_probe_versions_expired

CREATE INDEX mtls_probe_versions_expired ON mtls_probe_versions USING btree (expires_at, credential_id, credential_version);

-- INDEX: mtls_rotation_pending_jobs

CREATE INDEX mtls_rotation_pending_jobs ON mtls_node_operations USING btree (rotation_id, node_id, job_id) WHERE (pending_job AND (rotation_id IS NOT NULL));

-- INDEX: mtls_rotations_batch

CREATE INDEX mtls_rotations_batch ON mtls_rotations USING btree (batch_id);

-- INDEX: mtls_rotations_due

CREATE INDEX mtls_rotations_due ON mtls_rotations USING btree (next_action_at, id) WHERE (phase <> 'complete'::text);

-- INDEX: mtls_rotations_serial

CREATE UNIQUE INDEX mtls_rotations_serial ON mtls_rotations USING btree (ca_id) WHERE (phase <> 'complete'::text);

-- INDEX: mtls_trust_cleanup_due

CREATE INDEX mtls_trust_cleanup_due ON mtls_trust_cleanup USING btree (next_action_at, ca_id, ca_version) WHERE (completed_at IS NULL);

-- INDEX: mtls_user_identities_ca

CREATE INDEX mtls_user_identities_ca ON mtls_user_identities USING btree (ca_id, user_id);

-- INDEX: mtls_user_identities_due

CREATE INDEX mtls_user_identities_due ON mtls_user_identities USING btree (next_action_at, id) WHERE ((NOT archived) AND (next_action_at IS NOT NULL));

-- INDEX: mtls_user_pending

CREATE INDEX mtls_user_pending ON mtls_user_identities USING btree (id) WHERE ((NOT archived) AND (next_action_at IS NULL));

-- INDEX: mtls_user_versions_ca_expiry

CREATE INDEX mtls_user_versions_ca_expiry ON mtls_user_versions USING btree (ca_id, ca_version, expires_at);

-- INDEX: mtls_user_versions_credential

CREATE INDEX mtls_user_versions_credential ON mtls_user_versions USING btree (credential_id, credential_version);

-- INDEX: mtls_user_versions_current

CREATE INDEX mtls_user_versions_current ON mtls_user_versions USING btree (identity_id, ca_version, credential_version DESC) WHERE (NOT discarded);

-- INDEX: mtls_user_versions_expired

CREATE INDEX mtls_user_versions_expired ON mtls_user_versions USING btree (expires_at, credential_id, credential_version);

-- INDEX: mtls_user_versions_release_pending

CREATE INDEX mtls_user_versions_release_pending ON mtls_user_versions USING btree (ca_id, ca_version, identity_id, credential_version) WHERE ((NOT released) AND (NOT discarded));

-- INDEX: mtls_user_versions_selection

CREATE INDEX mtls_user_versions_selection ON mtls_user_versions USING btree (identity_id, ca_version, credential_version DESC) WHERE (released AND (NOT discarded));

-- INDEX: node_network_samples_node_cleanup_idx

CREATE INDEX node_network_samples_node_cleanup_idx ON node_network_samples USING btree (node_id, sampled_at DESC, id DESC);

-- INDEX: node_network_samples_node_idx

CREATE INDEX node_network_samples_node_idx ON node_network_samples USING btree (node_id, sampled_at);

-- INDEX: node_network_samples_time_idx

CREATE INDEX node_network_samples_time_idx ON node_network_samples USING btree (sampled_at);

-- INDEX: nodes_monitoring_search_trgm

CREATE INDEX nodes_monitoring_search_trgm ON nodes USING gin (hysteriax_search_text(VARIADIC ARRAY[id, name, public_host, ssh_host]) public.gin_trgm_ops);

-- INDEX: nodes_name_id_idx

CREATE INDEX nodes_name_id_idx ON nodes USING btree (lower(name) COLLATE "C", name COLLATE "C", id);

-- INDEX: online_cycle_window

CREATE INDEX online_cycle_window ON online_cycle_metrics USING btree (scope_node_id, started_at);

-- INDEX: online_samples_cycle_time_idx

CREATE INDEX online_samples_cycle_time_idx ON online_samples USING btree (cycle_started_at);

-- INDEX: online_samples_node_cleanup_idx

CREATE INDEX online_samples_node_cleanup_idx ON online_samples USING btree (node_id, sampled_at DESC, id DESC);

-- INDEX: online_samples_node_latest_idx

CREATE INDEX online_samples_node_latest_idx ON online_samples USING btree (node_id, sampled_at DESC NULLS LAST, id DESC);

-- INDEX: online_samples_node_time_idx

CREATE INDEX online_samples_node_time_idx ON online_samples USING btree (node_id, sampled_at DESC);

-- INDEX: online_samples_retention_idx

CREATE INDEX online_samples_retention_idx ON online_samples USING btree (sampled_at, id);

-- INDEX: online_samples_time_idx

CREATE INDEX online_samples_time_idx ON online_samples USING btree (sampled_at);

-- INDEX: probe_minute_window

CREATE INDEX probe_minute_window ON probe_minute_totals USING btree (minute, node_id);

-- INDEX: proxy_probe_samples_node_cleanup_idx

CREATE INDEX proxy_probe_samples_node_cleanup_idx ON proxy_probe_samples USING btree (node_id, sampled_at DESC, id DESC);

-- INDEX: proxy_probe_samples_node_latest_idx

CREATE INDEX proxy_probe_samples_node_latest_idx ON proxy_probe_samples USING btree (node_id, sampled_at DESC NULLS LAST);

-- INDEX: proxy_probe_samples_node_time_idx

CREATE INDEX proxy_probe_samples_node_time_idx ON proxy_probe_samples USING btree (node_id, sampled_at DESC);

-- INDEX: proxy_probe_samples_retention_idx

CREATE INDEX proxy_probe_samples_retention_idx ON proxy_probe_samples USING btree (sampled_at, id);

-- INDEX: proxy_probe_samples_time_idx

CREATE INDEX proxy_probe_samples_time_idx ON proxy_probe_samples USING btree (sampled_at);

-- INDEX: resource_leases_expiry

CREATE INDEX resource_leases_expiry ON resource_leases USING btree (expires_at) WHERE (owner IS NOT NULL);

-- INDEX: resource_leases_work_expiry_idx

CREATE INDEX resource_leases_work_expiry_idx ON resource_leases USING btree (work_id, expires_at) WHERE (work_id IS NOT NULL);

-- INDEX: resource_reference_history

CREATE INDEX resource_reference_history ON config_resource_references USING btree (config_version_id, node_id, config_revision) WHERE (config_version_id IS NOT NULL);

-- INDEX: resource_reference_lookup

CREATE INDEX resource_reference_lookup ON config_resource_references USING btree (resource_id, node_id);

-- INDEX: resource_reference_node

CREATE INDEX resource_reference_node ON config_resource_references USING btree (node_id, kind, source_id);

-- INDEX: retry_chain_failure_window

CREATE INDEX retry_chain_failure_window ON job_retry_chain_state USING btree (failed_seed_at) WHERE (status <> 'succeeded'::text);

-- INDEX: retry_chain_members_root

CREATE INDEX retry_chain_members_root ON job_retry_chain_members USING btree (root_id);

-- INDEX: sampling_cycles_retention_idx

CREATE INDEX sampling_cycles_retention_idx ON sampling_cycles USING btree (period_start, id);

-- INDEX: subscription_templates_default

CREATE UNIQUE INDEX subscription_templates_default ON subscription_templates USING btree (format) WHERE is_default;

-- INDEX: traffic_dirty_node_keys

CREATE INDEX traffic_dirty_node_keys ON traffic_dirty_hours USING btree (node_id, source, hour);

-- INDEX: traffic_hour_window

CREATE INDEX traffic_hour_window ON traffic_hour_totals USING btree (source, hour, node_id);

-- INDEX: traffic_minute_window

CREATE INDEX traffic_minute_window ON traffic_minute_totals USING btree (source, minute, node_id);

-- INDEX: traffic_records_node_cleanup_idx

CREATE INDEX traffic_records_node_cleanup_idx ON traffic_records USING btree (node_id, sampled_at DESC, id DESC);

-- INDEX: traffic_records_node_time_idx

CREATE INDEX traffic_records_node_time_idx ON traffic_records USING btree (node_id, sampled_at DESC);

-- INDEX: traffic_records_sample_user

CREATE UNIQUE INDEX traffic_records_sample_user ON traffic_records USING btree (sample_id, user_id) WHERE (sample_id IS NOT NULL);

-- INDEX: traffic_records_time_idx

CREATE INDEX traffic_records_time_idx ON traffic_records USING btree (sampled_at);

-- INDEX: traffic_records_user_node_time_idx

CREATE INDEX traffic_records_user_node_time_idx ON traffic_records USING btree (user_id, node_id, sampled_at DESC);

-- INDEX: traffic_sample_batches_capture_deadline

CREATE INDEX traffic_sample_batches_capture_deadline ON traffic_sample_batches USING btree (capture_deadline) WHERE (state = 'capturing'::text);

-- INDEX: traffic_sample_batches_capture_expiry_idx

CREATE INDEX traffic_sample_batches_capture_expiry_idx ON traffic_sample_batches USING btree (capture_deadline, sample_id) WHERE (state = 'capturing'::text);

-- INDEX: traffic_sample_batches_cleanup

CREATE INDEX traffic_sample_batches_cleanup ON traffic_sample_batches USING btree (node_id, sampled_at DESC, sample_id DESC);

-- INDEX: traffic_sample_batches_completed_node

CREATE INDEX traffic_sample_batches_completed_node ON traffic_sample_batches USING btree (node_id, sample_order DESC) WHERE (state = 'complete'::text);

-- INDEX: traffic_sample_batches_node_pending

CREATE INDEX traffic_sample_batches_node_pending ON traffic_sample_batches USING btree (node_id, sample_order) WHERE (state = ANY (ARRAY['capturing'::text, 'ready'::text]));

-- INDEX: traffic_sample_batches_pending

CREATE INDEX traffic_sample_batches_pending ON traffic_sample_batches USING btree (sample_order) WHERE (state = 'ready'::text);

-- INDEX: traffic_sample_batches_unfinished_order_idx

CREATE INDEX traffic_sample_batches_unfinished_order_idx ON traffic_sample_batches USING btree (sample_order) WHERE (state = ANY (ARRAY['capturing'::text, 'ready'::text]));

-- INDEX: traffic_sample_items_cleanup

CREATE INDEX traffic_sample_items_cleanup ON traffic_sample_items USING btree (node_id, sampled_at DESC, id DESC);

-- INDEX: traffic_sample_items_pending

CREATE INDEX traffic_sample_items_pending ON traffic_sample_items USING btree (sample_id, user_id) WHERE (processed_at IS NULL);

-- INDEX: user_access_checks_due_idx

CREATE INDEX user_access_checks_due_idx ON user_access_checks USING btree (next_action_at, user_id);

-- INDEX: user_node_usage_node_cleanup_idx

CREATE INDEX user_node_usage_node_cleanup_idx ON user_node_usage USING btree (node_id, sampled_at DESC, user_id DESC);

-- INDEX: users_name_id_idx

CREATE INDEX users_name_id_idx ON users USING btree (lower(name) COLLATE "C", name COLLATE "C", id);

-- TRIGGER: audit_records audit_count_delete

CREATE TRIGGER audit_count_delete AFTER DELETE ON audit_records REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_audit_deleted();

-- TRIGGER: audit_records audit_count_insert

CREATE TRIGGER audit_count_insert AFTER INSERT ON audit_records REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_audit_inserted();

-- TRIGGER: audit_records audit_count_truncate

CREATE TRIGGER audit_count_truncate AFTER TRUNCATE ON audit_records FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_audit_truncated();

-- TRIGGER: cache_flush_pending cache_commit_versions

CREATE CONSTRAINT TRIGGER cache_commit_versions AFTER INSERT ON cache_flush_pending DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_flush();

-- TRIGGER: admin_tokens cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON admin_tokens FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('', 'true');

-- TRIGGER: audit_records cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON audit_records FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('audit', 'false');

-- TRIGGER: authorization_group_nodes cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON authorization_group_nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('groups,node_config', 'true');

-- TRIGGER: authorization_group_users cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON authorization_group_users FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('groups,user_config', 'true');

-- TRIGGER: authorization_groups cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON authorization_groups FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('groups', 'true');

-- TRIGGER: config_resources cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON config_resources FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('resources', 'true');

-- TRIGGER: config_versions cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON config_versions FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('node_config,credentials', 'true');

-- TRIGGER: credential_batch_items cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON credential_batch_items FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials', 'true');

-- TRIGGER: credential_batches cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON credential_batches FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials', 'true');

-- TRIGGER: credential_versions cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON credential_versions FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials', 'true');

-- TRIGGER: credentials cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON credentials FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials', 'true');

-- TRIGGER: data_gaps cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON data_gaps FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('gaps,traffic', 'false');

-- TRIGGER: dns_bindings cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON dns_bindings FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('dns,node_config', 'true');

-- TRIGGER: dns_connections cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON dns_connections FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('dns', 'true');

-- TRIGGER: dns_credential_batch_items cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON dns_credential_batch_items FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials,dns', 'true');

-- TRIGGER: dns_records cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON dns_records FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('dns', 'true');

-- TRIGGER: dns_zones cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON dns_zones FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('dns', 'true');

-- TRIGGER: job_events cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON job_events FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('jobs', 'false');

-- TRIGGER: jobs cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('jobs', 'false');

-- TRIGGER: monitoring_probe_state cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON monitoring_probe_state FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('probe', 'false');

-- TRIGGER: mtls_activation_trust cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_activation_trust FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('node_config,user_config,credentials', 'false');

-- TRIGGER: mtls_activations cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_activations FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('node_config,user_config,credentials', 'false');

-- TRIGGER: mtls_active_probes cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_active_probes FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('node_config,user_config,credentials,groups', 'false');

-- TRIGGER: mtls_authorities cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_authorities FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials,user_config,node_config,groups', 'false');

-- TRIGGER: mtls_ca_generations cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_ca_generations FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials,user_config,node_config,groups', 'false');

-- TRIGGER: mtls_cleanup_nodes cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_cleanup_nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('node_config,user_config,credentials,groups', 'false');

-- TRIGGER: mtls_deployment_snapshots cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_deployment_snapshots FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('node_config,user_config,credentials,groups', 'false');

-- TRIGGER: mtls_node_operations cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_node_operations FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('node_config,user_config,credentials,groups', 'false');

-- TRIGGER: mtls_node_trust cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_node_trust FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials,node_config,user_config,groups', 'false');

-- TRIGGER: mtls_probe_versions cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_probe_versions FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials,node_config', 'false');

-- TRIGGER: mtls_rotation_nodes cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_rotation_nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials,node_config,groups', 'false');

-- TRIGGER: mtls_rotations cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_rotations FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials,node_config,groups', 'false');

-- TRIGGER: mtls_user_identities cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_user_identities FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials,user_config,groups', 'false');

-- TRIGGER: mtls_user_versions cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON mtls_user_versions FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials,user_config,groups', 'false');

-- TRIGGER: node_alerts cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON node_alerts FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('nodes', 'false');

-- TRIGGER: node_assignments cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON node_assignments FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('user_config,node_config,credentials', 'true');

-- TRIGGER: node_network_samples cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON node_network_samples FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('network', 'false');

-- TRIGGER: node_packages cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON node_packages FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('', 'true');

-- TRIGGER: nodes cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('', 'true');

-- TRIGGER: online_samples cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON online_samples FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('online', 'false');

-- TRIGGER: proxy_probe_samples cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON proxy_probe_samples FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('probe', 'false');

-- TRIGGER: subscription_credentials cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON subscription_credentials FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('credentials,user_config', 'true');

-- TRIGGER: traffic_baselines cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON traffic_baselines FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('traffic', 'false');

-- TRIGGER: traffic_records cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON traffic_records FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('traffic', 'false');

-- TRIGGER: users cache_row_changed

CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON users FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('', 'true');

-- TRIGGER: admin_tokens cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON admin_tokens FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: audit_records cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON audit_records FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: authorization_group_nodes cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON authorization_group_nodes FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: authorization_group_users cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON authorization_group_users FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: authorization_groups cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON authorization_groups FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: config_resources cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON config_resources FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: config_versions cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON config_versions FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: credential_batch_items cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON credential_batch_items FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: credential_batches cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON credential_batches FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: credential_versions cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON credential_versions FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: credentials cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON credentials FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: data_gaps cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON data_gaps FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: dns_bindings cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON dns_bindings FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: dns_connections cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON dns_connections FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: dns_credential_batch_items cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON dns_credential_batch_items FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: dns_records cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON dns_records FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: dns_zones cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON dns_zones FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: job_events cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON job_events FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: jobs cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON jobs FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: monitoring_probe_state cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON monitoring_probe_state FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_activation_trust cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_activation_trust FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_activations cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_activations FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_active_probes cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_active_probes FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_authorities cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_authorities FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_ca_generations cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_ca_generations FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_cleanup_nodes cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_cleanup_nodes FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_deployment_snapshots cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_deployment_snapshots FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_node_operations cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_node_operations FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_node_trust cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_node_trust FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_probe_versions cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_probe_versions FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_rotation_nodes cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_rotation_nodes FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_rotations cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_rotations FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_user_identities cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_user_identities FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: mtls_user_versions cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON mtls_user_versions FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: node_alerts cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON node_alerts FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: node_assignments cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON node_assignments FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: node_network_samples cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON node_network_samples FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: node_packages cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON node_packages FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: nodes cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON nodes FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: online_samples cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON online_samples FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: proxy_probe_samples cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON proxy_probe_samples FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: subscription_credentials cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON subscription_credentials FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: traffic_baselines cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON traffic_baselines FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: traffic_records cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON traffic_records FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: users cache_truncated

CREATE TRIGGER cache_truncated AFTER TRUNCATE ON users FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_truncated();

-- TRIGGER: online_samples cycle_dirty

CREATE TRIGGER cycle_dirty AFTER INSERT OR DELETE OR UPDATE ON online_samples FOR EACH ROW EXECUTE FUNCTION hysteriax_cycle_dirty();

-- TRIGGER: job_events event_publication_pending

CREATE TRIGGER event_publication_pending AFTER INSERT ON job_events FOR EACH ROW EXECUTE FUNCTION hysteriax_event_pending();

-- TRIGGER: node_network_samples flow_totals

CREATE TRIGGER flow_totals AFTER INSERT OR DELETE OR UPDATE ON node_network_samples FOR EACH ROW EXECUTE FUNCTION hysteriax_flow_totals();

-- TRIGGER: traffic_records flow_totals

CREATE TRIGGER flow_totals AFTER INSERT OR DELETE OR UPDATE ON traffic_records FOR EACH ROW EXECUTE FUNCTION hysteriax_flow_totals();

-- TRIGGER: traffic_minute_totals hour_dirty

CREATE TRIGGER hour_dirty AFTER INSERT OR DELETE OR UPDATE ON traffic_minute_totals FOR EACH ROW EXECUTE FUNCTION hysteriax_hour_dirty();

-- TRIGGER: credential_versions immutable_credential_version

CREATE TRIGGER immutable_credential_version BEFORE UPDATE ON credential_versions FOR EACH ROW EXECUTE FUNCTION hysteriax_immutable_credential_version();

-- TRIGGER: jobs job_totals_changed

CREATE TRIGGER job_totals_changed AFTER INSERT OR DELETE OR UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION hysteriax_job_totals_changed();

-- TRIGGER: management_receipts management_receipt_complete

CREATE CONSTRAINT TRIGGER management_receipt_complete AFTER INSERT OR UPDATE ON management_receipts DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION hysteriax_management_receipt_complete();

-- TRIGGER: jobs mtls_job_pending

CREATE TRIGGER mtls_job_pending AFTER UPDATE OF status ON jobs FOR EACH ROW WHEN ((old.status IS DISTINCT FROM new.status)) EXECUTE FUNCTION hysteriax_mtls_job_pending();

-- TRIGGER: mtls_node_operations mtls_operation_pending_job

CREATE TRIGGER mtls_operation_pending_job BEFORE INSERT OR UPDATE OF job_id ON mtls_node_operations FOR EACH ROW EXECUTE FUNCTION hysteriax_mtls_operation_pending_job();

-- TRIGGER: node_assignments node_authorization_changed

CREATE TRIGGER node_authorization_changed AFTER INSERT OR DELETE OR UPDATE ON node_assignments FOR EACH ROW EXECUTE FUNCTION hysteriax_node_authorization_changed();

-- TRIGGER: nodes node_authorization_created

CREATE TRIGGER node_authorization_created AFTER INSERT ON nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_node_authorization_created();

-- TRIGGER: node_assignments node_authorizations_truncated

CREATE TRIGGER node_authorizations_truncated AFTER TRUNCATE ON node_assignments FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_node_authorizations_truncated();

-- TRIGGER: online_cycle_metrics online_minute_dirty

CREATE TRIGGER online_minute_dirty AFTER INSERT OR DELETE OR UPDATE ON online_cycle_metrics FOR EACH ROW EXECUTE FUNCTION hysteriax_online_minute_dirty();

-- TRIGGER: proxy_probe_samples probe_totals

CREATE TRIGGER probe_totals AFTER INSERT OR DELETE OR UPDATE ON proxy_probe_samples FOR EACH ROW EXECUTE FUNCTION hysteriax_probe_totals();

-- TRIGGER: credential_reference_dirty reference_source_transaction

CREATE CONSTRAINT TRIGGER reference_source_transaction AFTER INSERT OR UPDATE ON credential_reference_dirty DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION hysteriax_require_transactional_references();

-- TRIGGER: config_versions refs_dirty_history

CREATE TRIGGER refs_dirty_history AFTER INSERT OR DELETE OR UPDATE ON config_versions FOR EACH ROW EXECUTE FUNCTION hysteriax_reference_dirty();

-- TRIGGER: nodes refs_dirty_insert

CREATE TRIGGER refs_dirty_insert AFTER INSERT OR DELETE ON nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_reference_dirty();

-- TRIGGER: nodes refs_dirty_update

CREATE TRIGGER refs_dirty_update AFTER UPDATE OF desired_config_enc, deployed_config_enc ON nodes FOR EACH ROW WHEN (((old.desired_config_enc IS DISTINCT FROM new.desired_config_enc) OR (old.deployed_config_enc IS DISTINCT FROM new.deployed_config_enc))) EXECUTE FUNCTION hysteriax_reference_dirty();

-- TRIGGER: config_versions resource_source_deleted

CREATE TRIGGER resource_source_deleted AFTER DELETE ON config_versions FOR EACH ROW EXECUTE FUNCTION hysteriax_resource_source_deleted();

-- TRIGGER: nodes resource_source_deleted

CREATE TRIGGER resource_source_deleted AFTER DELETE ON nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_resource_source_deleted();

-- TRIGGER: jobs retry_state_changed

CREATE TRIGGER retry_state_changed AFTER INSERT OR DELETE OR UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION hysteriax_retry_state_changed();

-- TRIGGER: users schedule_changed_user_access_check

CREATE TRIGGER schedule_changed_user_access_check AFTER UPDATE ON users FOR EACH ROW WHEN ((((((old.enabled IS DISTINCT FROM new.enabled) OR (old.expires_at IS DISTINCT FROM new.expires_at)) OR (old.quota_bytes IS DISTINCT FROM new.quota_bytes)) OR (old.access_kick_enqueued_at IS DISTINCT FROM new.access_kick_enqueued_at)) OR (((old.quota_bytes IS NOT NULL) AND (old.usage_bytes >= old.quota_bytes)) IS DISTINCT FROM ((new.quota_bytes IS NOT NULL) AND (new.usage_bytes >= new.quota_bytes))))) EXECUTE FUNCTION hysteriax_update_user_access_check();

-- TRIGGER: users schedule_new_user_access_check

CREATE TRIGGER schedule_new_user_access_check AFTER INSERT ON users FOR EACH ROW EXECUTE FUNCTION hysteriax_update_user_access_check();

-- TRIGGER: users track_access_restriction_epoch

CREATE TRIGGER track_access_restriction_epoch AFTER UPDATE OF access_kick_enqueued_at ON users FOR EACH ROW WHEN ((old.access_kick_enqueued_at IS DISTINCT FROM new.access_kick_enqueued_at)) EXECUTE FUNCTION hysteriax_track_access_restriction_epoch();

-- FK CONSTRAINT: node_assignments assignment_mtls_credential_fkey

ALTER TABLE ONLY node_assignments
    ADD CONSTRAINT assignment_mtls_credential_fkey FOREIGN KEY (mtls_credential_id, mtls_credential_version) REFERENCES credential_versions(credential_id, version);

-- FK CONSTRAINT: authorization_group_nodes authorization_group_nodes_group_id_fkey

ALTER TABLE ONLY authorization_group_nodes
    ADD CONSTRAINT authorization_group_nodes_group_id_fkey FOREIGN KEY (group_id) REFERENCES authorization_groups(id) ON DELETE CASCADE;

-- FK CONSTRAINT: authorization_group_nodes authorization_group_nodes_node_id_fkey

ALTER TABLE ONLY authorization_group_nodes
    ADD CONSTRAINT authorization_group_nodes_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: authorization_group_previews authorization_group_previews_group_id_fkey

ALTER TABLE ONLY authorization_group_previews
    ADD CONSTRAINT authorization_group_previews_group_id_fkey FOREIGN KEY (group_id) REFERENCES authorization_groups(id) ON DELETE CASCADE;

-- FK CONSTRAINT: authorization_group_previews authorization_group_previews_user_id_fkey

ALTER TABLE ONLY authorization_group_previews
    ADD CONSTRAINT authorization_group_previews_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

-- FK CONSTRAINT: authorization_group_users authorization_group_users_group_id_fkey

ALTER TABLE ONLY authorization_group_users
    ADD CONSTRAINT authorization_group_users_group_id_fkey FOREIGN KEY (group_id) REFERENCES authorization_groups(id) ON DELETE CASCADE;

-- FK CONSTRAINT: authorization_group_users authorization_group_users_user_id_fkey

ALTER TABLE ONLY authorization_group_users
    ADD CONSTRAINT authorization_group_users_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

-- FK CONSTRAINT: background_work background_work_cycle_id_fkey

ALTER TABLE ONLY background_work
    ADD CONSTRAINT background_work_cycle_id_fkey FOREIGN KEY (cycle_id) REFERENCES sampling_cycles(id);

-- FK CONSTRAINT: cache_bucket_pending cache_bucket_pending_domain_bucket_fkey

ALTER TABLE ONLY cache_bucket_pending
    ADD CONSTRAINT cache_bucket_pending_domain_bucket_fkey FOREIGN KEY (domain, bucket) REFERENCES cache_bucket_versions(domain, bucket);

-- FK CONSTRAINT: config_resource_references config_resource_references_node_id_fkey

ALTER TABLE ONLY config_resource_references
    ADD CONSTRAINT config_resource_references_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: config_resources config_resources_node_id_fkey

ALTER TABLE ONLY config_resources
    ADD CONSTRAINT config_resources_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: config_versions config_versions_node_id_fkey

ALTER TABLE ONLY config_versions
    ADD CONSTRAINT config_versions_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: credential_application_receipts credential_application_receipts_job_id_fkey

ALTER TABLE ONLY credential_application_receipts
    ADD CONSTRAINT credential_application_receipts_job_id_fkey FOREIGN KEY (job_id) REFERENCES jobs(id);

-- FK CONSTRAINT: credential_batch_items credential_batch_items_batch_id_fkey

ALTER TABLE ONLY credential_batch_items
    ADD CONSTRAINT credential_batch_items_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES credential_batches(id) ON DELETE CASCADE;

-- FK CONSTRAINT: credential_batch_items credential_batch_items_job_id_fkey

ALTER TABLE ONLY credential_batch_items
    ADD CONSTRAINT credential_batch_items_job_id_fkey FOREIGN KEY (job_id) REFERENCES jobs(id);

-- FK CONSTRAINT: credential_batches credential_batches_credential_id_fkey

ALTER TABLE ONLY credential_batches
    ADD CONSTRAINT credential_batches_credential_id_fkey FOREIGN KEY (credential_id) REFERENCES credentials(id);

-- FK CONSTRAINT: credential_batches credential_batches_credential_id_version_fkey

ALTER TABLE ONLY credential_batches
    ADD CONSTRAINT credential_batches_credential_id_version_fkey FOREIGN KEY (credential_id, version) REFERENCES credential_versions(credential_id, version);

-- FK CONSTRAINT: credential_reference_edges credential_reference_edges_config_version_id_fkey

ALTER TABLE ONLY credential_reference_edges
    ADD CONSTRAINT credential_reference_edges_config_version_id_fkey FOREIGN KEY (config_version_id) REFERENCES config_versions(id) ON DELETE CASCADE;

-- FK CONSTRAINT: credential_reference_edges credential_reference_edges_node_id_fkey

ALTER TABLE ONLY credential_reference_edges
    ADD CONSTRAINT credential_reference_edges_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: credential_versions credential_versions_credential_id_fkey

ALTER TABLE ONLY credential_versions
    ADD CONSTRAINT credential_versions_credential_id_fkey FOREIGN KEY (credential_id) REFERENCES credentials(id) ON DELETE CASCADE;

-- FK CONSTRAINT: data_gaps data_gaps_node_id_fkey

ALTER TABLE ONLY data_gaps
    ADD CONSTRAINT data_gaps_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: deployment_probe_tokens deployment_probe_tokens_job_id_fkey

ALTER TABLE ONLY deployment_probe_tokens
    ADD CONSTRAINT deployment_probe_tokens_job_id_fkey FOREIGN KEY (job_id) REFERENCES jobs(id);

-- FK CONSTRAINT: deployment_probe_tokens deployment_probe_tokens_node_id_fkey

ALTER TABLE ONLY deployment_probe_tokens
    ADD CONSTRAINT deployment_probe_tokens_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: dns_bindings dns_bindings_node_id_fkey

ALTER TABLE ONLY dns_bindings
    ADD CONSTRAINT dns_bindings_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: dns_bindings dns_bindings_zone_id_fkey

ALTER TABLE ONLY dns_bindings
    ADD CONSTRAINT dns_bindings_zone_id_fkey FOREIGN KEY (zone_id) REFERENCES dns_zones(id);

-- FK CONSTRAINT: dns_connections dns_connections_credential_id_credential_version_fkey

ALTER TABLE ONLY dns_connections
    ADD CONSTRAINT dns_connections_credential_id_credential_version_fkey FOREIGN KEY (credential_id, credential_version) REFERENCES credential_versions(credential_id, version);

-- FK CONSTRAINT: dns_credential_batch_items dns_credential_batch_items_batch_id_fkey

ALTER TABLE ONLY dns_credential_batch_items
    ADD CONSTRAINT dns_credential_batch_items_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES credential_batches(id);

-- FK CONSTRAINT: dns_credential_batch_items dns_credential_batch_items_connection_id_fkey

ALTER TABLE ONLY dns_credential_batch_items
    ADD CONSTRAINT dns_credential_batch_items_connection_id_fkey FOREIGN KEY (connection_id) REFERENCES dns_connections(id);

-- FK CONSTRAINT: dns_credential_batch_items dns_credential_batch_items_job_id_fkey

ALTER TABLE ONLY dns_credential_batch_items
    ADD CONSTRAINT dns_credential_batch_items_job_id_fkey FOREIGN KEY (job_id) REFERENCES jobs(id);

-- FK CONSTRAINT: dns_operations dns_operations_connection_id_fkey

ALTER TABLE ONLY dns_operations
    ADD CONSTRAINT dns_operations_connection_id_fkey FOREIGN KEY (connection_id) REFERENCES dns_connections(id);

-- FK CONSTRAINT: dns_operations dns_operations_job_id_fkey

ALTER TABLE ONLY dns_operations
    ADD CONSTRAINT dns_operations_job_id_fkey FOREIGN KEY (job_id) REFERENCES jobs(id);

-- FK CONSTRAINT: dns_records dns_records_zone_id_fkey

ALTER TABLE ONLY dns_records
    ADD CONSTRAINT dns_records_zone_id_fkey FOREIGN KEY (zone_id) REFERENCES dns_zones(id);

-- FK CONSTRAINT: dns_zones dns_zones_connection_id_fkey

ALTER TABLE ONLY dns_zones
    ADD CONSTRAINT dns_zones_connection_id_fkey FOREIGN KEY (connection_id) REFERENCES dns_connections(id);

-- FK CONSTRAINT: event_publication_pending event_publication_pending_event_id_fkey

ALTER TABLE ONLY event_publication_pending
    ADD CONSTRAINT event_publication_pending_event_id_fkey FOREIGN KEY (event_id) REFERENCES job_events(id) ON DELETE CASCADE;

-- FK CONSTRAINT: event_publications event_publications_event_id_fkey

ALTER TABLE ONLY event_publications
    ADD CONSTRAINT event_publications_event_id_fkey FOREIGN KEY (event_id) REFERENCES job_events(id) ON DELETE CASCADE;

-- FK CONSTRAINT: execution_operations execution_operations_resource_key_fkey

ALTER TABLE ONLY execution_operations
    ADD CONSTRAINT execution_operations_resource_key_fkey FOREIGN KEY (resource_key) REFERENCES resource_leases(resource_key);

-- FK CONSTRAINT: job_events job_events_job_id_fkey

ALTER TABLE ONLY job_events
    ADD CONSTRAINT job_events_job_id_fkey FOREIGN KEY (job_id) REFERENCES jobs(id) ON DELETE CASCADE;

-- FK CONSTRAINT: jobs jobs_retry_of_job_id_fkey

ALTER TABLE ONLY jobs
    ADD CONSTRAINT jobs_retry_of_job_id_fkey FOREIGN KEY (retry_of_job_id) REFERENCES jobs(id);

-- FK CONSTRAINT: kick_requests kick_requests_latest_job_id_fkey

ALTER TABLE ONLY kick_requests
    ADD CONSTRAINT kick_requests_latest_job_id_fkey FOREIGN KEY (latest_job_id) REFERENCES jobs(id);

-- FK CONSTRAINT: kick_requests kick_requests_node_id_fkey

ALTER TABLE ONLY kick_requests
    ADD CONSTRAINT kick_requests_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: monitoring_leases monitoring_leases_node_id_fkey

ALTER TABLE ONLY monitoring_leases
    ADD CONSTRAINT monitoring_leases_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: monitoring_probe_state monitoring_probe_state_node_id_fkey

ALTER TABLE ONLY monitoring_probe_state
    ADD CONSTRAINT monitoring_probe_state_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: monitoring_probe_tokens monitoring_probe_tokens_node_id_fkey

ALTER TABLE ONLY monitoring_probe_tokens
    ADD CONSTRAINT monitoring_probe_tokens_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: mtls_activation_trust mtls_activation_trust_activation_id_fkey

ALTER TABLE ONLY mtls_activation_trust
    ADD CONSTRAINT mtls_activation_trust_activation_id_fkey FOREIGN KEY (activation_id) REFERENCES mtls_activations(id) ON DELETE CASCADE;

-- FK CONSTRAINT: mtls_activation_trust mtls_activation_trust_ca_id_ca_version_fkey

ALTER TABLE ONLY mtls_activation_trust
    ADD CONSTRAINT mtls_activation_trust_ca_id_ca_version_fkey FOREIGN KEY (ca_id, ca_version) REFERENCES mtls_ca_generations(ca_id, ca_version);

-- FK CONSTRAINT: mtls_activations mtls_activations_job_id_fkey

ALTER TABLE ONLY mtls_activations
    ADD CONSTRAINT mtls_activations_job_id_fkey FOREIGN KEY (job_id) REFERENCES jobs(id);

-- FK CONSTRAINT: mtls_activations mtls_activations_previous_activation_id_fkey

ALTER TABLE ONLY mtls_activations
    ADD CONSTRAINT mtls_activations_previous_activation_id_fkey FOREIGN KEY (previous_activation_id) REFERENCES mtls_activations(id);

-- FK CONSTRAINT: mtls_activations mtls_activations_probe_credential_id_probe_credential_vers_fkey

ALTER TABLE ONLY mtls_activations
    ADD CONSTRAINT mtls_activations_probe_credential_id_probe_credential_vers_fkey FOREIGN KEY (probe_credential_id, probe_credential_version) REFERENCES credential_versions(credential_id, version);

-- FK CONSTRAINT: mtls_active_probes mtls_active_probes_ca_id_ca_version_fkey

ALTER TABLE ONLY mtls_active_probes
    ADD CONSTRAINT mtls_active_probes_ca_id_ca_version_fkey FOREIGN KEY (ca_id, ca_version) REFERENCES mtls_ca_generations(ca_id, ca_version);

-- FK CONSTRAINT: mtls_active_probes mtls_active_probes_node_id_credential_id_credential_versio_fkey

ALTER TABLE ONLY mtls_active_probes
    ADD CONSTRAINT mtls_active_probes_node_id_credential_id_credential_versio_fkey FOREIGN KEY (node_id, credential_id, credential_version) REFERENCES mtls_probe_versions(node_id, credential_id, credential_version);

-- FK CONSTRAINT: mtls_active_probes mtls_active_probes_node_id_fkey

ALTER TABLE ONLY mtls_active_probes
    ADD CONSTRAINT mtls_active_probes_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: mtls_authorities mtls_authorities_credential_id_fkey

ALTER TABLE ONLY mtls_authorities
    ADD CONSTRAINT mtls_authorities_credential_id_fkey FOREIGN KEY (credential_id) REFERENCES credentials(id) ON DELETE CASCADE;

-- FK CONSTRAINT: mtls_authorities mtls_authorities_credential_id_published_version_fkey

ALTER TABLE ONLY mtls_authorities
    ADD CONSTRAINT mtls_authorities_credential_id_published_version_fkey FOREIGN KEY (credential_id, published_version) REFERENCES credential_versions(credential_id, version);

-- FK CONSTRAINT: mtls_ca_generations mtls_ca_generations_ca_id_ca_version_fkey

ALTER TABLE ONLY mtls_ca_generations
    ADD CONSTRAINT mtls_ca_generations_ca_id_ca_version_fkey FOREIGN KEY (ca_id, ca_version) REFERENCES credential_versions(credential_id, version);

-- FK CONSTRAINT: mtls_ca_generations mtls_ca_generations_ca_id_fkey

ALTER TABLE ONLY mtls_ca_generations
    ADD CONSTRAINT mtls_ca_generations_ca_id_fkey FOREIGN KEY (ca_id) REFERENCES mtls_authorities(credential_id) ON DELETE CASCADE;

-- FK CONSTRAINT: mtls_cleanup_nodes mtls_cleanup_nodes_ca_id_ca_version_fkey

ALTER TABLE ONLY mtls_cleanup_nodes
    ADD CONSTRAINT mtls_cleanup_nodes_ca_id_ca_version_fkey FOREIGN KEY (ca_id, ca_version) REFERENCES mtls_trust_cleanup(ca_id, ca_version);

-- FK CONSTRAINT: mtls_cleanup_nodes mtls_cleanup_nodes_job_id_fkey

ALTER TABLE ONLY mtls_cleanup_nodes
    ADD CONSTRAINT mtls_cleanup_nodes_job_id_fkey FOREIGN KEY (job_id) REFERENCES jobs(id);

-- FK CONSTRAINT: mtls_cleanup_nodes mtls_cleanup_nodes_node_id_fkey

ALTER TABLE ONLY mtls_cleanup_nodes
    ADD CONSTRAINT mtls_cleanup_nodes_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: mtls_deployment_snapshots mtls_deployment_snapshots_activation_id_fkey

ALTER TABLE ONLY mtls_deployment_snapshots
    ADD CONSTRAINT mtls_deployment_snapshots_activation_id_fkey FOREIGN KEY (activation_id) REFERENCES mtls_activations(id);

-- FK CONSTRAINT: mtls_user_identities mtls_identity_scheduled_generation

ALTER TABLE ONLY mtls_user_identities
    ADD CONSTRAINT mtls_identity_scheduled_generation FOREIGN KEY (ca_id, scheduled_ca_version) REFERENCES mtls_ca_generations(ca_id, ca_version);

-- FK CONSTRAINT: mtls_node_operations mtls_node_operations_base_activation_id_fkey

ALTER TABLE ONLY mtls_node_operations
    ADD CONSTRAINT mtls_node_operations_base_activation_id_fkey FOREIGN KEY (base_activation_id) REFERENCES mtls_activations(id);

-- FK CONSTRAINT: mtls_node_operations mtls_node_operations_ca_id_ca_version_fkey

ALTER TABLE ONLY mtls_node_operations
    ADD CONSTRAINT mtls_node_operations_ca_id_ca_version_fkey FOREIGN KEY (ca_id, ca_version) REFERENCES mtls_ca_generations(ca_id, ca_version);

-- FK CONSTRAINT: mtls_node_operations mtls_node_operations_job_id_fkey

ALTER TABLE ONLY mtls_node_operations
    ADD CONSTRAINT mtls_node_operations_job_id_fkey FOREIGN KEY (job_id) REFERENCES jobs(id);

-- FK CONSTRAINT: mtls_node_operations mtls_node_operations_node_id_fkey

ALTER TABLE ONLY mtls_node_operations
    ADD CONSTRAINT mtls_node_operations_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: mtls_node_operations mtls_node_operations_rotation_id_fkey

ALTER TABLE ONLY mtls_node_operations
    ADD CONSTRAINT mtls_node_operations_rotation_id_fkey FOREIGN KEY (rotation_id) REFERENCES mtls_rotations(id);

-- FK CONSTRAINT: mtls_node_trust mtls_node_trust_ca_id_ca_version_fkey

ALTER TABLE ONLY mtls_node_trust
    ADD CONSTRAINT mtls_node_trust_ca_id_ca_version_fkey FOREIGN KEY (ca_id, ca_version) REFERENCES mtls_ca_generations(ca_id, ca_version);

-- FK CONSTRAINT: mtls_node_trust mtls_node_trust_node_id_fkey

ALTER TABLE ONLY mtls_node_trust
    ADD CONSTRAINT mtls_node_trust_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: mtls_probe_versions mtls_probe_versions_ca_id_ca_version_fkey

ALTER TABLE ONLY mtls_probe_versions
    ADD CONSTRAINT mtls_probe_versions_ca_id_ca_version_fkey FOREIGN KEY (ca_id, ca_version) REFERENCES mtls_ca_generations(ca_id, ca_version);

-- FK CONSTRAINT: mtls_probe_versions mtls_probe_versions_credential_id_credential_version_fkey

ALTER TABLE ONLY mtls_probe_versions
    ADD CONSTRAINT mtls_probe_versions_credential_id_credential_version_fkey FOREIGN KEY (credential_id, credential_version) REFERENCES credential_versions(credential_id, version);

-- FK CONSTRAINT: mtls_rotation_nodes mtls_rotation_nodes_job_id_fkey

ALTER TABLE ONLY mtls_rotation_nodes
    ADD CONSTRAINT mtls_rotation_nodes_job_id_fkey FOREIGN KEY (job_id) REFERENCES jobs(id);

-- FK CONSTRAINT: mtls_rotation_nodes mtls_rotation_nodes_node_id_fkey

ALTER TABLE ONLY mtls_rotation_nodes
    ADD CONSTRAINT mtls_rotation_nodes_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: mtls_rotation_nodes mtls_rotation_nodes_rotation_id_fkey

ALTER TABLE ONLY mtls_rotation_nodes
    ADD CONSTRAINT mtls_rotation_nodes_rotation_id_fkey FOREIGN KEY (rotation_id) REFERENCES mtls_rotations(id) ON DELETE CASCADE;

-- FK CONSTRAINT: mtls_rotations mtls_rotations_batch_id_fkey

ALTER TABLE ONLY mtls_rotations
    ADD CONSTRAINT mtls_rotations_batch_id_fkey FOREIGN KEY (batch_id) REFERENCES credential_batches(id);

-- FK CONSTRAINT: mtls_rotations mtls_rotations_ca_id_ca_version_fkey

ALTER TABLE ONLY mtls_rotations
    ADD CONSTRAINT mtls_rotations_ca_id_ca_version_fkey FOREIGN KEY (ca_id, ca_version) REFERENCES mtls_ca_generations(ca_id, ca_version);

-- FK CONSTRAINT: mtls_trust_cleanup mtls_trust_cleanup_ca_id_ca_version_fkey

ALTER TABLE ONLY mtls_trust_cleanup
    ADD CONSTRAINT mtls_trust_cleanup_ca_id_ca_version_fkey FOREIGN KEY (ca_id, ca_version) REFERENCES mtls_ca_generations(ca_id, ca_version);

-- FK CONSTRAINT: mtls_user_identities mtls_user_identities_ca_id_fkey

ALTER TABLE ONLY mtls_user_identities
    ADD CONSTRAINT mtls_user_identities_ca_id_fkey FOREIGN KEY (ca_id) REFERENCES mtls_authorities(credential_id);

-- FK CONSTRAINT: mtls_user_identities mtls_user_identities_credential_id_fkey

ALTER TABLE ONLY mtls_user_identities
    ADD CONSTRAINT mtls_user_identities_credential_id_fkey FOREIGN KEY (credential_id) REFERENCES credentials(id);

-- FK CONSTRAINT: mtls_user_versions mtls_user_versions_ca_id_ca_version_fkey

ALTER TABLE ONLY mtls_user_versions
    ADD CONSTRAINT mtls_user_versions_ca_id_ca_version_fkey FOREIGN KEY (ca_id, ca_version) REFERENCES mtls_ca_generations(ca_id, ca_version);

-- FK CONSTRAINT: mtls_user_versions mtls_user_versions_credential_id_credential_version_fkey

ALTER TABLE ONLY mtls_user_versions
    ADD CONSTRAINT mtls_user_versions_credential_id_credential_version_fkey FOREIGN KEY (credential_id, credential_version) REFERENCES credential_versions(credential_id, version);

-- FK CONSTRAINT: mtls_user_versions mtls_user_versions_identity_id_fkey

ALTER TABLE ONLY mtls_user_versions
    ADD CONSTRAINT mtls_user_versions_identity_id_fkey FOREIGN KEY (identity_id) REFERENCES mtls_user_identities(id) ON DELETE CASCADE;

-- FK CONSTRAINT: mtls_user_versions mtls_version_identity_ca

ALTER TABLE ONLY mtls_user_versions
    ADD CONSTRAINT mtls_version_identity_ca FOREIGN KEY (identity_id, ca_id) REFERENCES mtls_user_identities(id, ca_id);

-- FK CONSTRAINT: mtls_user_versions mtls_version_identity_credential

ALTER TABLE ONLY mtls_user_versions
    ADD CONSTRAINT mtls_version_identity_credential FOREIGN KEY (identity_id, credential_id) REFERENCES mtls_user_identities(id, credential_id);

-- FK CONSTRAINT: node_acme_observations node_acme_observations_node_id_fkey

ALTER TABLE ONLY node_acme_observations
    ADD CONSTRAINT node_acme_observations_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: node_alerts node_alerts_node_id_fkey

ALTER TABLE ONLY node_alerts
    ADD CONSTRAINT node_alerts_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: node_assignments node_assignments_mtls_credential_id_fkey

ALTER TABLE ONLY node_assignments
    ADD CONSTRAINT node_assignments_mtls_credential_id_fkey FOREIGN KEY (mtls_credential_id) REFERENCES credentials(id);

-- FK CONSTRAINT: node_assignments node_assignments_node_id_fkey

ALTER TABLE ONLY node_assignments
    ADD CONSTRAINT node_assignments_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: node_assignments node_assignments_user_id_fkey

ALTER TABLE ONLY node_assignments
    ADD CONSTRAINT node_assignments_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

-- FK CONSTRAINT: node_authorization_versions node_authorization_versions_node_id_fkey

ALTER TABLE ONLY node_authorization_versions
    ADD CONSTRAINT node_authorization_versions_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON UPDATE CASCADE ON DELETE CASCADE;

-- FK CONSTRAINT: node_limit_epochs node_limit_epochs_node_id_fkey

ALTER TABLE ONLY node_limit_epochs
    ADD CONSTRAINT node_limit_epochs_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: node_network_samples node_network_samples_node_id_fkey

ALTER TABLE ONLY node_network_samples
    ADD CONSTRAINT node_network_samples_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: node_packages node_packages_node_id_fkey

ALTER TABLE ONLY node_packages
    ADD CONSTRAINT node_packages_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: nodes nodes_mtls_activation_id_fkey

ALTER TABLE ONLY nodes
    ADD CONSTRAINT nodes_mtls_activation_id_fkey FOREIGN KEY (mtls_activation_id) REFERENCES mtls_activations(id);

-- FK CONSTRAINT: nodes nodes_ssh_credential_fkey

ALTER TABLE ONLY nodes
    ADD CONSTRAINT nodes_ssh_credential_fkey FOREIGN KEY (ssh_credential_id, ssh_credential_version) REFERENCES credential_versions(credential_id, version);

-- FK CONSTRAINT: nodes nodes_ssh_credential_id_fkey

ALTER TABLE ONLY nodes
    ADD CONSTRAINT nodes_ssh_credential_id_fkey FOREIGN KEY (ssh_credential_id) REFERENCES credentials(id);

-- FK CONSTRAINT: online_samples online_samples_node_id_fkey

ALTER TABLE ONLY online_samples
    ADD CONSTRAINT online_samples_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: probe_minute_totals probe_minute_totals_node_id_fkey

ALTER TABLE ONLY probe_minute_totals
    ADD CONSTRAINT probe_minute_totals_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: proxy_probe_samples proxy_probe_samples_node_id_fkey

ALTER TABLE ONLY proxy_probe_samples
    ADD CONSTRAINT proxy_probe_samples_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: credential_reference_edges reference_edge_history

ALTER TABLE ONLY credential_reference_edges
    ADD CONSTRAINT reference_edge_history FOREIGN KEY (config_version_id, node_id, config_revision) REFERENCES config_versions(id, node_id, revision) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED NOT VALID;

-- FK CONSTRAINT: credential_reference_edges reference_edge_version

ALTER TABLE ONLY credential_reference_edges
    ADD CONSTRAINT reference_edge_version FOREIGN KEY (credential_id, version) REFERENCES credential_versions(credential_id, version) NOT VALID;

-- FK CONSTRAINT: config_resource_references resource_reference_history

ALTER TABLE ONLY config_resource_references
    ADD CONSTRAINT resource_reference_history FOREIGN KEY (config_version_id, node_id, config_revision) REFERENCES config_versions(id, node_id, revision) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED;

-- FK CONSTRAINT: config_resource_references resource_reference_resource

ALTER TABLE ONLY config_resource_references
    ADD CONSTRAINT resource_reference_resource FOREIGN KEY (resource_id, node_id) REFERENCES config_resources(id, node_id);

-- FK CONSTRAINT: subscription_credentials subscription_credentials_user_id_fkey

ALTER TABLE ONLY subscription_credentials
    ADD CONSTRAINT subscription_credentials_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

-- FK CONSTRAINT: subscription_templates subscription_template_published

ALTER TABLE ONLY subscription_templates
    ADD CONSTRAINT subscription_template_published FOREIGN KEY (id, published_version) REFERENCES subscription_template_versions(template_id, version) DEFERRABLE INITIALLY DEFERRED;

-- FK CONSTRAINT: subscription_template_versions subscription_template_versions_template_id_fkey

ALTER TABLE ONLY subscription_template_versions
    ADD CONSTRAINT subscription_template_versions_template_id_fkey FOREIGN KEY (template_id) REFERENCES subscription_templates(id);

-- FK CONSTRAINT: traffic_baselines traffic_baselines_node_id_fkey

ALTER TABLE ONLY traffic_baselines
    ADD CONSTRAINT traffic_baselines_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: traffic_baselines traffic_baselines_user_id_fkey

ALTER TABLE ONLY traffic_baselines
    ADD CONSTRAINT traffic_baselines_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

-- FK CONSTRAINT: traffic_hour_totals traffic_hour_totals_node_id_fkey

ALTER TABLE ONLY traffic_hour_totals
    ADD CONSTRAINT traffic_hour_totals_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: traffic_minute_totals traffic_minute_totals_node_id_fkey

ALTER TABLE ONLY traffic_minute_totals
    ADD CONSTRAINT traffic_minute_totals_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: traffic_records traffic_records_node_id_fkey

ALTER TABLE ONLY traffic_records
    ADD CONSTRAINT traffic_records_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: traffic_records traffic_records_user_id_fkey

ALTER TABLE ONLY traffic_records
    ADD CONSTRAINT traffic_records_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

-- FK CONSTRAINT: traffic_sample_batches traffic_sample_batches_node_id_fkey

ALTER TABLE ONLY traffic_sample_batches
    ADD CONSTRAINT traffic_sample_batches_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: traffic_sample_items traffic_sample_items_sample_id_node_id_fkey

ALTER TABLE ONLY traffic_sample_items
    ADD CONSTRAINT traffic_sample_items_sample_id_node_id_fkey FOREIGN KEY (sample_id, node_id) REFERENCES traffic_sample_batches(sample_id, node_id) ON DELETE CASCADE;

-- FK CONSTRAINT: user_access_checks user_access_checks_user_id_fkey

ALTER TABLE ONLY user_access_checks
    ADD CONSTRAINT user_access_checks_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

-- FK CONSTRAINT: user_access_restriction_epochs user_access_restriction_epochs_user_id_fkey

ALTER TABLE ONLY user_access_restriction_epochs
    ADD CONSTRAINT user_access_restriction_epochs_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

-- FK CONSTRAINT: user_node_usage user_node_usage_node_id_fkey

ALTER TABLE ONLY user_node_usage
    ADD CONSTRAINT user_node_usage_node_id_fkey FOREIGN KEY (node_id) REFERENCES nodes(id) ON DELETE CASCADE;

-- FK CONSTRAINT: user_node_usage user_node_usage_user_id_fkey

ALTER TABLE ONLY user_node_usage
    ADD CONSTRAINT user_node_usage_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;

-- Initial singleton rows and cache domains. Generate a distinct epoch per DB.
INSERT INTO audit_totals(id, total) VALUES (true, 0);
INSERT INTO cache_version_epoch(id, epoch) VALUES (true, gen_random_uuid()::text);
INSERT INTO event_publication_stream(id, epoch) VALUES (true, gen_random_uuid()::text);
INSERT INTO cache_bucket_versions(domain, bucket)
SELECT domain, bucket
FROM unnest(ARRAY[
    'audit', 'credentials', 'directory', 'dns', 'gaps', 'groups', 'jobs',
    'network', 'node_config', 'nodes', 'online', 'probe', 'quota_rank',
    'resources', 'traffic', 'user_config', 'users'
]) AS domains(domain)
CROSS JOIN generate_series(0, 63) AS buckets(bucket);
