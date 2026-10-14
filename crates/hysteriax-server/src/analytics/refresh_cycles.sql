WITH samples AS MATERIALIZED (
    SELECT * FROM online_samples WHERE cycle_id=ANY($1)
), global_base AS (
    SELECT cycle_id,min(cycle_started_at) started_at,
        count(*) FILTER(WHERE status='ok') covered,max(expected_nodes) expected,
        bool_or(status!='ok') OR count(*) FILTER(WHERE status='ok')<max(expected_nodes) incomplete,
        count(*) FILTER(WHERE traffic_status='ok') traffic_covered,
        count(*) FILTER(WHERE traffic_status='ok')<max(expected_nodes) traffic_incomplete
    FROM samples GROUP BY cycle_id
), members AS MATERIALIZED (
    SELECT s.cycle_id,s.node_id,e.key,e.value::numeric connections
    FROM samples s LEFT JOIN LATERAL jsonb_each_text(s.connections) e ON true WHERE s.status='ok'
), global_counts AS (
    SELECT cycle_id,count(DISTINCT key) users,coalesce(sum(connections),0) connections FROM members GROUP BY cycle_id
), node_counts AS (
    SELECT cycle_id,node_id,count(DISTINCT key) users,coalesce(sum(connections),0) connections FROM members GROUP BY cycle_id,node_id
)
INSERT INTO online_cycle_metrics
SELECT b.cycle_id,'',b.started_at,c.users,c.connections,b.covered,b.expected,b.incomplete,b.traffic_covered,b.traffic_incomplete
FROM global_base b LEFT JOIN global_counts c USING(cycle_id)
UNION ALL
SELECT s.cycle_id,s.node_id,s.cycle_started_at,c.users,c.connections,
    (s.status='ok')::integer,1,s.status!='ok',(s.traffic_status='ok')::integer,s.traffic_status!='ok'
FROM samples s LEFT JOIN node_counts c USING(cycle_id,node_id)
