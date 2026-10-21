-- Avoid rebuilding statistics that are being removed with their parent node.
CREATE OR REPLACE FUNCTION hysteriax_flow_totals() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE s text;
BEGIN
    -- A deleted node's rollups have their own ON DELETE CASCADE. Updating them
    -- per sample only repeats work while holding the global writer lock.
    IF TG_OP='DELETE' AND NOT EXISTS(SELECT 1 FROM nodes WHERE id=OLD.node_id) THEN
        RETURN NULL;
    END IF;
    s:=CASE WHEN TG_TABLE_NAME='traffic_records' THEN 'users' ELSE 'network' END;
    IF TG_OP<>'INSERT' THEN
        PERFORM hysteriax_flow_adjust(s,OLD.node_id,OLD.sampled_at,OLD.delta_tx,OLD.delta_rx,(OLD.gap_reason IS NOT NULL OR OLD.baseline_only)::integer,-1);
        IF s='users' THEN
            UPDATE user_node_usage SET tx=tx-OLD.delta_tx,rx=rx-OLD.delta_rx,samples=samples-1 WHERE user_id=OLD.user_id AND node_id=OLD.node_id;
            DELETE FROM user_node_usage WHERE user_id=OLD.user_id AND node_id=OLD.node_id AND samples=0;
            UPDATE user_node_usage SET sampled_at=coalesce((SELECT max(sampled_at) FROM traffic_records WHERE user_id=OLD.user_id AND node_id=OLD.node_id),sampled_at)
            WHERE user_id=OLD.user_id AND node_id=OLD.node_id AND sampled_at=OLD.sampled_at;
        END IF;
    END IF;
    IF TG_OP<>'DELETE' THEN
        PERFORM hysteriax_flow_adjust(s,NEW.node_id,NEW.sampled_at,NEW.delta_tx,NEW.delta_rx,(NEW.gap_reason IS NOT NULL OR NEW.baseline_only)::integer,1);
        IF s='users' THEN
            INSERT INTO user_node_usage VALUES(NEW.user_id,NEW.node_id,NEW.delta_tx,NEW.delta_rx,1,NEW.sampled_at)
            ON CONFLICT(user_id,node_id) DO UPDATE SET tx=user_node_usage.tx+EXCLUDED.tx,rx=user_node_usage.rx+EXCLUDED.rx,
                samples=user_node_usage.samples+1,sampled_at=greatest(user_node_usage.sampled_at,EXCLUDED.sampled_at);
        END IF;
    END IF;
    RETURN NULL;
END $$;

CREATE OR REPLACE FUNCTION hysteriax_probe_totals() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    -- A deleted node's rollups have their own ON DELETE CASCADE. Updating them
    -- per sample only repeats work while holding the global writer lock.
    IF TG_OP='DELETE' AND NOT EXISTS(SELECT 1 FROM nodes WHERE id=OLD.node_id) THEN
        RETURN NULL;
    END IF;
    IF TG_OP<>'INSERT' AND OLD.status IN ('ok','failed') THEN
        UPDATE probe_minute_totals SET attempts=attempts-1,
            successes=successes-(OLD.status='ok' AND OLD.external_status IS DISTINCT FROM 'failed')::integer
        WHERE node_id=OLD.node_id AND minute=hysteriax_minute(OLD.sampled_at);
        DELETE FROM probe_minute_totals WHERE node_id=OLD.node_id AND minute=hysteriax_minute(OLD.sampled_at) AND attempts=0;
    END IF;
    IF TG_OP<>'DELETE' AND NEW.status IN ('ok','failed') THEN
        INSERT INTO probe_minute_totals VALUES(NEW.node_id,hysteriax_minute(NEW.sampled_at),1,(NEW.status='ok' AND NEW.external_status IS DISTINCT FROM 'failed')::integer)
        ON CONFLICT(node_id,minute) DO UPDATE SET attempts=probe_minute_totals.attempts+1,successes=probe_minute_totals.successes+EXCLUDED.successes;
    END IF;
    RETURN NULL;
END $$;
