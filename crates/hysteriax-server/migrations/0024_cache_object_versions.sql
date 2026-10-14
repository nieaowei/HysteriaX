CREATE TABLE job_cache_versions(job_id text PRIMARY KEY,generation bigint NOT NULL DEFAULT 0);
CREATE TABLE batch_cache_versions(batch_id text PRIMARY KEY,generation bigint NOT NULL DEFAULT 0);
INSERT INTO job_cache_versions SELECT id,0 FROM jobs;
INSERT INTO batch_cache_versions SELECT id,0 FROM credential_batches;
CREATE INDEX credential_batch_items_job ON credential_batch_items(job_id);
CREATE INDEX dns_batch_items_job ON dns_credential_batch_items(job_id);
CREATE FUNCTION hysteriax_job_cache_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE ids text[];
BEGIN
 IF TG_OP='UPDATE' AND to_jsonb(OLD)-ARRAY['lease_owner','lease_expires_at']=to_jsonb(NEW)-ARRAY['lease_owner','lease_expires_at'] THEN RETURN NULL; END IF;
 IF TG_OP<>'INSERT' THEN ids:=ARRAY[OLD.id]; END IF;
 IF TG_OP<>'DELETE' THEN ids:=coalesce(ids,ARRAY[]::text[])||ARRAY[NEW.id]; END IF;
 IF TG_OP<>'UPDATE' OR OLD.retry_of_job_id IS DISTINCT FROM NEW.retry_of_job_id THEN
  IF TG_OP<>'INSERT' THEN ids:=ids||ARRAY[OLD.retry_of_job_id];END IF;
  IF TG_OP<>'DELETE' THEN ids:=ids||ARRAY[NEW.retry_of_job_id];END IF;
 END IF;
 INSERT INTO job_cache_versions SELECT DISTINCT id,1 FROM unnest(ids) id WHERE id IS NOT NULL ON CONFLICT(job_id) DO UPDATE SET generation=job_cache_versions.generation+1;
 IF TG_OP<>'UPDATE' OR (OLD.status,OLD.stage,OLD.error_message) IS DISTINCT FROM (NEW.status,NEW.stage,NEW.error_message) THEN
  INSERT INTO batch_cache_versions
  SELECT DISTINCT batch_id,1 FROM (SELECT batch_id FROM credential_batch_items WHERE job_id=ANY(ids) UNION SELECT batch_id FROM dns_credential_batch_items WHERE job_id=ANY(ids)) x
  ON CONFLICT(batch_id) DO UPDATE SET generation=batch_cache_versions.generation+1;
 END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER job_cache_changed AFTER INSERT OR UPDATE OR DELETE ON jobs FOR EACH ROW EXECUTE FUNCTION hysteriax_job_cache_changed();
CREATE FUNCTION hysteriax_batch_cache_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE ids text[];
BEGIN
 IF TG_TABLE_NAME='credential_batches' THEN
  IF TG_OP<>'INSERT' THEN ids:=ARRAY[OLD.id];END IF;
  IF TG_OP<>'DELETE' THEN ids:=coalesce(ids,ARRAY[]::text[])||ARRAY[NEW.id];END IF;
 ELSE
  IF TG_OP<>'INSERT' THEN ids:=ARRAY[OLD.batch_id];END IF;
  IF TG_OP<>'DELETE' THEN ids:=coalesce(ids,ARRAY[]::text[])||ARRAY[NEW.batch_id];END IF;
 END IF;
 INSERT INTO batch_cache_versions SELECT DISTINCT id,1 FROM unnest(ids) id WHERE id IS NOT NULL ON CONFLICT(batch_id) DO UPDATE SET generation=batch_cache_versions.generation+1;
 RETURN NULL;
END $$;
CREATE TRIGGER batch_cache_changed AFTER INSERT OR UPDATE OR DELETE ON credential_batches FOR EACH ROW EXECUTE FUNCTION hysteriax_batch_cache_changed();
CREATE TRIGGER batch_cache_changed AFTER INSERT OR UPDATE OR DELETE ON credential_batch_items FOR EACH ROW EXECUTE FUNCTION hysteriax_batch_cache_changed();
CREATE TRIGGER batch_cache_changed AFTER INSERT OR UPDATE OR DELETE ON dns_credential_batch_items FOR EACH ROW EXECUTE FUNCTION hysteriax_batch_cache_changed();
