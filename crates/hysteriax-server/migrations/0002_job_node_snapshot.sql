ALTER TABLE jobs ADD COLUMN node_name TEXT;

UPDATE jobs
SET node_name = nodes.name
FROM nodes
WHERE jobs.node_id = nodes.id;

-- Keep historical node identifiers even after the node is removed.
ALTER TABLE jobs DROP CONSTRAINT jobs_node_id_fkey;
