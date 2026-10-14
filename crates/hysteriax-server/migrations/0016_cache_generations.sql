-- Committed database generations cover API writers, collectors and direct SQL alike.
CREATE TABLE cache_generation (id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK(id), generation BIGINT NOT NULL DEFAULT 0, configuration_generation BIGINT NOT NULL DEFAULT 0);
INSERT INTO cache_generation(id) VALUES(TRUE);
CREATE FUNCTION hysteriax_cache_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE cache_generation SET generation = generation + 1 WHERE id;
    RETURN NULL;
END;
$$;
DO $$ DECLARE t text; BEGIN
    FOREACH t IN ARRAY ARRAY['nodes','users','config_versions','config_resources','node_assignments',
        'subscription_credentials','credentials','credential_versions','credential_batches','credential_batch_items',
        'dns_credential_batch_items','authorization_groups','authorization_group_users','authorization_group_nodes',
        'dns_connections','dns_zones','dns_records','dns_bindings','node_packages','node_alerts',
        'traffic_records','traffic_baselines','node_network_samples','online_samples','proxy_probe_samples',
        'monitoring_probe_state','data_gaps','jobs','job_events','audit_records'] LOOP
        EXECUTE format('CREATE TRIGGER cache_changed AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_cache_changed()', t);
    END LOOP;
END $$;
CREATE FUNCTION hysteriax_admin_cache_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP='UPDATE' AND (to_jsonb(NEW)-'last_used_at')=(to_jsonb(OLD)-'last_used_at') THEN RETURN NEW; END IF;
    UPDATE cache_generation SET generation=generation+1,configuration_generation=configuration_generation+1 WHERE id;
    RETURN NULL;
END $$;
CREATE TRIGGER admin_cache_changed AFTER INSERT OR UPDATE OR DELETE ON admin_tokens FOR EACH ROW EXECUTE FUNCTION hysteriax_admin_cache_changed();

CREATE FUNCTION hysteriax_configuration_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE cache_generation SET configuration_generation=configuration_generation+1 WHERE id;
    RETURN NULL;
END $$;
DO $$ DECLARE t text; BEGIN
    FOREACH t IN ARRAY ARRAY['config_versions','config_resources','node_assignments','subscription_credentials',
        'credentials','credential_versions','credential_batches','credential_batch_items','dns_credential_batch_items',
        'authorization_groups','authorization_group_users','authorization_group_nodes','dns_connections','dns_zones','dns_records','dns_bindings'] LOOP
        EXECUTE format('CREATE TRIGGER configuration_changed AFTER INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_configuration_changed()',t);
    END LOOP;
END $$;
CREATE FUNCTION hysteriax_directory_row_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE old_data jsonb; new_data jsonb;
BEGIN
    IF TG_OP='UPDATE' THEN
        old_data:=to_jsonb(OLD);new_data:=to_jsonb(NEW);
        IF TG_TABLE_NAME='nodes' THEN
            old_data:=old_data-ARRAY['last_seen_at','last_sample_at','updated_at'];
            new_data:=new_data-ARRAY['last_seen_at','last_sample_at','updated_at'];
        ELSIF TG_TABLE_NAME='users' THEN
            old_data:=old_data-ARRAY['usage_bytes','updated_at','access_kick_enqueued_at'];
            new_data:=new_data-ARRAY['usage_bytes','updated_at','access_kick_enqueued_at'];
        ELSIF TG_TABLE_NAME='node_packages' THEN
            old_data:=jsonb_build_object('config',OLD.config,'generation',OLD.generation);
            new_data:=jsonb_build_object('config',NEW.config,'generation',NEW.generation);
        END IF;
        IF old_data=new_data THEN RETURN NULL; END IF;
    END IF;
    UPDATE cache_generation SET configuration_generation=configuration_generation+1 WHERE id;
    RETURN NULL;
END $$;
CREATE TRIGGER directory_changed AFTER INSERT OR UPDATE OR DELETE ON nodes FOR EACH ROW EXECUTE FUNCTION hysteriax_directory_row_changed();
CREATE TRIGGER directory_changed AFTER INSERT OR UPDATE OR DELETE ON users FOR EACH ROW EXECUTE FUNCTION hysteriax_directory_row_changed();
CREATE TRIGGER directory_changed AFTER INSERT OR UPDATE OR DELETE ON node_packages FOR EACH ROW EXECUTE FUNCTION hysteriax_directory_row_changed();
