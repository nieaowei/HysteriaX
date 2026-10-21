-- A refresh owns only the keys it locked; a later source change cannot be
-- acknowledged by a stale or unrelated batch. Preserve all queued source keys.
ALTER TABLE analytics_dirty_cycles ADD COLUMN generation bigint NOT NULL DEFAULT 1 CHECK(generation>0);
ALTER TABLE traffic_dirty_hours ADD COLUMN generation bigint NOT NULL DEFAULT 1 CHECK(generation>0);
ALTER TABLE online_dirty_minutes ADD COLUMN generation bigint NOT NULL DEFAULT 1 CHECK(generation>0);
ALTER TABLE job_retry_state_dirty ADD COLUMN generation bigint NOT NULL DEFAULT 1 CHECK(generation>0);
ALTER TABLE credential_reference_dirty ADD COLUMN generation bigint NOT NULL DEFAULT 1 CHECK(generation>0);
CREATE TABLE dirty_key_versions(queue text NOT NULL,entity_key text NOT NULL,generation bigint NOT NULL CHECK(generation>0),PRIMARY KEY(queue,entity_key));
INSERT INTO dirty_key_versions SELECT 'analytics_dirty_cycles',(jsonb_build_object('cycle_id',cycle_id))::text,generation FROM analytics_dirty_cycles;
INSERT INTO dirty_key_versions SELECT 'traffic_dirty_hours',(jsonb_build_object('source',source,'node_id',node_id,'hour',extract(epoch FROM hour)))::text,generation FROM traffic_dirty_hours;
INSERT INTO dirty_key_versions SELECT 'online_dirty_minutes',(jsonb_build_object('scope_node_id',scope_node_id,'minute',extract(epoch FROM minute)))::text,generation FROM online_dirty_minutes;
INSERT INTO dirty_key_versions SELECT 'job_retry_state_dirty',(jsonb_build_object('job_id',job_id))::text,generation FROM job_retry_state_dirty;
INSERT INTO dirty_key_versions SELECT 'credential_reference_dirty',(jsonb_build_object('kind',kind,'source_id',source_id))::text,generation FROM credential_reference_dirty;
CREATE FUNCTION hysteriax_mark_dirty(queue_name text,keys jsonb) RETURNS void LANGUAGE plpgsql AS $$
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
CREATE INDEX traffic_dirty_node_keys ON traffic_dirty_hours(node_id,source,hour);
CREATE TABLE analytics_retry_guards(root_id text PRIMARY KEY);
CREATE OR REPLACE FUNCTION hysteriax_cycle_dirty() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE old_id uuid; new_id uuid; item uuid;
BEGIN
 IF TG_OP<>'INSERT' THEN old_id:=OLD.cycle_id; END IF;
 IF TG_OP<>'DELETE' THEN new_id:=NEW.cycle_id; END IF;
 FOR item IN SELECT DISTINCT id FROM unnest(ARRAY[old_id,new_id]) id WHERE id IS NOT NULL ORDER BY id LOOP
  PERFORM hysteriax_mark_dirty('analytics_dirty_cycles',jsonb_build_object('cycle_id',item));
 END LOOP;
 RETURN NULL;
END $$;
CREATE OR REPLACE FUNCTION hysteriax_hour_dirty() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE a_source text; a_node text; a_hour timestamptz; b_source text; b_node text; b_hour timestamptz; item record;
BEGIN
 IF TG_OP<>'INSERT' THEN a_source:=OLD.source; a_node:=OLD.node_id; a_hour:=hysteriax_hour(OLD.minute); END IF;
 IF TG_OP<>'DELETE' THEN b_source:=NEW.source; b_node:=NEW.node_id; b_hour:=hysteriax_hour(NEW.minute); END IF;
 FOR item IN SELECT DISTINCT source,node_id,hour FROM (VALUES(a_source,a_node,a_hour),(b_source,b_node,b_hour)) keys(source,node_id,hour) WHERE source IS NOT NULL ORDER BY source,node_id,hour LOOP
  PERFORM hysteriax_mark_dirty('traffic_dirty_hours',jsonb_build_object('source',item.source,'node_id',item.node_id,'hour',extract(epoch FROM item.hour)));
 END LOOP;
 RETURN NULL;
END $$;
CREATE OR REPLACE FUNCTION hysteriax_online_minute_dirty() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE a_node text; a_minute timestamptz; b_node text; b_minute timestamptz; item record;
BEGIN
 IF TG_OP<>'INSERT' THEN a_node:=OLD.scope_node_id; a_minute:=hysteriax_minute(OLD.started_at); END IF;
 IF TG_OP<>'DELETE' THEN b_node:=NEW.scope_node_id; b_minute:=hysteriax_minute(NEW.started_at); END IF;
 FOR item IN SELECT DISTINCT scope_node_id,minute FROM (VALUES(a_node,a_minute),(b_node,b_minute)) keys(scope_node_id,minute) WHERE scope_node_id IS NOT NULL ORDER BY scope_node_id,minute LOOP
  PERFORM hysteriax_mark_dirty('online_dirty_minutes',jsonb_build_object('scope_node_id',item.scope_node_id,'minute',extract(epoch FROM item.minute)));
 END LOOP;
 RETURN NULL;
END $$;
CREATE OR REPLACE FUNCTION hysteriax_retry_state_changed() RETURNS trigger LANGUAGE plpgsql AS $$
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
CREATE OR REPLACE FUNCTION hysteriax_reference_dirty() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_kind text; ids text[]:=ARRAY[]::text[]; item text;
BEGIN
 source_kind:=CASE WHEN TG_TABLE_NAME='nodes' THEN 'node' ELSE 'history' END;
 IF TG_OP<>'INSERT' THEN ids:=array_append(ids,OLD.id); END IF;
 IF TG_OP<>'DELETE' THEN ids:=array_append(ids,NEW.id); END IF;
 FOR item IN SELECT DISTINCT id FROM unnest(ids) id WHERE id IS NOT NULL ORDER BY id LOOP
  PERFORM hysteriax_mark_dirty('credential_reference_dirty',jsonb_build_object('kind',source_kind,'source_id',item));
 END LOOP;
 RETURN NULL;
END $$;

-- One authoritative computation is shared by refreshers and read-only dirty
-- fallbacks, including cross-node user deduplication and unknown/zero states.
CREATE FUNCTION hysteriax_online_cycle_values(ids uuid[])
RETURNS TABLE(cycle_id uuid,scope_node_id text,started_at timestamptz,users bigint,connections numeric,covered bigint,expected bigint,incomplete boolean,traffic_covered bigint,traffic_incomplete boolean)
LANGUAGE sql STABLE AS $values$
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

$values$;
