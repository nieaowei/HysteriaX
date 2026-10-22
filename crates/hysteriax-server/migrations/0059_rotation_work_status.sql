-- Completion must include queued, running and failed publication metadata, not
-- only due candidates. Scope the check to one rotation without scanning history.
CREATE INDEX background_work_rotation_status
 ON background_work(kind,(payload->>'rotation_id'),state,id)
 WHERE cycle_id IS NULL AND node_id IS NULL;
