CREATE TABLE credential_reference_dirty (
    kind TEXT NOT NULL CHECK(kind IN ('node','history')), source_id TEXT NOT NULL,
    PRIMARY KEY(kind,source_id)
);
CREATE TABLE credential_reference_edges (
    kind TEXT NOT NULL, source_id TEXT NOT NULL,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    config_version_id TEXT REFERENCES config_versions(id) ON DELETE CASCADE,
    config_revision BIGINT, source TEXT NOT NULL, field TEXT NOT NULL,
    credential_id TEXT NOT NULL, version BIGINT NOT NULL,
    PRIMARY KEY(kind,source_id,source,field,credential_id,version)
);
CREATE INDEX credential_reference_lookup ON credential_reference_edges(credential_id,source,node_id);
CREATE FUNCTION hysteriax_reference_dirty() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE kind text; source_id text;
BEGIN
    kind:=CASE WHEN TG_TABLE_NAME='nodes' THEN 'node' ELSE 'history' END;
    source_id:=CASE WHEN TG_OP='DELETE' THEN OLD.id ELSE NEW.id END;
    INSERT INTO credential_reference_dirty VALUES(kind,source_id) ON CONFLICT DO NOTHING;
    RETURN NULL;
END $$;
CREATE TRIGGER refs_dirty_insert AFTER INSERT OR DELETE ON nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_reference_dirty();
CREATE TRIGGER refs_dirty_update AFTER UPDATE OF desired_config_enc,deployed_config_enc ON nodes FOR EACH ROW
    WHEN (OLD.desired_config_enc IS DISTINCT FROM NEW.desired_config_enc OR OLD.deployed_config_enc IS DISTINCT FROM NEW.deployed_config_enc)
    EXECUTE FUNCTION hysteriax_reference_dirty();
CREATE TRIGGER refs_dirty_history AFTER INSERT OR UPDATE OR DELETE ON config_versions FOR EACH ROW EXECUTE FUNCTION hysteriax_reference_dirty();
INSERT INTO credential_reference_dirty SELECT 'node',id FROM nodes;
INSERT INTO credential_reference_dirty SELECT 'history',id FROM config_versions;
