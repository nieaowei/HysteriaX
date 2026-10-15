-- The extension is database-wide; serialize installation across isolated test schemas.
SELECT pg_advisory_xact_lock(62177345902813);
CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;
-- All inputs are text: unlike general concat_ws(any), this wrapper has no
-- locale/timezone-sensitive conversion of non-text values and can be indexed.
CREATE FUNCTION hysteriax_search_text(VARIADIC parts text[]) RETURNS text
LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$ SELECT lower(array_to_string(parts,' ')) $$;
CREATE INDEX jobs_search_trgm ON jobs USING gin(hysteriax_search_text(id,kind,stage,status,node_name,node_id,resource_name,resource_id,error_message) public.gin_trgm_ops);
CREATE INDEX audit_search_trgm ON audit_records USING gin(hysteriax_search_text(id,action,actor,entity_type,entity_id) public.gin_trgm_ops);
CREATE INDEX jobs_kind_page ON jobs(kind,id);
CREATE INDEX jobs_stage_page ON jobs(stage,id);
CREATE INDEX jobs_status_page ON jobs(status,id);
CREATE INDEX jobs_node_page ON jobs((coalesce(resource_name,node_name,node_id,'')),id);
CREATE INDEX audit_action_page ON audit_records(action,id);
CREATE INDEX audit_type_page ON audit_records(entity_type,id);
CREATE INDEX audit_entity_page ON audit_records(entity_id,id);
CREATE INDEX audit_actor_page ON audit_records(actor,id);
CREATE TABLE audit_totals(id boolean PRIMARY KEY DEFAULT true CHECK(id),total bigint NOT NULL CHECK(total>=0));
INSERT INTO audit_totals SELECT true,count(*) FROM audit_records;
CREATE FUNCTION hysteriax_audit_inserted() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN UPDATE audit_totals SET total=total+(SELECT count(*) FROM added); RETURN NULL; END $$;
CREATE FUNCTION hysteriax_audit_deleted() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN UPDATE audit_totals SET total=total-(SELECT count(*) FROM removed); RETURN NULL; END $$;
CREATE FUNCTION hysteriax_audit_truncated() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN UPDATE audit_totals SET total=0; RETURN NULL; END $$;
CREATE TRIGGER audit_count_insert AFTER INSERT ON audit_records REFERENCING NEW TABLE AS added FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_audit_inserted();
CREATE TRIGGER audit_count_delete AFTER DELETE ON audit_records REFERENCING OLD TABLE AS removed FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_audit_deleted();
CREATE TRIGGER audit_count_truncate AFTER TRUNCATE ON audit_records FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_audit_truncated();
