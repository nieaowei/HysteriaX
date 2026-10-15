-- Stop the API first. Return schema 0027 to 0026 without changing source data.
BEGIN;
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version>27) THEN
  RAISE EXCEPTION 'refusing overview pagination rollback after newer migrations';
 END IF;
END $$;
DROP INDEX IF EXISTS nodes_monitoring_search_trgm;
DELETE FROM _sqlx_migrations WHERE version=27;
COMMIT;
