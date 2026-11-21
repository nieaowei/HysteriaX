-- Keep row triggers for ledger writes. A retention DELETE can remove many
-- entries for the same node/minute and user/node in one statement.
DROP TRIGGER flow_totals ON traffic_records;
CREATE TRIGGER flow_totals AFTER INSERT OR UPDATE ON traffic_records
    FOR EACH ROW EXECUTE FUNCTION hysteriax_flow_totals();

DROP TRIGGER cache_row_changed ON traffic_records;
CREATE TRIGGER cache_row_changed AFTER INSERT OR UPDATE ON traffic_records
    FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('traffic', 'false');

CREATE FUNCTION hysteriax_ledger_deleted() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE cleanup_node text := current_setting('hysteriax.history_cleanup_node', true);
BEGIN
    IF NOT EXISTS (SELECT 1 FROM removed_records) THEN
        RETURN NULL;
    END IF;

    -- Preserve the traffic bucket and user/node entity invalidations from the
    -- row trigger, using the same transaction-local pending tables.
    INSERT INTO cache_bucket_pending
    SELECT DISTINCT txid_current(), 'traffic', hysteriax_cache_bucket(node_id)
    FROM removed_records ON CONFLICT DO NOTHING;

    INSERT INTO cache_entity_pending
    SELECT txid_current(), kind, entity_id
    FROM (
        SELECT 'nodes' AS kind, node_id AS entity_id FROM removed_records
        UNION
        SELECT 'users', user_id FROM removed_records
    ) affected
    ON CONFLICT DO NOTHING;
    PERFORM hysteriax_cache_queue_flush();

    -- A deleted node's FK cascades remove its summaries. The incremental
    -- node cleanup also explicitly owns all of that node's summaries.
    IF NOT EXISTS (
        SELECT 1 FROM removed_records r JOIN nodes n ON n.id = r.node_id
        WHERE r.node_id IS DISTINCT FROM cleanup_node
    ) THEN
        RETURN NULL;
    END IF;

    -- Lock summaries in key order before deriving the deltas and looking up
    -- a remaining latest sample. Subsequent statements see earlier commits
    -- from competing writers at READ COMMITTED.
    PERFORM 1
    FROM traffic_minute_totals totals
    JOIN (
        SELECT DISTINCT r.node_id, hysteriax_minute(r.sampled_at) AS minute
        FROM removed_records r JOIN nodes n ON n.id = r.node_id
        WHERE r.node_id IS DISTINCT FROM cleanup_node
    ) affected USING (node_id, minute)
    WHERE totals.source = 'users'
    ORDER BY totals.node_id, totals.minute
    FOR UPDATE OF totals;

    WITH delta AS MATERIALIZED (
        SELECT r.node_id, hysteriax_minute(r.sampled_at) AS minute,
               sum(r.delta_tx) AS tx, sum(r.delta_rx) AS rx, count(*) AS samples,
               count(*) FILTER (WHERE r.gap_reason IS NOT NULL OR r.baseline_only) AS incomplete
        FROM removed_records r JOIN nodes n ON n.id = r.node_id
        WHERE r.node_id IS DISTINCT FROM cleanup_node
        GROUP BY r.node_id, hysteriax_minute(r.sampled_at)
    ), removed AS (
        DELETE FROM traffic_minute_totals totals USING delta d
        WHERE totals.source = 'users' AND totals.node_id = d.node_id
          AND totals.minute = d.minute AND totals.samples = d.samples
    )
    UPDATE traffic_minute_totals totals
    SET tx = totals.tx - d.tx, rx = totals.rx - d.rx, samples = totals.samples - d.samples,
        incomplete_samples = totals.incomplete_samples - d.incomplete
    FROM delta d
    WHERE totals.source = 'users' AND totals.node_id = d.node_id
      AND totals.minute = d.minute AND totals.samples > d.samples;

    PERFORM 1
    FROM user_node_usage usage
    JOIN (
        SELECT DISTINCT r.user_id, r.node_id
        FROM removed_records r JOIN nodes n ON n.id = r.node_id
        WHERE r.node_id IS DISTINCT FROM cleanup_node
    ) affected USING (user_id, node_id)
    ORDER BY usage.user_id, usage.node_id
    FOR UPDATE OF usage;

    WITH delta AS MATERIALIZED (
        SELECT r.user_id, r.node_id, sum(r.delta_tx) AS tx, sum(r.delta_rx) AS rx,
               count(*) AS samples, max(r.sampled_at) AS latest
        FROM removed_records r JOIN nodes n ON n.id = r.node_id
        WHERE r.node_id IS DISTINCT FROM cleanup_node
        GROUP BY r.user_id, r.node_id
    ), removed AS (
        DELETE FROM user_node_usage usage USING delta d
        WHERE usage.user_id = d.user_id AND usage.node_id = d.node_id AND usage.samples = d.samples
    )
    UPDATE user_node_usage usage
    SET tx = usage.tx - d.tx, rx = usage.rx - d.rx, samples = usage.samples - d.samples,
        sampled_at = CASE WHEN usage.sampled_at = d.latest THEN coalesce((
            SELECT r.sampled_at FROM traffic_records r
            WHERE r.user_id = d.user_id AND r.node_id = d.node_id
            ORDER BY r.sampled_at DESC LIMIT 1
        ), usage.sampled_at) ELSE usage.sampled_at END
    FROM delta d
    WHERE usage.user_id = d.user_id AND usage.node_id = d.node_id AND usage.samples > d.samples;

    RETURN NULL;
END $$;

CREATE TRIGGER ledger_deleted AFTER DELETE ON traffic_records
    REFERENCING OLD TABLE AS removed_records
    FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_ledger_deleted();

-- Flush a retention page without per-entity PL/pgSQL work while holding the
-- shared cache bucket locks. Keep the lock order and exact version semantics.
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
                INSERT INTO cache_entity_versions(kind,entity_id,generation,deleted,bucket,bucket_generation)
                VALUES('quota_rank',entity,1,NOT present,entity_bucket,bucket_revision)
                ON CONFLICT(kind,entity_id) DO UPDATE SET generation=cache_entity_versions.generation+1,
                    deleted=EXCLUDED.deleted,bucket_generation=EXCLUDED.bucket_generation
                RETURNING generation INTO entity_revision;
                PERFORM pg_notify('hysteriax_quota_rank',json_build_object(
                    'schema',current_schema(),'id',entity,'version',entity_revision
                )::text);
            END LOOP;
        ELSE
            EXECUTE format(
                'INSERT INTO cache_entity_versions(kind,entity_id,generation,deleted,bucket,bucket_generation)
                 SELECT p.kind,p.entity_id,1,
                    NOT coalesce((SELECT true FROM %I e WHERE e.id=p.entity_id),false),
                    hysteriax_cache_bucket(p.entity_id),0
                 FROM cache_entity_pending p WHERE p.transaction_id=$1 AND p.kind=$2
                 ORDER BY p.entity_id
                 ON CONFLICT(kind,entity_id) DO UPDATE SET generation=cache_entity_versions.generation+1,
                    deleted=EXCLUDED.deleted,bucket_generation=EXCLUDED.bucket_generation',
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

-- Keep the bounded sampling claim ordered on its active queue. Nodes due at
-- the same time sort differently for each kind sharing their execution leases.
CREATE INDEX background_work_sampling_claim_idx
    ON background_work (kind, available_at, (hashtext(kind || ':' || node_id)), id)
    WHERE cycle_id IS NOT NULL AND state IN ('queued','running');
