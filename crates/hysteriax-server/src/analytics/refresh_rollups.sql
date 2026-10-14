DELETE FROM traffic_hour_totals h USING traffic_dirty_hours d WHERE h.source=d.source AND h.node_id=d.node_id AND h.hour=d.hour;
INSERT INTO traffic_hour_totals
SELECT m.source,m.node_id,d.hour,sum(m.tx),sum(m.rx),sum(m.samples),sum(m.incomplete_samples)
FROM traffic_dirty_hours d JOIN traffic_minute_totals m ON m.source=d.source AND m.node_id=d.node_id AND m.minute>=d.hour AND m.minute<d.hour+interval '1 hour'
GROUP BY m.source,m.node_id,d.hour;
DELETE FROM traffic_dirty_hours;
DELETE FROM online_minute_totals m USING online_dirty_minutes d WHERE m.scope_node_id=d.scope_node_id AND m.minute=d.minute;
INSERT INTO online_minute_totals
SELECT c.scope_node_id,d.minute,count(*),count(c.users),coalesce(sum(c.users),0),max(c.users),count(c.connections),coalesce(sum(c.connections),0),max(c.connections),min(c.covered),max(c.expected),bool_or(c.incomplete),min(c.traffic_covered),bool_or(c.traffic_incomplete)
FROM online_dirty_minutes d JOIN online_cycle_metrics c ON c.scope_node_id=d.scope_node_id AND c.started_at>=d.minute AND c.started_at<d.minute+interval '1 minute'
GROUP BY c.scope_node_id,d.minute;
DELETE FROM online_dirty_minutes;
