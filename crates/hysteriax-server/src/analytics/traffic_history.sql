-- Compose complete UTC minutes plus exact raw edges for every displayed rolling bucket.
WITH windows AS (
    SELECT bucket,least(bucket+CASE WHEN $1='hour' THEN interval '1 hour' ELSE interval '24 hours' END,$4) finish
    FROM generate_series($3::timestamptz,$4::timestamptz-interval '1 microsecond',CASE WHEN $1='hour' THEN interval '1 hour' ELSE interval '24 hours' END) bucket
), bounds AS (
    SELECT *,hysteriax_minute(bucket)+CASE WHEN bucket=hysteriax_minute(bucket) THEN interval '0' ELSE interval '1 minute' END whole_start,
        hysteriax_minute(finish) whole_finish,
        hysteriax_hour(bucket)+CASE WHEN bucket=hysteriax_hour(bucket) THEN interval '0' ELSE interval '1 hour' END hour_start,
        hysteriax_hour(finish) hour_finish FROM windows
), hours AS (
    SELECT b.bucket,h.node_id,sum(h.tx) tx,sum(h.rx) rx,sum(h.incomplete_samples) bad
    FROM bounds b JOIN traffic_hour_totals h ON h.hour>=b.hour_start AND h.hour<b.hour_finish
    WHERE h.source=$2 AND ($5::text IS NULL OR h.node_id=$5)
      AND NOT EXISTS(SELECT 1 FROM traffic_dirty_hours d WHERE d.source=h.source AND d.node_id=h.node_id AND d.hour=h.hour)
    GROUP BY b.bucket,h.node_id
), minute_ranges AS (
    SELECT bucket,whole_start range_start,least(whole_finish,hour_start) range_end,NULL::text node_id FROM bounds WHERE whole_start<hour_start
    UNION ALL
    SELECT bucket,greatest(whole_start,hour_finish),whole_finish,NULL::text FROM bounds WHERE hour_finish<whole_finish AND hour_finish>=hour_start
    UNION ALL
    SELECT b.bucket,d.hour,d.hour+interval '1 hour',d.node_id FROM bounds b JOIN traffic_dirty_hours d ON d.hour>=b.hour_start AND d.hour<b.hour_finish WHERE d.source=$2 AND ($5::text IS NULL OR d.node_id=$5)
), whole AS (
    SELECT b.bucket,t.node_id,sum(t.tx) tx,sum(t.rx) rx,sum(t.incomplete_samples) bad
    FROM minute_ranges b JOIN traffic_minute_totals t ON t.minute>=b.range_start AND t.minute<b.range_end AND (b.node_id IS NULL OR t.node_id=b.node_id)
    WHERE t.source=$2 AND ($5::text IS NULL OR t.node_id=$5) GROUP BY b.bucket,t.node_id
), edge_ranges AS (
    SELECT bucket,bucket edge_start,least(finish,whole_start) edge_finish FROM bounds WHERE bucket<whole_start
    UNION ALL
    SELECT bucket,whole_finish,finish FROM bounds WHERE whole_finish<finish AND whole_finish>=whole_start
), edge AS (
    SELECT b.bucket,t.node_id,sum(t.delta_tx)::numeric tx,sum(t.delta_rx)::numeric rx,
        count(*) FILTER(WHERE t.gap_reason IS NOT NULL OR t.baseline_only) bad
    FROM edge_ranges b JOIN RAW_TABLE t ON t.sampled_at>=b.edge_start AND t.sampled_at<b.edge_finish
    WHERE ($5::text IS NULL OR t.node_id=$5) GROUP BY b.bucket,t.node_id
), totals AS (SELECT * FROM hours UNION ALL SELECT * FROM whole UNION ALL SELECT * FROM edge)
SELECT bucket,sum(tx)::bigint tx,sum(rx)::bigint rx,count(DISTINCT node_id)::bigint covered_nodes,bool_or(bad>0) incomplete
FROM totals GROUP BY bucket ORDER BY bucket
