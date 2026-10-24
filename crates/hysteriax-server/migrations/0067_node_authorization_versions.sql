-- A node-local source fence for bounded deployment identity preparation. It is
-- independent of cache/monitoring generations and never rewrites business rows.
CREATE TABLE node_authorization_versions (
    node_id text PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    generation bigint NOT NULL DEFAULT 0 CHECK (generation >= 0)
);
INSERT INTO node_authorization_versions(node_id) SELECT id FROM nodes;

CREATE FUNCTION hysteriax_node_authorization_created() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO node_authorization_versions(node_id) VALUES(NEW.id);
    RETURN NULL;
END $$;
CREATE TRIGGER node_authorization_created AFTER INSERT ON nodes
FOR EACH ROW EXECUTE FUNCTION hysteriax_node_authorization_created();

CREATE FUNCTION hysteriax_node_authorization_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE old_node text; new_node text; target text;
BEGIN
    IF TG_OP='UPDATE' AND (OLD.user_id,OLD.node_id) IS NOT DISTINCT FROM (NEW.user_id,NEW.node_id) THEN
        RETURN NULL;
    END IF;
    IF TG_OP<>'INSERT' THEN old_node:=OLD.node_id; END IF;
    IF TG_OP<>'DELETE' THEN new_node:=NEW.node_id; END IF;
    FOR target IN SELECT DISTINCT id FROM unnest(ARRAY[old_node,new_node]) id WHERE id IS NOT NULL ORDER BY id LOOP
        -- A cascading node deletion can already have removed its version row.
        UPDATE node_authorization_versions SET generation=generation+1 WHERE node_id=target;
    END LOOP;
    RETURN NULL;
END $$;
CREATE TRIGGER node_authorization_changed AFTER INSERT OR UPDATE OR DELETE ON node_assignments
FOR EACH ROW EXECUTE FUNCTION hysteriax_node_authorization_changed();

CREATE FUNCTION hysteriax_node_authorizations_truncated() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE target text;
BEGIN
    FOR target IN SELECT node_id FROM node_authorization_versions ORDER BY node_id LOOP
        UPDATE node_authorization_versions SET generation=generation+1 WHERE node_id=target;
    END LOOP;
    RETURN NULL;
END $$;
CREATE TRIGGER node_authorizations_truncated AFTER TRUNCATE ON node_assignments
FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_node_authorizations_truncated();
