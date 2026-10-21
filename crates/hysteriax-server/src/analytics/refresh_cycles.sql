INSERT INTO online_cycle_metrics
SELECT * FROM hysteriax_online_cycle_values($1)
ON CONFLICT(cycle_id,scope_node_id) DO UPDATE SET
    started_at=EXCLUDED.started_at, users=EXCLUDED.users,
    connections=EXCLUDED.connections, covered=EXCLUDED.covered,
    expected=EXCLUDED.expected, incomplete=EXCLUDED.incomplete,
    traffic_covered=EXCLUDED.traffic_covered, traffic_incomplete=EXCLUDED.traffic_incomplete
WHERE (online_cycle_metrics.started_at,online_cycle_metrics.users,online_cycle_metrics.connections,
       online_cycle_metrics.covered,online_cycle_metrics.expected,online_cycle_metrics.incomplete,
       online_cycle_metrics.traffic_covered,online_cycle_metrics.traffic_incomplete)
    IS DISTINCT FROM
      (EXCLUDED.started_at,EXCLUDED.users,EXCLUDED.connections,EXCLUDED.covered,EXCLUDED.expected,
       EXCLUDED.incomplete,EXCLUDED.traffic_covered,EXCLUDED.traffic_incomplete)
