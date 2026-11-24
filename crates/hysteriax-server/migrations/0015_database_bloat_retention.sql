-- 1. Drop redundant full-table B-tree indexes on hot tables and upgrade probe latest index for shared use.
DROP INDEX online_samples_node_cleanup_idx;
DROP INDEX online_samples_node_time_idx;
DROP INDEX online_samples_retention_idx;
DROP INDEX proxy_probe_samples_node_cleanup_idx;
DROP INDEX proxy_probe_samples_node_latest_idx;
DROP INDEX proxy_probe_samples_node_time_idx;
DROP INDEX proxy_probe_samples_retention_idx;
DROP INDEX traffic_records_node_time_idx;
DROP INDEX node_network_samples_node_idx;

CREATE INDEX proxy_probe_samples_node_latest_idx
    ON proxy_probe_samples (node_id, sampled_at DESC NULLS LAST, id DESC);

-- 2. Tune table-level autovacuum and fillfactor to prevent dead-tuple and index bloat.
DO $$
DECLARE tbl text;
BEGIN
    FOREACH tbl IN ARRAY ARRAY[
        'traffic_records', 'traffic_sample_items', 'traffic_sample_batches',
        'node_network_samples', 'online_samples', 'proxy_probe_samples',
        'background_work', 'resource_leases', 'traffic_minute_totals',
        'traffic_hour_totals', 'online_cycle_metrics', 'online_minute_totals',
        'probe_minute_totals'
    ] LOOP
        EXECUTE format(
            'ALTER TABLE %I SET (autovacuum_vacuum_scale_factor = 0.01, autovacuum_analyze_scale_factor = 0.02, autovacuum_vacuum_threshold = 5000, autovacuum_vacuum_cost_limit = 2000, autovacuum_vacuum_cost_delay = 2)',
            tbl
        );
    END LOOP;

    FOREACH tbl IN ARRAY ARRAY[
        'cache_bucket_pending', 'cache_entity_pending', 'cache_flush_pending',
        'analytics_dirty_cycles', 'traffic_dirty_hours', 'online_dirty_minutes',
        'job_retry_state_dirty', 'credential_reference_dirty', 'event_publication_pending'
    ] LOOP
        EXECUTE format(
            'ALTER TABLE %I SET (autovacuum_vacuum_scale_factor = 0.0, autovacuum_vacuum_threshold = 500, autovacuum_analyze_scale_factor = 0.0, autovacuum_analyze_threshold = 500, autovacuum_vacuum_cost_limit = 2000, autovacuum_vacuum_cost_delay = 0)',
            tbl
        );
    END LOOP;

    FOREACH tbl IN ARRAY ARRAY[
        'cache_bucket_versions', 'node_packages', 'users', 'nodes', 'monitoring_probe_state'
    ] LOOP
        EXECUTE format('ALTER TABLE %I SET (fillfactor = 80)', tbl);
    END LOOP;
END $$;

-- 3. Convert proxy_probe_samples and online_samples DELETE triggers to statement-level.
DROP TRIGGER probe_totals ON proxy_probe_samples;
CREATE TRIGGER probe_totals AFTER INSERT OR UPDATE ON proxy_probe_samples
    FOR EACH ROW EXECUTE FUNCTION hysteriax_probe_totals();

DROP TRIGGER cache_row_changed ON proxy_probe_samples;
CREATE TRIGGER cache_row_changed AFTER INSERT OR UPDATE ON proxy_probe_samples
    FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('probe', 'false');

