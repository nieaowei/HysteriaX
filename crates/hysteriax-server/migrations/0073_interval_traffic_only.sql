-- Previously deployed nodes must finish their cutover with an older server.
-- Do not reinterpret a pending cumulative response as an interval increment.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM traffic_sample_batches
               WHERE capture_mode <> 'interval' AND state IN ('capturing','ready'))
       OR EXISTS (SELECT 1 FROM nodes n WHERE traffic_capture_mode <> 'interval'
                  AND (deployed_revision IS NOT NULL
                       OR EXISTS (SELECT 1 FROM traffic_sample_batches b WHERE b.node_id=n.id)
                       OR EXISTS (SELECT 1 FROM traffic_baselines b WHERE b.node_id=n.id))) THEN
        RAISE EXCEPTION 'finish traffic cutover and settle legacy captures before interval-only migration';
    END IF;
END $$;

-- Undeployed nodes without accounting history can start directly in interval mode.
UPDATE nodes SET traffic_capture_mode='interval' WHERE traffic_capture_mode<>'interval';
ALTER TABLE nodes ALTER COLUMN traffic_capture_mode SET DEFAULT 'interval';
ALTER TABLE nodes DROP CONSTRAINT nodes_traffic_capture_mode_check;
ALTER TABLE nodes ADD CONSTRAINT nodes_traffic_capture_mode_check CHECK (traffic_capture_mode='interval');

ALTER TABLE traffic_sample_batches ALTER COLUMN capture_mode SET DEFAULT 'interval';
ALTER TABLE traffic_sample_batches ADD CONSTRAINT traffic_pending_interval_only
    CHECK (capture_mode='interval' OR state IN ('complete','discarded'));
ALTER TABLE traffic_sample_batches DROP COLUMN cutover_checked;
ALTER TABLE traffic_sample_batches DROP COLUMN cutover_gap;

-- Earlier collectors did not store this observation. Normalize only pending
-- captured intervals; completed historical rows need no rewrite.
UPDATE traffic_sample_batches SET clear_instance_verified=true
    WHERE state='ready' AND clear_instance_verified IS NULL;
ALTER TABLE traffic_sample_batches ALTER COLUMN clear_instance_verified SET DEFAULT false;
ALTER TABLE traffic_sample_batches ADD CONSTRAINT traffic_ready_instance_observed
    CHECK (state<>'ready' OR clear_instance_verified IS NOT NULL);

-- Completed cleanup run/progress rows remain as audit records. They do not
-- schedule work; their one-time executable tooling has been removed.
