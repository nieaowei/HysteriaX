-- Encrypted configuration sources remain unchanged; maintenance derives these
-- pins using the same source queue and commit guard as credential references.
CREATE UNIQUE INDEX config_resource_reference_identity ON config_resources(id,node_id);
CREATE TABLE config_resource_references (
 kind text NOT NULL,
 source_id text NOT NULL,
 node_id text NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
 config_version_id text,
 config_revision bigint,
 source text NOT NULL,
 resource_id text NOT NULL,
 PRIMARY KEY(kind,source_id,source,resource_id),
 CONSTRAINT resource_reference_shape CHECK (
  (kind='node' AND source IN ('desired','deployed') AND source_id=node_id AND config_version_id IS NULL AND config_revision IS NULL)
  OR (kind='history' AND source='history' AND source_id=config_version_id AND config_version_id IS NOT NULL AND config_revision IS NOT NULL AND config_revision>0)
 ),
 CONSTRAINT resource_reference_resource FOREIGN KEY(resource_id,node_id) REFERENCES config_resources(id,node_id),
 CONSTRAINT resource_reference_history FOREIGN KEY(config_version_id,node_id,config_revision)
  REFERENCES config_versions(id,node_id,revision) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX resource_reference_lookup ON config_resource_references(resource_id,node_id);
CREATE INDEX resource_reference_node ON config_resource_references(node_id,kind,source_id);
CREATE INDEX resource_reference_history ON config_resource_references(config_version_id,node_id,config_revision) WHERE config_version_id IS NOT NULL;
CREATE INDEX credential_reference_node ON credential_reference_edges(node_id,kind,source_id);
CREATE INDEX credential_reference_history ON credential_reference_edges(config_version_id,node_id,config_revision) WHERE config_version_id IS NOT NULL;
CREATE FUNCTION hysteriax_resource_source_deleted() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 DELETE FROM config_resource_references WHERE kind=CASE WHEN TG_TABLE_NAME='nodes' THEN 'node' ELSE 'history' END AND source_id=OLD.id;
 RETURN NULL;
END $$;
CREATE TRIGGER resource_source_deleted AFTER DELETE ON nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_resource_source_deleted();
CREATE TRIGGER resource_source_deleted AFTER DELETE ON config_versions FOR EACH ROW EXECUTE FUNCTION hysteriax_resource_source_deleted();
-- Include already-clean sources: this is a new projection, not a runtime scan.
SELECT hysteriax_mark_dirty('credential_reference_dirty',jsonb_build_object('kind','node','source_id',id)) FROM nodes ORDER BY id;
SELECT hysteriax_mark_dirty('credential_reference_dirty',jsonb_build_object('kind','history','source_id',id)) FROM config_versions ORDER BY id;
