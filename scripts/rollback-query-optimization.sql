-- Run only with the API stopped, to return schema 0025/0026 to 0024.
-- Source rows are preserved; pg_trgm remains installed in public.
BEGIN;
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version>26) THEN
  RAISE EXCEPTION 'refusing query optimization rollback after newer migrations';
 END IF;
END $$;
DROP VIEW IF EXISTS credential_directory;
DROP FUNCTION IF EXISTS hysteriax_directory_credential_status(boolean,timestamptz);
DROP FUNCTION IF EXISTS hysteriax_directory_user_status(boolean,timestamptz,bigint,bigint);
DROP FUNCTION IF EXISTS hysteriax_rfc3339(text);
DROP INDEX IF EXISTS credential_name_page,credential_kind_page,credential_name_trgm;
DROP TRIGGER IF EXISTS audit_count_insert ON audit_records;
DROP TRIGGER IF EXISTS audit_count_delete ON audit_records;
DROP TRIGGER IF EXISTS audit_count_truncate ON audit_records;
DROP FUNCTION IF EXISTS hysteriax_audit_inserted(),hysteriax_audit_deleted(),hysteriax_audit_truncated();
DROP TABLE IF EXISTS audit_totals;
DROP INDEX IF EXISTS jobs_search_trgm,audit_search_trgm,jobs_kind_page,jobs_stage_page,jobs_status_page,jobs_node_page,audit_action_page,audit_type_page,audit_entity_page,audit_actor_page;
DROP FUNCTION IF EXISTS hysteriax_search_text(text[]);
DELETE FROM _sqlx_migrations WHERE version IN(25,26);
COMMIT;
