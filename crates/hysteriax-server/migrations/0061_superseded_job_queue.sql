-- A bounded pending range for one rotation. Keep this derived flag in the
-- transaction that changes its authoritative job, including manual cancels.
ALTER TABLE mtls_node_operations ADD COLUMN pending_job BOOLEAN NOT NULL DEFAULT FALSE;
UPDATE mtls_node_operations o SET pending_job=j.status IN ('queued','waiting_confirmation')
 FROM jobs j WHERE j.id=o.job_id;
CREATE FUNCTION hysteriax_mtls_operation_pending_job() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 SELECT status IN ('queued','waiting_confirmation') INTO NEW.pending_job FROM jobs WHERE id=NEW.job_id;
 RETURN NEW;
END $$;
CREATE TRIGGER mtls_operation_pending_job BEFORE INSERT OR UPDATE OF job_id ON mtls_node_operations
 FOR EACH ROW EXECUTE FUNCTION hysteriax_mtls_operation_pending_job();
CREATE FUNCTION hysteriax_mtls_job_pending() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 UPDATE mtls_node_operations SET pending_job=NEW.status IN ('queued','waiting_confirmation')
 WHERE job_id=NEW.id AND pending_job IS DISTINCT FROM (NEW.status IN ('queued','waiting_confirmation'));
 RETURN NEW;
END $$;
CREATE TRIGGER mtls_job_pending AFTER UPDATE OF status ON jobs
 FOR EACH ROW WHEN (OLD.status IS DISTINCT FROM NEW.status) EXECUTE FUNCTION hysteriax_mtls_job_pending();
CREATE INDEX mtls_rotation_pending_jobs ON mtls_node_operations(rotation_id,node_id,job_id)
 WHERE pending_job AND rotation_id IS NOT NULL;

-- The committed supersession fact prevents execution before physical status
-- and cancellation events have been materialized in bounded pages.
CREATE VIEW mtls_superseded_node_jobs AS
 SELECT o.job_id FROM mtls_node_operations o JOIN mtls_rotations r ON r.id=o.rotation_id
 WHERE r.outcome IS NOT NULL;
