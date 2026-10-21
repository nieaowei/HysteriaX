WITH dirty AS (SELECT * FROM jsonb_to_recordset($1::jsonb) AS d(source text,node_id text,hour timestamptz))
DELETE FROM traffic_hour_totals WHERE ctid=ANY(ARRAY(
 SELECT h.ctid FROM dirty d CROSS JOIN LATERAL (
  SELECT h.ctid FROM traffic_hour_totals h
  WHERE h.source=d.source AND h.node_id=d.node_id AND h.hour=d.hour OFFSET 0
 ) h
));
WITH dirty AS (SELECT * FROM jsonb_to_recordset($1::jsonb) AS d(source text,node_id text,hour timestamptz))
INSERT INTO traffic_hour_totals
SELECT m.source,m.node_id,d.hour,sum(m.tx),sum(m.rx),sum(m.samples),sum(m.incomplete_samples)
FROM dirty d CROSS JOIN LATERAL (
 SELECT m.* FROM traffic_minute_totals m
 WHERE m.source=d.source AND m.node_id=d.node_id AND m.minute>=d.hour AND m.minute<d.hour+interval '1 hour'
 OFFSET 0
) m
GROUP BY m.source,m.node_id,d.hour;
WITH dirty AS (SELECT * FROM jsonb_to_recordset($1::jsonb) AS d(scope_node_id text,minute timestamptz))
DELETE FROM online_minute_totals WHERE ctid=ANY(ARRAY(
 SELECT m.ctid FROM dirty d CROSS JOIN LATERAL (
  SELECT m.ctid FROM online_minute_totals m
  WHERE m.scope_node_id=d.scope_node_id AND m.minute=d.minute OFFSET 0
 ) m
));
WITH dirty AS (SELECT * FROM jsonb_to_recordset($1::jsonb) AS d(scope_node_id text,minute timestamptz))
INSERT INTO online_minute_totals
SELECT c.scope_node_id,d.minute,count(*),count(c.users),coalesce(sum(c.users),0),max(c.users),count(c.connections),coalesce(sum(c.connections),0),max(c.connections),min(c.covered),max(c.expected),bool_or(c.incomplete),min(c.traffic_covered),bool_or(c.traffic_incomplete)
FROM dirty d CROSS JOIN LATERAL (
 SELECT c.* FROM online_cycle_metrics c
 WHERE c.scope_node_id=d.scope_node_id AND c.started_at>=d.minute AND c.started_at<d.minute+interval '1 minute'
 OFFSET 0
) c
GROUP BY c.scope_node_id,d.minute;