CREATE FUNCTION hysteriax_probe_deleted() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE cleanup_node text := current_setting('hysteriax.history_cleanup_node', true);
BEGIN
    IF NOT EXISTS (SELECT 1 FROM removed_probes) THEN
        RETURN NULL;
    END IF;

    INSERT INTO cache_bucket_pending
    SELECT DISTINCT txid_current(), 'probe', hysteriax_cache_bucket(node_id)
    FROM removed_probes ON CONFLICT DO NOTHING;

    INSERT INTO cache_entity_pending
    SELECT DISTINCT txid_current(), 'nodes', node_id
    FROM removed_probes ON CONFLICT DO NOTHING;
    PERFORM hysteriax_cache_queue_flush();

    IF NOT EXISTS (
        SELECT 1 FROM removed_probes r JOIN nodes n ON n.id = r.node_id
        WHERE r.node_id IS DISTINCT FROM cleanup_node
          AND r.status IN ('ok', 'failed')
    ) THEN
        RETURN NULL;
    END IF;

    PERFORM 1
    FROM probe_minute_totals totals
    JOIN (
        SELECT DISTINCT r.node_id, hysteriax_minute(r.sampled_at) AS minute
        FROM removed_probes r JOIN nodes n ON n.id = r.node_id
        WHERE r.node_id IS DISTINCT FROM cleanup_node
          AND r.status IN ('ok', 'failed')
    ) affected USING (node_id, minute)
    ORDER BY totals.node_id, totals.minute
    FOR UPDATE OF totals;

    WITH delta AS MATERIALIZED (
        SELECT r.node_id, hysteriax_minute(r.sampled_at) AS minute,
               count(*) AS attempts,
               count(*) FILTER (WHERE r.status = 'ok' AND r.external_status IS DISTINCT FROM 'failed') AS successes
        FROM removed_probes r JOIN nodes n ON n.id = r.node_id
        WHERE r.node_id IS DISTINCT FROM cleanup_node
          AND r.status IN ('ok', 'failed')
        GROUP BY r.node_id, hysteriax_minute(r.sampled_at)
    ), removed AS (
        DELETE FROM probe_minute_totals totals USING delta d
        WHERE totals.node_id = d.node_id AND totals.minute = d.minute
          AND totals.attempts <= d.attempts
    )
    UPDATE probe_minute_totals totals
    SET attempts = totals.attempts - d.attempts,
        successes = totals.successes - d.successes
    FROM delta d
    WHERE totals.node_id = d.node_id AND totals.minute = d.minute
      AND totals.attempts > d.attempts;

    RETURN NULL;
END $$;

CREATE TRIGGER probe_deleted AFTER DELETE ON proxy_probe_samples
    REFERENCING OLD TABLE AS removed_probes
    FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_probe_deleted();

DROP TRIGGER cycle_dirty ON online_samples;
CREATE TRIGGER cycle_dirty AFTER INSERT OR UPDATE ON online_samples
    FOR EACH ROW EXECUTE FUNCTION hysteriax_cycle_dirty();

DROP TRIGGER cache_row_changed ON online_samples;
CREATE TRIGGER cache_row_changed AFTER INSERT OR UPDATE ON online_samples
    FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('online', 'false');

CREATE FUNCTION hysteriax_online_deleted() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE item record;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM removed_online) THEN
        RETURN NULL;
    END IF;

    INSERT INTO cache_bucket_pending
    SELECT DISTINCT txid_current(), 'online', hysteriax_cache_bucket(node_id)
    FROM removed_online ON CONFLICT DO NOTHING;

    INSERT INTO cache_entity_pending
    SELECT DISTINCT txid_current(), 'nodes', node_id
    FROM removed_online ON CONFLICT DO NOTHING;
    PERFORM hysteriax_cache_queue_flush();

    FOR item IN
        SELECT DISTINCT cycle_id FROM removed_online ORDER BY cycle_id
    LOOP
        PERFORM hysteriax_mark_dirty(
            'analytics_dirty_cycles',
            jsonb_build_object('cycle_id', item.cycle_id)
        );
    END LOOP;

    RETURN NULL;
END $$;

CREATE TRIGGER online_deleted AFTER DELETE ON online_samples
    REFERENCING OLD TABLE AS removed_online
    FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_online_deleted();

-- 4. Track tombstone update timestamps for cache_entity_versions retention.
ALTER TABLE cache_entity_versions
    ADD COLUMN updated_at timestamptz NOT NULL DEFAULT clock_timestamp();

CREATE OR REPLACE FUNCTION hysteriax_cache_flush() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE entity_kind text; entity text; present boolean;
    entity_bucket smallint; bucket_revision bigint; entity_revision bigint;
