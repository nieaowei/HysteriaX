WITH windows AS (
    SELECT bucket,least(bucket+CASE WHEN $1='hour' THEN interval '1 hour' ELSE interval '24 hours' END,$3) finish
    FROM generate_series($2::timestamptz,$3::timestamptz-interval '1 microsecond',CASE WHEN $1='hour' THEN interval '1 hour' ELSE interval '24 hours' END) bucket
), bounds AS (
    SELECT *,hysteriax_minute(bucket)+CASE WHEN bucket=hysteriax_minute(bucket) THEN interval '0' ELSE interval '1 minute' END whole_start,
        hysteriax_minute(finish) whole_finish FROM windows
), whole AS (
    SELECT b.bucket,sum(p.attempts) attempts,sum(p.successes) successes
    FROM bounds b JOIN probe_minute_totals p ON p.minute>=b.whole_start AND p.minute<b.whole_finish
    WHERE ($4::text IS NULL OR p.node_id=$4) GROUP BY b.bucket
) , edge_ranges AS (
    SELECT bucket,bucket edge_start,least(finish,whole_start) edge_finish FROM bounds WHERE bucket<whole_start
    UNION ALL
    SELECT bucket,whole_finish,finish FROM bounds WHERE whole_finish<finish AND whole_finish>=whole_start
), edge AS (
    SELECT b.bucket,count(*) attempts,count(*) FILTER(WHERE p.status='ok' AND p.external_status IS DISTINCT FROM 'failed') successes
    FROM edge_ranges b JOIN proxy_probe_samples p ON p.sampled_at>=b.edge_start AND p.sampled_at<b.edge_finish
    WHERE ($4::text IS NULL OR p.node_id=$4) AND p.status IN ('ok','failed')
    GROUP BY b.bucket
), counts AS (
    SELECT bucket,sum(attempts)::bigint attempts,sum(successes)::bigint successes FROM (SELECT * FROM whole UNION ALL SELECT * FROM edge) parts GROUP BY bucket
), latency AS (
    SELECT date_bin(CASE WHEN $1='hour' THEN interval '1 hour' ELSE interval '24 hours' END,sampled_at,$2) bucket,
        percentile_cont(0.5) WITHIN GROUP(ORDER BY latency_ms) p50,
        percentile_cont(0.95) WITHIN GROUP(ORDER BY latency_ms) p95
    FROM proxy_probe_samples WHERE sampled_at >= $2 AND sampled_at < $3 AND status='ok'
      AND external_status IS DISTINCT FROM 'failed' AND ($4::text IS NULL OR node_id=$4)
    GROUP BY bucket
)
SELECT c.bucket,c.attempts,c.successes,l.p50,l.p95 FROM counts c LEFT JOIN latency l USING(bucket) ORDER BY c.bucket
