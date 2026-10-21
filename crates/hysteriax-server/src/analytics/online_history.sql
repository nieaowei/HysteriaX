WITH dirty_ids AS MATERIALIZED (
 SELECT d.cycle_id FROM analytics_dirty_cycles d
 WHERE EXISTS(SELECT 1 FROM online_samples s WHERE s.cycle_id=d.cycle_id AND s.cycle_started_at>=hysteriax_minute($3) AND s.cycle_started_at<$4 AND ($5::text IS NULL OR s.node_id=$5))
    OR EXISTS(SELECT 1 FROM online_cycle_metrics c WHERE c.cycle_id=d.cycle_id AND c.scope_node_id=coalesce($5,'') AND c.started_at>=hysteriax_minute($3) AND c.started_at<$4)
), dirty_values AS MATERIALIZED (
 SELECT * FROM hysteriax_online_cycle_values(ARRAY(SELECT cycle_id FROM dirty_ids))
 WHERE scope_node_id=coalesce($5,'') AND started_at>=hysteriax_minute($3) AND started_at<$4
), dirty_minutes AS MATERIALIZED (
 SELECT minute FROM online_dirty_minutes WHERE scope_node_id=coalesce($5,'') AND minute>=hysteriax_minute($3) AND minute<$4
 UNION SELECT hysteriax_minute(started_at) FROM dirty_values
 UNION SELECT hysteriax_minute(c.started_at) FROM dirty_ids d CROSS JOIN LATERAL (
  SELECT c.started_at FROM online_cycle_metrics c WHERE c.cycle_id=d.cycle_id AND c.scope_node_id=coalesce($5,'') AND c.started_at>=hysteriax_minute($3) AND c.started_at<$4 OFFSET 0
 ) c
), windows AS (
 SELECT bucket,least(bucket+CASE WHEN $1='hour' THEN interval '1 hour' ELSE interval '24 hours' END,$4) finish
 FROM generate_series($3::timestamptz,$4::timestamptz-interval '1 microsecond',CASE WHEN $1='hour' THEN interval '1 hour' ELSE interval '24 hours' END) bucket
), bounds AS (
 SELECT *,hysteriax_minute(bucket)+CASE WHEN bucket=hysteriax_minute(bucket) THEN interval '0' ELSE interval '1 minute' END whole_start,hysteriax_minute(finish) whole_finish FROM windows
), whole AS (
 SELECT b.bucket,m.samples,m.user_cycles,m.users_sum,m.users_peak,m.connection_cycles,m.connections_sum,m.connections_peak,m.covered,m.expected,m.incomplete,m.traffic_covered,m.traffic_incomplete
 FROM bounds b JOIN online_minute_totals m ON m.minute>=b.whole_start AND m.minute<b.whole_finish
 WHERE m.scope_node_id=coalesce($5,'') AND NOT EXISTS(SELECT 1 FROM dirty_minutes d WHERE d.minute=m.minute)
), ranges AS (
 SELECT bucket,bucket range_start,least(finish,whole_start) range_end FROM bounds WHERE bucket<whole_start
 UNION ALL SELECT bucket,whole_finish,finish FROM bounds WHERE whole_finish<finish AND whole_finish>=whole_start
 UNION ALL SELECT b.bucket,d.minute,d.minute+interval '1 minute' FROM bounds b JOIN dirty_minutes d ON d.minute>=b.whole_start AND d.minute<b.whole_finish
), edges AS (
 SELECT r.bucket,1::bigint samples,(c.users IS NOT NULL)::int user_cycles,coalesce(c.users,0)::numeric users_sum,c.users users_peak,(c.connections IS NOT NULL)::int connection_cycles,coalesce(c.connections,0) connections_sum,c.connections connections_peak,c.covered,c.expected,c.incomplete,c.traffic_covered,c.traffic_incomplete
 -- Keep the scan parameterized by each actual edge/dirty range. OFFSET 0
 -- prevents flattening this LATERAL join into a scan of all cycle metrics
 -- when ranges is empty (the usual aligned, fully aggregated window).
 FROM ranges r CROSS JOIN LATERAL (
  SELECT c.* FROM online_cycle_metrics c
  WHERE c.scope_node_id=coalesce($5,'') AND c.started_at>=r.range_start AND c.started_at<r.range_end AND NOT EXISTS(SELECT 1 FROM dirty_ids d WHERE d.cycle_id=c.cycle_id)
  UNION ALL SELECT * FROM dirty_values c WHERE c.started_at>=r.range_start AND c.started_at<r.range_end
  OFFSET 0
 ) c
), all_values AS (SELECT * FROM whole UNION ALL SELECT * FROM edges)
SELECT bucket,(sum(users_sum)/nullif(sum(user_cycles),0))::double precision users_avg,max(users_peak)::double precision users_peak,
 (sum(connections_sum)/nullif(sum(connection_cycles),0))::double precision connections_avg,max(connections_peak)::double precision connections_peak,
 min(covered)::bigint covered_nodes,bool_or(incomplete) incomplete,min(traffic_covered)::bigint covered,max(expected)::bigint expected,bool_or(traffic_incomplete) traffic_incomplete
FROM all_values GROUP BY bucket ORDER BY bucket
