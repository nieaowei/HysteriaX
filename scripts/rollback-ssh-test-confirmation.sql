-- Downgrading cannot resume suspended tests; preserve their cancellation history.
BEGIN;
WITH cancelled AS (
    UPDATE jobs SET status='cancelled', stage='superseded', finished_at=now(), updated_at=now()
    WHERE status='waiting_confirmation' RETURNING id, kind, node_id
)
INSERT INTO job_events(job_id,event_type,payload_json,created_at)
SELECT id,'job.cancelled',jsonb_build_object('id',id,'kind',kind,'node_id',node_id,'status','cancelled','stage','superseded'),now() FROM cancelled;
ALTER TABLE jobs DROP CONSTRAINT jobs_status_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_status_check CHECK (
    status IN ('queued','running','succeeded','failed','rolled_back','cancelled')
);
DELETE FROM _sqlx_migrations WHERE version=30;
COMMIT;
