-- Retire the permanent per-key version directory without reusing an old token.
-- This migration runs with application writers stopped.
DO $$
DECLARE
    queue_name text;
    old_max bigint;
    pending_max bigint;
BEGIN
    FOREACH queue_name IN ARRAY ARRAY[
        'analytics_dirty_cycles', 'traffic_dirty_hours', 'online_dirty_minutes',
        'job_retry_state_dirty', 'credential_reference_dirty'
    ] LOOP
        SELECT coalesce(max(generation), 0) INTO old_max
          FROM dirty_key_versions WHERE queue = queue_name;
        EXECUTE format('SELECT coalesce(max(generation), 0) FROM %I', queue_name)
          INTO pending_max;
        EXECUTE format('CREATE SEQUENCE %I AS bigint CACHE 32 NO CYCLE',
                       queue_name || '_revision_seq');
        IF greatest(old_max, pending_max) > 0 THEN
            PERFORM setval((queue_name || '_revision_seq')::regclass,
                           greatest(old_max, pending_max), true);
        END IF;
    END LOOP;
END $$;

-- Sequence values are unique tokens within a queue. With sequence caching,
-- their numerical order does not indicate transaction or commit order.
CREATE OR REPLACE FUNCTION hysteriax_mark_dirty(queue_name text, keys jsonb) RETURNS void
    LANGUAGE plpgsql
    AS $$
DECLARE revision bigint;
BEGIN
 IF queue_name<>ALL(ARRAY['analytics_dirty_cycles','traffic_dirty_hours','online_dirty_minutes','job_retry_state_dirty','credential_reference_dirty']) THEN RAISE EXCEPTION 'invalid dirty queue'; END IF;
 revision := nextval((queue_name || '_revision_seq')::regclass);
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

CREATE OR REPLACE FUNCTION hysteriax_require_transactional_references() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE pending boolean; source_exists boolean;
BEGIN
 IF current_setting('hysteriax.reference_maintenance',true)='on' THEN RETURN NULL; END IF;
 SELECT EXISTS(SELECT 1 FROM credential_reference_dirty d WHERE d.kind=NEW.kind AND d.source_id=NEW.source_id) INTO pending;
 IF NOT pending THEN RETURN NULL; END IF;
 IF NEW.kind='node' THEN SELECT EXISTS(SELECT 1 FROM nodes WHERE id=NEW.source_id) INTO source_exists;
 ELSE SELECT EXISTS(SELECT 1 FROM config_versions WHERE id=NEW.source_id) INTO source_exists; END IF;
 IF source_exists THEN
  RAISE EXCEPTION 'configuration references must be indexed in the source transaction'
   USING ERRCODE='23514',CONSTRAINT='reference_source_transaction';
 END IF;
 RETURN NULL;
END $$;

DROP TABLE dirty_key_versions;

-- Treat pre-existing publications as newly visible upon installation.
ALTER TABLE event_publications ADD COLUMN published_at timestamptz NOT NULL DEFAULT transaction_timestamp();
ALTER TABLE event_publication_stream ADD COLUMN last_published_at timestamptz NOT NULL DEFAULT transaction_timestamp();

CREATE INDEX traffic_sample_items_history_idx ON traffic_sample_items ((hashtext(node_id) & 3), processed_at, (coalesce(sample_id, '')), id)
    WHERE processed_at IS NOT NULL;
CREATE INDEX traffic_records_history_idx ON traffic_records ((hashtext(node_id) & 3), sampled_at, (coalesce(sample_id, '')), id);
CREATE INDEX node_network_samples_history_idx ON node_network_samples ((hashtext(node_id) & 3), sampled_at, id);
CREATE INDEX traffic_sample_batches_history_idx ON traffic_sample_batches ((hashtext(node_id) & 3), completed_at, sample_id)
    WHERE state = 'complete' AND completed_at IS NOT NULL;
CREATE INDEX data_gaps_history_idx ON data_gaps ((hashtext(node_id) & 3), resolved_at, id) WHERE resolved_at IS NOT NULL;
CREATE INDEX service_instances_history_idx ON service_instances (heartbeat_at, instance_id);
CREATE INDEX deployment_probe_tokens_history_idx ON deployment_probe_tokens (expires_at, node_id);
CREATE INDEX job_events_history_idx ON job_events (created_at, id);
CREATE INDEX jobs_history_idx ON jobs (finished_at, id)
    WHERE retry_of_job_id IS NULL AND status IN ('succeeded', 'failed', 'rolled_back', 'cancelled')
      AND finished_at IS NOT NULL;

-- Keep dependency checks bounded as job and operational history grow.
CREATE INDEX credential_application_receipts_job_idx ON credential_application_receipts (job_id);
CREATE INDEX deployment_probe_tokens_job_idx ON deployment_probe_tokens (job_id);
CREATE INDEX dns_operations_job_idx ON dns_operations (job_id);
CREATE INDEX kick_requests_latest_job_idx ON kick_requests (latest_job_id);
CREATE INDEX mtls_activations_job_idx ON mtls_activations (job_id);
CREATE INDEX mtls_cleanup_nodes_job_idx ON mtls_cleanup_nodes (job_id);
CREATE INDEX mtls_node_operations_job_idx ON mtls_node_operations (job_id);
CREATE INDEX mtls_rotation_nodes_job_idx ON mtls_rotation_nodes (job_id);
CREATE INDEX background_work_active_node_kind_idx ON background_work (node_id, kind)
    WHERE state IN ('queued', 'running');
CREATE INDEX resource_leases_owner_idx ON resource_leases (owner) WHERE owner IS NOT NULL;
CREATE INDEX execution_operations_pending_owner_idx ON execution_operations (owner)
    WHERE state IN ('dispatching', 'reconciling', 'needs_attention');