BEGIN
    PERFORM 1 FROM cache_bucket_versions v
    JOIN cache_bucket_pending p USING (domain, bucket)
    WHERE p.transaction_id=NEW.transaction_id
    ORDER BY v.domain, v.bucket FOR NO KEY UPDATE OF v;

    UPDATE cache_bucket_versions v SET generation=v.generation+1
    FROM cache_bucket_pending p WHERE p.transaction_id=NEW.transaction_id
        AND v.domain=p.domain AND v.bucket=p.bucket;

    FOR entity_kind IN
        SELECT DISTINCT kind FROM cache_entity_pending
        WHERE transaction_id=NEW.transaction_id ORDER BY kind
    LOOP
        IF entity_kind='quota_rank' THEN
            FOR entity IN
                SELECT entity_id FROM cache_entity_pending
                WHERE transaction_id=NEW.transaction_id AND kind='quota_rank' ORDER BY entity_id
            LOOP
                entity_bucket:=hysteriax_cache_bucket(entity);
                SELECT EXISTS(
                    SELECT 1 FROM nodes n JOIN node_packages p ON p.node_id=n.id
                    WHERE n.id=entity AND (p.config->>'quota_bytes')::bigint>0
                ) INTO present;
                SELECT generation INTO bucket_revision FROM cache_bucket_versions
                WHERE domain='quota_rank' AND bucket=entity_bucket;
                INSERT INTO cache_entity_versions(kind,entity_id,generation,deleted,bucket,bucket_generation,updated_at)
                VALUES('quota_rank',entity,1,NOT present,entity_bucket,bucket_revision,clock_timestamp())
                ON CONFLICT(kind,entity_id) DO UPDATE SET generation=cache_entity_versions.generation+1,
                    deleted=EXCLUDED.deleted,bucket_generation=EXCLUDED.bucket_generation,
                    updated_at=clock_timestamp()
                RETURNING generation INTO entity_revision;
                PERFORM pg_notify('hysteriax_quota_rank',json_build_object(
                    'schema',current_schema(),'id',entity,'version',entity_revision
                )::text);
            END LOOP;
        ELSE
            EXECUTE format(
                'INSERT INTO cache_entity_versions(kind,entity_id,generation,deleted,bucket,bucket_generation,updated_at)
                 SELECT p.kind,p.entity_id,1,
                    NOT coalesce((SELECT true FROM %I e WHERE e.id=p.entity_id),false),
                    hysteriax_cache_bucket(p.entity_id),0,clock_timestamp()
                 FROM cache_entity_pending p WHERE p.transaction_id=$1 AND p.kind=$2
                 ORDER BY p.entity_id
                 ON CONFLICT(kind,entity_id) DO UPDATE SET generation=cache_entity_versions.generation+1,
                    deleted=EXCLUDED.deleted,bucket_generation=EXCLUDED.bucket_generation,
                    updated_at=clock_timestamp()',
                entity_kind
            ) USING NEW.transaction_id, entity_kind;
        END IF;
    END LOOP;
    DELETE FROM cache_bucket_pending WHERE transaction_id=NEW.transaction_id;
    DELETE FROM cache_entity_pending WHERE transaction_id=NEW.transaction_id;
    DELETE FROM cache_flush_pending WHERE transaction_id=NEW.transaction_id;
    PERFORM set_config('hysteriax.cache_flush_transaction','',true);
    RETURN NULL;
END $$;

-- 5. Retire legacy single-threaded monitoring-retention maintenance rows.
DELETE FROM resource_leases l
WHERE l.resource_key IN (
    'work:maintenance:monitoring-retention:online_samples',
    'work:maintenance:monitoring-retention:proxy_probe_samples',
    'work:maintenance:monitoring-retention:monitoring_probe_tokens'
)
AND NOT EXISTS (SELECT 1 FROM execution_operations o WHERE o.resource_key = l.resource_key);

DELETE FROM background_work
WHERE id IN (
    'maintenance:monitoring-retention:online_samples',
    'maintenance:monitoring-retention:proxy_probe_samples',
    'maintenance:monitoring-retention:monitoring_probe_tokens'
);

-- 6. Targeted retention indexes for sharded monitoring and full operational/metadata cleanup.
CREATE INDEX online_samples_history_idx
    ON online_samples ((hashtext(cycle_id::text) & 3), sampled_at, cycle_id, id)
    WHERE NOT pending;
CREATE INDEX proxy_probe_samples_history_idx
    ON proxy_probe_samples ((hashtext(node_id) & 3), sampled_at, id);
