WITH boundary AS (SELECT hysteriax_minute($1::timestamptz)+CASE WHEN $1=hysteriax_minute($1) THEN interval '0' ELSE interval '1 minute' END finish)
SELECT coalesce((SELECT total FROM job_status_totals WHERE status='queued'),0)::bigint queued,
 coalesce((SELECT total FROM job_status_totals WHERE status='running'),0)::bigint running,
 (coalesce((SELECT sum(total) FROM job_failed_minute_totals,boundary WHERE minute>=boundary.finish),0)
 +(SELECT count(*) FROM jobs,boundary WHERE status='failed' AND finished_at>=$1 AND finished_at<boundary.finish))::bigint failed
