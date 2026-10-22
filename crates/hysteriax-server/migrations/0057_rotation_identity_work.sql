-- Bounded direct advancement of one rotation shares the same issuance work as
-- asynchronous schedulers; retain every existing work and its fencing metadata.
CREATE INDEX background_work_rotation_identity_due
 ON background_work(kind,(payload->>'rotation_id'),available_at,id)
 WHERE cycle_id IS NULL AND node_id IS NULL AND state IN ('queued','running');
