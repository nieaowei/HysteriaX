SELECT finished_at, id FROM background_work
WHERE cycle_id IS NOT NULL AND kind IN ('traffic', 'network', 'proxy')
  AND state IN ('complete', 'failed', 'expired') AND finished_at IS NOT NULL
  AND finished_at < $1
  AND (finished_at, id) > (coalesce($2::timestamptz, '-infinity'), coalesce($3::text, ''))
ORDER BY finished_at, id LIMIT $4
