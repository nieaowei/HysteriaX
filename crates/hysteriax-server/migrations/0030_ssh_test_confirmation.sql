-- Waiting for explicit trust is durable and does not occupy an SSH worker.
ALTER TABLE jobs DROP CONSTRAINT jobs_status_check;
ALTER TABLE jobs ADD CONSTRAINT jobs_status_check CHECK (
    status IN ('queued','running','waiting_confirmation','succeeded','failed','rolled_back','cancelled')
);
