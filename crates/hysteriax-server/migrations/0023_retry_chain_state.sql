CREATE TABLE job_retry_chain_state(leaf_id text PRIMARY KEY,root_id text NOT NULL,name text NOT NULL,status text NOT NULL,last_error text,last_failure_at timestamptz,failed_seed_at timestamptz);
CREATE INDEX retry_chain_failure_window ON job_retry_chain_state(failed_seed_at) WHERE status<>'succeeded';
CREATE TABLE job_retry_chain_members(job_id text PRIMARY KEY,root_id text NOT NULL);
CREATE INDEX retry_chain_members_root ON job_retry_chain_members(root_id);
CREATE TABLE job_retry_state_dirty(job_id text PRIMARY KEY);
INSERT INTO job_retry_state_dirty SELECT id FROM jobs;
CREATE FUNCTION hysteriax_retry_state_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='UPDATE' AND jsonb_build_array(to_jsonb(OLD)->'retry_of_job_id',to_jsonb(OLD)->'status',to_jsonb(OLD)->'error_message',to_jsonb(OLD)->'finished_at',to_jsonb(OLD)->'node_name',to_jsonb(OLD)->'kind') = jsonb_build_array(to_jsonb(NEW)->'retry_of_job_id',to_jsonb(NEW)->'status',to_jsonb(NEW)->'error_message',to_jsonb(NEW)->'finished_at',to_jsonb(NEW)->'node_name',to_jsonb(NEW)->'kind') THEN RETURN NULL; END IF;
 IF TG_OP<>'INSERT' THEN
  INSERT INTO job_retry_state_dirty SELECT value FROM unnest(ARRAY[OLD.id,OLD.retry_of_job_id]) value WHERE value IS NOT NULL ON CONFLICT DO NOTHING;
 END IF;
 IF TG_OP<>'DELETE' THEN
  INSERT INTO job_retry_state_dirty SELECT value FROM unnest(ARRAY[NEW.id,NEW.retry_of_job_id]) value WHERE value IS NOT NULL ON CONFLICT DO NOTHING;
 END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER retry_state_changed AFTER INSERT OR UPDATE OR DELETE ON jobs FOR EACH ROW EXECUTE FUNCTION hysteriax_retry_state_changed();
