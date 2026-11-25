-- Scale indexes for 100k users / 10k nodes, deterministic job_status_totals
-- lock ordering, statement-level DELETE trigger on node_network_samples, and
-- removal of redundant indexes.

-- 1. Missing scale & foreign-key indexes.
CREATE INDEX node_assignments_node_user_idx
    ON node_assignments (node_id, user_id);
CREATE INDEX credentials_owner_user_idx
    ON credentials (owner_user_id, id)
    WHERE owner_user_id IS NOT NULL;
CREATE INDEX traffic_baselines_user_node_idx
    ON traffic_baselines (user_id, node_id);
CREATE INDEX nodes_mtls_activation_idx
    ON nodes (mtls_activation_id);
CREATE INDEX mtls_deployment_snapshots_activation_idx
    ON mtls_deployment_snapshots (activation_id);
CREATE INDEX mtls_node_operations_base_activation_idx
    ON mtls_node_operations (base_activation_id);
CREATE INDEX mtls_node_operations_rotation_idx
    ON mtls_node_operations (rotation_id);
CREATE INDEX mtls_node_operations_node_idx
    ON mtls_node_operations (node_id);
CREATE INDEX mtls_cleanup_nodes_node_idx
    ON mtls_cleanup_nodes (node_id);
CREATE INDEX mtls_rotation_nodes_node_idx
    ON mtls_rotation_nodes (node_id);
CREATE INDEX users_enabled_expires_idx
    ON users (expires_at, id)
    WHERE enabled AND expires_at IS NOT NULL;
CREATE INDEX users_enabled_exhausted_idx
    ON users (id)
    WHERE enabled AND quota_bytes IS NOT NULL AND usage_bytes >= quota_bytes;

-- 2. Drop duplicate / strictly subsumed indexes.
DROP INDEX monitoring_probe_tokens_expiry_idx;
DROP INDEX traffic_sample_batches_capture_deadline;

-- 3. Lock job_status_totals rows in deterministic status order before adjusting
--    both OLD and NEW rows to prevent AB-BA deadlocks under concurrent job transitions.
CREATE OR REPLACE FUNCTION hysteriax_job_totals_changed() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    s1 text;
    s2 text;
BEGIN
    IF TG_OP = 'UPDATE' AND (OLD.status, OLD.kind, OLD.node_id, OLD.payload_json->>'node_limit', OLD.finished_at)
        IS NOT DISTINCT FROM (NEW.status, NEW.kind, NEW.node_id, NEW.payload_json->>'node_limit', NEW.finished_at) THEN
        RETURN NULL;
    END IF;
    IF TG_OP = 'UPDATE' AND OLD.status IS DISTINCT FROM NEW.status THEN
        s1 := least(OLD.status, NEW.status);
        s2 := greatest(OLD.status, NEW.status);
        PERFORM 1 FROM job_status_totals WHERE status IN (s1, s2) ORDER BY status FOR UPDATE;
    END IF;
    IF TG_OP <> 'INSERT' THEN PERFORM hysteriax_adjust_job(OLD, -1); END IF;
    IF TG_OP <> 'DELETE' THEN PERFORM hysteriax_adjust_job(NEW, 1); END IF;
    RETURN NULL;
END $$;

-- 4. Convert node_network_samples DELETE triggers to statement-level so retention
--    batches aggregate rollup updates once per statement instead of per row.
DROP TRIGGER flow_totals ON node_network_samples;
CREATE TRIGGER flow_totals AFTER INSERT OR UPDATE ON node_network_samples
    FOR EACH ROW EXECUTE FUNCTION hysteriax_flow_totals();

DROP TRIGGER cache_row_changed ON node_network_samples;
CREATE TRIGGER cache_row_changed AFTER INSERT OR UPDATE ON node_network_samples
    FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('network', 'false');

CREATE FUNCTION hysteriax_network_deleted() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE cleanup_node text := current_setting('hysteriax.history_cleanup_node', true);
BEGIN
    IF NOT EXISTS (SELECT 1 FROM removed_network) THEN
        RETURN NULL;
    END IF;

    INSERT INTO cache_bucket_pending
    SELECT DISTINCT txid_current(), 'network', hysteriax_cache_bucket(node_id)
    FROM removed_network ON CONFLICT DO NOTHING;

    INSERT INTO cache_entity_pending
    SELECT DISTINCT txid_current(), 'nodes', node_id
    FROM removed_network ON CONFLICT DO NOTHING;
    PERFORM hysteriax_cache_queue_flush();

    IF NOT EXISTS (
        SELECT 1 FROM removed_network r JOIN nodes n ON n.id = r.node_id
        WHERE r.node_id IS DISTINCT FROM cleanup_node
    ) THEN
        RETURN NULL;
    END IF;

    PERFORM 1
    FROM traffic_minute_totals totals
    JOIN (
        SELECT DISTINCT r.node_id, hysteriax_minute(r.sampled_at) AS minute
        FROM removed_network r JOIN nodes n ON n.id = r.node_id
        WHERE r.node_id IS DISTINCT FROM cleanup_node
    ) affected USING (node_id, minute)
    WHERE totals.source = 'network'
    ORDER BY totals.node_id, totals.minute
    FOR UPDATE OF totals;

    WITH delta AS MATERIALIZED (
        SELECT r.node_id, hysteriax_minute(r.sampled_at) AS minute,
               sum(r.delta_tx) AS tx, sum(r.delta_rx) AS rx, count(*) AS samples,
               count(*) FILTER (WHERE r.gap_reason IS NOT NULL OR r.baseline_only) AS incomplete
        FROM removed_network r JOIN nodes n ON n.id = r.node_id
        WHERE r.node_id IS DISTINCT FROM cleanup_node
        GROUP BY r.node_id, hysteriax_minute(r.sampled_at)
    ), removed AS (
        DELETE FROM traffic_minute_totals totals USING delta d
        WHERE totals.source = 'network' AND totals.node_id = d.node_id
          AND totals.minute = d.minute AND totals.samples = d.samples
    )
    UPDATE traffic_minute_totals totals
    SET tx = totals.tx - d.tx, rx = totals.rx - d.rx, samples = totals.samples - d.samples,
        incomplete_samples = totals.incomplete_samples - d.incomplete
    FROM delta d
    WHERE totals.source = 'network' AND totals.node_id = d.node_id
      AND totals.minute = d.minute AND totals.samples > d.samples;

    RETURN NULL;
END $$;

CREATE TRIGGER network_deleted AFTER DELETE ON node_network_samples
    REFERENCING OLD TABLE AS removed_network
    FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_network_deleted();
