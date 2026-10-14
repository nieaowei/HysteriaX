CREATE TABLE probe_minute_totals (
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    minute TIMESTAMPTZ NOT NULL, attempts BIGINT NOT NULL, successes BIGINT NOT NULL,
    PRIMARY KEY(node_id,minute)
);
CREATE INDEX probe_minute_window ON probe_minute_totals(minute,node_id);
INSERT INTO probe_minute_totals
SELECT node_id,hysteriax_minute(sampled_at),count(*),count(*) FILTER(WHERE status='ok' AND external_status IS DISTINCT FROM 'failed')
FROM proxy_probe_samples WHERE status IN ('ok','failed') GROUP BY node_id,hysteriax_minute(sampled_at);
CREATE FUNCTION hysteriax_probe_totals() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
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
CREATE TRIGGER probe_totals AFTER INSERT OR UPDATE OR DELETE ON proxy_probe_samples FOR EACH ROW EXECUTE FUNCTION hysteriax_probe_totals();
