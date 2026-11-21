SELECT period_start, id FROM sampling_cycles
WHERE period_start < $1
  AND (period_start, id) > (coalesce($2::timestamptz, '-infinity'),
     coalesce($3::uuid, '00000000-0000-0000-0000-000000000000'))
ORDER BY period_start, id LIMIT $4
