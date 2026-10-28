-- The one-time cleanup has finished; its progress records are no longer needed.
-- Drop the dependent table first, without cascading to any other relation.
DROP TABLE traffic_history_cleanup_nodes;
DROP TABLE traffic_history_cleanup_runs;
