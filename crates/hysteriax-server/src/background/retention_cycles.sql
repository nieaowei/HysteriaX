SELECT c.id, CASE
    WHEN c.deadline_at >= $2 OR c.kind NOT IN ('traffic', 'network', 'proxy')
        THEN 'cycle_ineligible'
    WHEN EXISTS (SELECT 1 FROM background_work w WHERE w.cycle_id = c.id)
        THEN 'work_remains'
    WHEN EXISTS (SELECT 1 FROM traffic_sample_batches b WHERE b.cycle_id = c.id
        AND (b.state IN ('capturing', 'ready')
            OR (b.state = 'complete' AND (b.completed_at IS NULL OR b.processed_count <> b.item_count))))
        THEN 'traffic_batch_unsettled'
    WHEN EXISTS (SELECT 1 FROM online_samples s WHERE s.cycle_id = c.id AND s.pending)
        THEN 'online_pending'
    ELSE NULL
END AS skip_reason
FROM sampling_cycles c
WHERE c.id = ANY($1::uuid[])
