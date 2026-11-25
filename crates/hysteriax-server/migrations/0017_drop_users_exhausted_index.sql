-- Remove partial index referencing usage_bytes so high-frequency billing updates
-- on users retain PostgreSQL HOT updates (fillfactor = 80 from migration 0015).
DROP INDEX IF EXISTS users_enabled_exhausted_idx;
