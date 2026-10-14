CREATE FUNCTION hysteriax_hour(v timestamptz) RETURNS timestamptz LANGUAGE sql IMMUTABLE AS $$ SELECT date_bin(interval '1 hour',v,'2000-01-01 00:00:00+00'::timestamptz) $$;
CREATE TABLE traffic_hour_totals(source TEXT NOT NULL,node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,hour timestamptz NOT NULL,tx numeric NOT NULL,rx numeric NOT NULL,samples bigint NOT NULL,incomplete_samples bigint NOT NULL,PRIMARY KEY(source,node_id,hour));
CREATE INDEX traffic_hour_window ON traffic_hour_totals(source,hour,node_id);
CREATE TABLE traffic_dirty_hours(source TEXT NOT NULL,node_id TEXT NOT NULL,hour timestamptz NOT NULL,PRIMARY KEY(source,node_id,hour));
INSERT INTO traffic_hour_totals SELECT source,node_id,hysteriax_hour(minute),sum(tx),sum(rx),sum(samples),sum(incomplete_samples) FROM traffic_minute_totals GROUP BY source,node_id,hysteriax_hour(minute);
CREATE FUNCTION hysteriax_hour_dirty() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP<>'INSERT' THEN INSERT INTO traffic_dirty_hours VALUES(OLD.source,OLD.node_id,hysteriax_hour(OLD.minute)) ON CONFLICT DO NOTHING; END IF;
 IF TG_OP<>'DELETE' THEN INSERT INTO traffic_dirty_hours VALUES(NEW.source,NEW.node_id,hysteriax_hour(NEW.minute)) ON CONFLICT DO NOTHING; END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER hour_dirty AFTER INSERT OR UPDATE OR DELETE ON traffic_minute_totals FOR EACH ROW EXECUTE FUNCTION hysteriax_hour_dirty();
CREATE TABLE online_minute_totals(scope_node_id TEXT NOT NULL,minute timestamptz NOT NULL,samples bigint NOT NULL,user_cycles bigint NOT NULL,users_sum numeric NOT NULL,users_peak bigint,connection_cycles bigint NOT NULL,connections_sum numeric NOT NULL,connections_peak numeric,covered bigint NOT NULL,expected bigint NOT NULL,incomplete boolean NOT NULL,traffic_covered bigint NOT NULL,traffic_incomplete boolean NOT NULL,PRIMARY KEY(scope_node_id,minute));
CREATE TABLE online_dirty_minutes(scope_node_id TEXT NOT NULL,minute timestamptz NOT NULL,PRIMARY KEY(scope_node_id,minute));
INSERT INTO online_minute_totals SELECT scope_node_id,hysteriax_minute(started_at),count(*),count(users),coalesce(sum(users),0),max(users),count(connections),coalesce(sum(connections),0),max(connections),min(covered),max(expected),bool_or(incomplete),min(traffic_covered),bool_or(traffic_incomplete) FROM online_cycle_metrics GROUP BY scope_node_id,hysteriax_minute(started_at);
CREATE FUNCTION hysteriax_online_minute_dirty() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF TG_OP<>'INSERT' THEN INSERT INTO online_dirty_minutes VALUES(OLD.scope_node_id,hysteriax_minute(OLD.started_at)) ON CONFLICT DO NOTHING; END IF;
 IF TG_OP<>'DELETE' THEN INSERT INTO online_dirty_minutes VALUES(NEW.scope_node_id,hysteriax_minute(NEW.started_at)) ON CONFLICT DO NOTHING; END IF;
 RETURN NULL;
END $$;
CREATE TRIGGER online_minute_dirty AFTER INSERT OR UPDATE OR DELETE ON online_cycle_metrics FOR EACH ROW EXECUTE FUNCTION hysteriax_online_minute_dirty();
