-- Maintenance rotates this epoch even when a restored lease counter is reused.
CREATE FUNCTION hysteriax_execution_epoch() RETURNS text
LANGUAGE sql STABLE AS $$ SELECT epoch FROM cache_version_epoch WHERE id $$;
ALTER TABLE resource_leases
    ADD COLUMN epoch text NOT NULL DEFAULT hysteriax_execution_epoch();

ALTER TABLE execution_operations
    ADD COLUMN epoch text NOT NULL DEFAULT hysteriax_execution_epoch();

-- This metadata follows the node identifier just as it follows node deletion.
ALTER TABLE node_authorization_versions DROP CONSTRAINT node_authorization_versions_node_id_fkey;
ALTER TABLE node_authorization_versions ADD CONSTRAINT node_authorization_versions_node_id_fkey
    FOREIGN KEY(node_id) REFERENCES nodes(id) ON DELETE CASCADE ON UPDATE CASCADE;
