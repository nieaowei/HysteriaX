CREATE FUNCTION hysteriax_minute(v timestamptz) RETURNS timestamptz LANGUAGE sql IMMUTABLE AS $$
    SELECT date_bin(interval '1 minute',v,'2000-01-01 00:00:00+00'::timestamptz)
$$;
CREATE TABLE traffic_minute_totals (
    source TEXT NOT NULL CHECK(source IN ('users','network')),
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    minute TIMESTAMPTZ NOT NULL, tx NUMERIC NOT NULL, rx NUMERIC NOT NULL,
    samples BIGINT NOT NULL, incomplete_samples BIGINT NOT NULL,
    PRIMARY KEY(source,node_id,minute)
);
CREATE INDEX traffic_minute_window ON traffic_minute_totals(source,minute,node_id);
CREATE TABLE user_node_usage (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    tx NUMERIC NOT NULL, rx NUMERIC NOT NULL, samples BIGINT NOT NULL,
    sampled_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY(user_id,node_id)
);
INSERT INTO traffic_minute_totals
SELECT 'users',node_id,hysteriax_minute(sampled_at),sum(delta_tx),sum(delta_rx),count(*),count(*) FILTER(WHERE gap_reason IS NOT NULL OR baseline_only)
FROM traffic_records GROUP BY node_id,hysteriax_minute(sampled_at);
INSERT INTO traffic_minute_totals
SELECT 'network',node_id,hysteriax_minute(sampled_at),sum(delta_tx),sum(delta_rx),count(*),count(*) FILTER(WHERE gap_reason IS NOT NULL OR baseline_only)
FROM node_network_samples GROUP BY node_id,hysteriax_minute(sampled_at);
INSERT INTO user_node_usage SELECT user_id,node_id,sum(delta_tx),sum(delta_rx),count(*),max(sampled_at) FROM traffic_records GROUP BY user_id,node_id;
CREATE FUNCTION hysteriax_flow_adjust(s text,n text,at_time timestamptz,tx_delta bigint,rx_delta bigint,bad bigint,sign bigint) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    IF sign>0 THEN
        INSERT INTO traffic_minute_totals VALUES(s,n,hysteriax_minute(at_time),tx_delta,rx_delta,1,bad)
        ON CONFLICT(source,node_id,minute) DO UPDATE SET tx=traffic_minute_totals.tx+EXCLUDED.tx,
            rx=traffic_minute_totals.rx+EXCLUDED.rx,samples=traffic_minute_totals.samples+1,
            incomplete_samples=traffic_minute_totals.incomplete_samples+EXCLUDED.incomplete_samples;
    ELSE
        UPDATE traffic_minute_totals SET tx=tx-tx_delta,rx=rx-rx_delta,samples=samples-1,incomplete_samples=incomplete_samples-bad
        WHERE source=s AND node_id=n AND minute=hysteriax_minute(at_time);
        DELETE FROM traffic_minute_totals WHERE source=s AND node_id=n AND minute=hysteriax_minute(at_time) AND samples=0;
    END IF;
END $$;
CREATE FUNCTION hysteriax_flow_totals() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE s text;
BEGIN
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
CREATE TRIGGER flow_totals AFTER INSERT OR UPDATE OR DELETE ON traffic_records FOR EACH ROW EXECUTE FUNCTION hysteriax_flow_totals();
CREATE TRIGGER flow_totals AFTER INSERT OR UPDATE OR DELETE ON node_network_samples FOR EACH ROW EXECUTE FUNCTION hysteriax_flow_totals();

-- Global cycle metrics are computed once per changed cycle, not once per history request.
CREATE TABLE analytics_dirty_cycles(cycle_id UUID PRIMARY KEY);
CREATE TABLE online_cycle_metrics (
    cycle_id UUID NOT NULL, scope_node_id TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    users BIGINT, connections NUMERIC,
    covered BIGINT NOT NULL, expected BIGINT NOT NULL, incomplete BOOLEAN NOT NULL,
    traffic_covered BIGINT NOT NULL, traffic_incomplete BOOLEAN NOT NULL,
    PRIMARY KEY(cycle_id,scope_node_id)
);
CREATE INDEX online_cycle_window ON online_cycle_metrics(scope_node_id,started_at);
CREATE FUNCTION hysteriax_cycle_dirty() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP<>'INSERT' THEN INSERT INTO analytics_dirty_cycles VALUES(OLD.cycle_id) ON CONFLICT DO NOTHING; END IF;
    IF TG_OP<>'DELETE' THEN INSERT INTO analytics_dirty_cycles VALUES(NEW.cycle_id) ON CONFLICT DO NOTHING; END IF;
    RETURN NULL;
END $$;
CREATE TRIGGER cycle_dirty AFTER INSERT OR UPDATE OR DELETE ON online_samples FOR EACH ROW EXECUTE FUNCTION hysteriax_cycle_dirty();
INSERT INTO analytics_dirty_cycles SELECT DISTINCT cycle_id FROM online_samples;
