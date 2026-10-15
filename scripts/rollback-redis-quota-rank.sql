-- Stop the API first. Return schema 0028 to 0027, preserving billing records.
BEGIN;
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version>28) THEN
  RAISE EXCEPTION 'refusing quota rank rollback after newer migrations';
 END IF;
END $$;
DROP TRIGGER IF EXISTS quota_rank_changed ON nodes;
DROP TRIGGER IF EXISTS quota_rank_changed ON node_packages;
DROP TRIGGER IF EXISTS quota_rank_truncated ON nodes;
DROP TRIGGER IF EXISTS quota_rank_truncated ON node_packages;
DROP FUNCTION IF EXISTS hysteriax_quota_rank_changed();
DELETE FROM cache_domain_versions WHERE domain='quota_rank';
DELETE FROM _sqlx_migrations WHERE version=28;
COMMIT;
