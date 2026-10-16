-- Stop the API first. Return schema 0029 to 0028 without changing source data.
BEGIN;
SET LOCAL lock_timeout = '5s';
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version>29) THEN
  RAISE EXCEPTION 'refusing traffic index rollback after newer migrations';
 END IF;
END $$;
DROP INDEX IF EXISTS traffic_records_user_node_time_idx;
DROP INDEX IF EXISTS traffic_records_node_time_idx;
DELETE FROM _sqlx_migrations WHERE version=29;
COMMIT;