CREATE INDEX traffic_sample_batches_discarded_history_idx
    ON traffic_sample_batches ((hashtext(node_id) & 3), completed_at, sample_id)
    WHERE state = 'discarded' AND completed_at IS NOT NULL;
CREATE INDEX traffic_baselines_history_idx
    ON traffic_baselines (sampled_at, node_id, user_id);
CREATE INDEX monitoring_probe_tokens_history_idx
    ON monitoring_probe_tokens (expires_at, node_id);
CREATE INDEX execution_operations_verified_history_idx
    ON execution_operations (updated_at, operation_id)
    WHERE state = 'verified';
CREATE INDEX execution_recheck_requests_history_idx
    ON execution_recheck_requests (created_at, namespace);
CREATE INDEX background_work_non_sampling_retention_idx
    ON background_work (finished_at, id)
    WHERE cycle_id IS NULL
      AND state IN ('complete', 'failed', 'expired')
      AND finished_at IS NOT NULL
      AND kind IN (
          'access-revocation',
          'node-limit-dispatch',
          'mtls-trust-cleanup-page',
          'mtls-node-publication-admission',
          'mtls-node-publication-metadata',
          'mtls-rotation-publication',
          'mtls-rotation-user-admission',
          'mtls-rotation-user-issuance',
          'mtls-superseded-identity-flags',
          'mtls-rotation-probe-admission',
          'mtls-rotation-probe-switch',
          'mtls-identity-publication-schedule',
          'mtls-superseded-node-jobs',
          'mtls-rotation-trust',
          'mtls-identity-renewal',
          'mtls-probe-renewal',
          'mtls-ca-publication'
      );
CREATE INDEX resource_leases_released_idx
    ON resource_leases (resource_key)
    WHERE owner IS NULL AND work_id IS NULL AND NOT blocked;
CREATE INDEX cache_entity_versions_tombstone_idx
    ON cache_entity_versions (updated_at, kind, entity_id)
    WHERE deleted AND kind <> 'quota_rank';
CREATE INDEX management_receipts_history_idx
    ON management_receipts (created_at, actor_id, operation, idempotency_key);
CREATE INDEX mtls_operations_history_idx
    ON mtls_operations (created_at, actor_id, idempotency_key);
CREATE INDEX kick_requests_history_idx
    ON kick_requests (updated_at, node_id, user_id)
    WHERE state IN ('completed', 'cancelled');
CREATE INDEX credential_application_receipts_history_idx
    ON credential_application_receipts (created_at, job_id);
CREATE INDEX credential_batches_history_idx
    ON credential_batches (created_at, id);
CREATE INDEX dns_operations_history_idx
    ON dns_operations (created_at, id);
CREATE INDEX dns_operations_resource_idx
    ON dns_operations (resource_id);
CREATE INDEX dns_bindings_record_ids_idx
    ON dns_bindings USING gin (record_ids);
CREATE INDEX dns_records_deleted_history_idx
    ON dns_records (deleted_at, id)
    WHERE deleted_at IS NOT NULL;
CREATE INDEX user_credentials_revoked_history_idx
    ON user_credentials (revoked_at, id)
    WHERE revoked_at IS NOT NULL;
CREATE INDEX config_versions_history_idx
    ON config_versions (created_at, id);
CREATE INDEX mtls_node_operations_history_idx
    ON mtls_node_operations (created_at, job_id);
CREATE INDEX mtls_rotations_history_idx
    ON mtls_rotations (updated_at, id)
    WHERE phase IN ('complete', 'failed');
CREATE INDEX mtls_trust_cleanup_history_idx
    ON mtls_trust_cleanup (completed_at, ca_id, ca_version)
    WHERE completed_at IS NOT NULL;
CREATE INDEX mtls_deployment_snapshots_history_idx
    ON mtls_deployment_snapshots (created_at, node_id, revision, activation_id);
CREATE INDEX mtls_activations_history_idx
    ON mtls_activations (updated_at, id)
    WHERE phase IN ('committed', 'rolled_back');
CREATE INDEX mtls_activations_previous_idx
    ON mtls_activations (previous_activation_id)
    WHERE previous_activation_id IS NOT NULL;
