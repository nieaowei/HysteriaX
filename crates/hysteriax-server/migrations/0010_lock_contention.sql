-- Stop every API replica before applying this migration.
ALTER TABLE node_packages ADD COLUMN next_evaluation_at timestamptz DEFAULT clock_timestamp();
CREATE INDEX node_packages_evaluation_due_idx ON node_packages(next_evaluation_at,node_id)
 WHERE next_evaluation_at IS NOT NULL;

-- The default initializes new packages once. Changes made through maintenance
-- SQL also invalidate scheduling; normal application saves evaluate atomically.
CREATE FUNCTION hysteriax_package_schedule_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF OLD.config IS DISTINCT FROM NEW.config THEN
  NEW.next_evaluation_at := clock_timestamp();
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER package_schedule_changed BEFORE UPDATE OF config ON node_packages
 FOR EACH ROW EXECUTE FUNCTION hysteriax_package_schedule_changed();

-- Replace only the retired scanner, never admitted restriction/restore work.
DELETE FROM resource_leases WHERE resource_key='work:maintenance:node-limits:evaluate';
DELETE FROM background_work WHERE id='maintenance:node-limits:evaluate';

-- Retry refreshers now use transaction advisory locks, not persistent guard rows.
DROP TABLE analytics_retry_guards;
