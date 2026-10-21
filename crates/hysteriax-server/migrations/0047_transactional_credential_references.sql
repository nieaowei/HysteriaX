-- Existing encrypted source bytes are retained. Explicit maintenance rebuilds
-- old edges before validating the constraints and publishing runtime readiness.
CREATE UNIQUE INDEX config_version_reference_identity ON config_versions(id,node_id,revision);
ALTER TABLE credential_reference_edges ADD CONSTRAINT reference_edge_shape CHECK (
 (kind='node' AND source IN ('desired','deployed') AND source_id=node_id AND config_version_id IS NULL AND config_revision IS NULL)
 OR (kind='history' AND source='history' AND source_id=config_version_id AND config_version_id IS NOT NULL AND config_revision IS NOT NULL AND config_revision>0)
) NOT VALID;
ALTER TABLE credential_reference_edges ADD CONSTRAINT reference_edge_version
 FOREIGN KEY(credential_id,version) REFERENCES credential_versions(credential_id,version) NOT VALID;
ALTER TABLE credential_reference_edges ADD CONSTRAINT reference_edge_history
 FOREIGN KEY(config_version_id,node_id,config_revision) REFERENCES config_versions(id,node_id,revision)
 ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED NOT VALID;

CREATE OR REPLACE FUNCTION hysteriax_reference_dirty() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE source_kind text; ids text[]:=ARRAY[]::text[]; item text;
BEGIN
 source_kind:=CASE WHEN TG_TABLE_NAME='nodes' THEN 'node' ELSE 'history' END;
 IF TG_OP='DELETE' THEN
  DELETE FROM credential_reference_edges WHERE kind=source_kind AND source_id=OLD.id;
  DELETE FROM credential_reference_dirty WHERE kind=source_kind AND source_id=OLD.id;
  RETURN NULL;
 END IF;
 IF TG_TABLE_NAME='config_versions' THEN
  IF TG_OP='UPDATE' THEN
   IF (OLD.id,OLD.node_id,OLD.revision,OLD.config_enc) IS NOT DISTINCT FROM (NEW.id,NEW.node_id,NEW.revision,NEW.config_enc) THEN RETURN NULL; END IF;
  END IF;
 END IF;
 IF TG_OP='UPDATE' AND OLD.id<>NEW.id THEN ids:=array_append(ids,OLD.id); END IF;
 ids:=array_append(ids,NEW.id);
 FOR item IN SELECT DISTINCT id FROM unnest(ids) id ORDER BY id LOOP
  PERFORM hysteriax_mark_dirty('credential_reference_dirty',jsonb_build_object('kind',source_kind,'source_id',item));
 END LOOP;
 RETURN NULL;
END $$;
CREATE FUNCTION hysteriax_require_transactional_references() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE pending boolean; source_exists boolean;
BEGIN
 IF current_setting('hysteriax.reference_maintenance',true)='on' THEN RETURN NULL; END IF;
 SELECT EXISTS(SELECT 1 FROM credential_reference_dirty d WHERE d.kind=NEW.kind AND d.source_id=NEW.source_id AND d.generation>=NEW.generation) INTO pending;
 IF NOT pending THEN RETURN NULL; END IF;
 IF NEW.kind='node' THEN SELECT EXISTS(SELECT 1 FROM nodes WHERE id=NEW.source_id) INTO source_exists;
 ELSE SELECT EXISTS(SELECT 1 FROM config_versions WHERE id=NEW.source_id) INTO source_exists; END IF;
 IF source_exists THEN
  RAISE EXCEPTION 'configuration references must be indexed in the source transaction'
   USING ERRCODE='23514',CONSTRAINT='reference_source_transaction';
 END IF;
 RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER reference_source_transaction AFTER INSERT OR UPDATE ON credential_reference_dirty
 DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION hysteriax_require_transactional_references();
