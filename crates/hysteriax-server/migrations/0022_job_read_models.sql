CREATE TABLE job_status_totals(status text PRIMARY KEY,total bigint NOT NULL);
CREATE TABLE job_node_kick_totals(node_id text PRIMARY KEY,pending bigint NOT NULL,pending_limit bigint NOT NULL,failed_limit bigint NOT NULL);
CREATE TABLE job_failed_minute_totals(minute timestamptz PRIMARY KEY,total bigint NOT NULL);
INSERT INTO job_status_totals SELECT status,count(*) FROM jobs GROUP BY status;
INSERT INTO job_node_kick_totals SELECT node_id,count(*) FILTER(WHERE status IN ('queued','running')),count(*) FILTER(WHERE status IN ('queued','running') AND payload_json->>'node_limit'='true'),count(*) FILTER(WHERE status='failed' AND payload_json->>'node_limit'='true') FROM jobs WHERE kind='kick' AND node_id IS NOT NULL GROUP BY node_id;
INSERT INTO job_failed_minute_totals SELECT hysteriax_minute(finished_at),count(*) FROM jobs WHERE status='failed' AND finished_at IS NOT NULL GROUP BY hysteriax_minute(finished_at);
CREATE INDEX jobs_failed_finished ON jobs(finished_at,id) WHERE status='failed';
CREATE FUNCTION hysteriax_adjust_job(j jobs,sign bigint) RETURNS void LANGUAGE plpgsql AS $$
DECLARE p bigint;pl bigint;f bigint;
BEGIN
 IF sign>0 THEN INSERT INTO job_status_totals VALUES(j.status,1) ON CONFLICT(status) DO UPDATE SET total=job_status_totals.total+1;
 ELSE UPDATE job_status_totals SET total=total-1 WHERE status=j.status; DELETE FROM job_status_totals WHERE status=j.status AND total=0; END IF;
 IF j.kind='kick' AND j.node_id IS NOT NULL THEN
  p:=(j.status IN ('queued','running'))::int;pl:=p*coalesce((j.payload_json->>'node_limit'='true')::int,0);f:=coalesce((j.status='failed' AND j.payload_json->>'node_limit'='true')::int,0);
  IF sign>0 THEN INSERT INTO job_node_kick_totals VALUES(j.node_id,p,pl,f) ON CONFLICT(node_id) DO UPDATE SET pending=job_node_kick_totals.pending+EXCLUDED.pending,pending_limit=job_node_kick_totals.pending_limit+EXCLUDED.pending_limit,failed_limit=job_node_kick_totals.failed_limit+EXCLUDED.failed_limit;
  ELSE UPDATE job_node_kick_totals SET pending=pending-p,pending_limit=pending_limit-pl,failed_limit=failed_limit-f WHERE node_id=j.node_id; END IF;
 END IF;
 IF j.status='failed' AND j.finished_at IS NOT NULL THEN
  IF sign>0 THEN INSERT INTO job_failed_minute_totals VALUES(hysteriax_minute(j.finished_at),1) ON CONFLICT(minute) DO UPDATE SET total=job_failed_minute_totals.total+1;
  ELSE UPDATE job_failed_minute_totals SET total=total-1 WHERE minute=hysteriax_minute(j.finished_at);DELETE FROM job_failed_minute_totals WHERE minute=hysteriax_minute(j.finished_at) AND total=0; END IF;
 END IF;
END $$;
CREATE FUNCTION hysteriax_job_totals_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP='UPDATE' AND (OLD.status,OLD.kind,OLD.node_id,OLD.payload_json->>'node_limit',OLD.finished_at) IS NOT DISTINCT FROM (NEW.status,NEW.kind,NEW.node_id,NEW.payload_json->>'node_limit',NEW.finished_at) THEN RETURN NULL; END IF;
 IF TG_OP<>'INSERT' THEN PERFORM hysteriax_adjust_job(OLD,-1);END IF;
 IF TG_OP<>'DELETE' THEN PERFORM hysteriax_adjust_job(NEW,1);END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER job_totals_changed AFTER INSERT OR UPDATE OR DELETE ON jobs FOR EACH ROW EXECUTE FUNCTION hysteriax_job_totals_changed();
