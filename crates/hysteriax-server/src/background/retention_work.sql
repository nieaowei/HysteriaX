SELECT w.id, CASE
    WHEN w.owner IS NOT NULL OR w.deadline_at IS NULL OR w.deadline_at >= $2
        OR w.cycle_id IS NULL OR w.node_id IS NULL OR w.id <> w.cycle_id::text || ':' || w.node_id
        OR w.finished_at IS NULL OR w.finished_at >= $2
        OR w.kind NOT IN ('traffic', 'network', 'proxy')
        OR w.state NOT IN ('complete', 'failed', 'expired')
        THEN 'work_ineligible'
    WHEN EXISTS (SELECT 1 FROM resource_leases attached WHERE attached.work_id = w.id)
        OR (l.resource_key IS NOT NULL AND (
            l.owner IS NOT NULL OR l.expires_at IS NOT NULL OR l.blocked OR l.generation <> w.generation))
        THEN 'lease_retained'
    WHEN EXISTS (SELECT 1 FROM execution_operations o WHERE o.work_id = w.id)
        OR EXISTS (SELECT 1 FROM execution_operations o WHERE o.resource_key = 'work:' || w.id)
        THEN 'execution_evidence'
    WHEN EXISTS (SELECT 1 FROM online_samples s WHERE s.cycle_id = w.cycle_id
        AND s.node_id = w.node_id AND s.pending)
        THEN 'online_pending'
    WHEN b.sample_id IS NOT NULL AND (
        w.kind <> 'traffic' OR b.node_id IS DISTINCT FROM w.node_id OR b.cycle_id IS DISTINCT FROM w.cycle_id
        OR NOT (
            (b.state = 'complete' AND b.completed_at IS NOT NULL AND b.processed_count = b.item_count)
            OR (b.state = 'discarded' AND b.completed_at < $2 AND b.completed_at IS NOT NULL
                AND b.clear_requested_at IS NULL AND b.payload_digest IS NULL
                AND b.processed_count = 0 AND b.item_count = 0
                AND NOT EXISTS (SELECT 1 FROM traffic_sample_items i WHERE i.sample_id = b.sample_id)
                AND NOT EXISTS (SELECT 1 FROM traffic_records r WHERE r.sample_id = b.sample_id))
        )) THEN 'traffic_batch_retained'
    WHEN (l.resource_key IS NOT NULL AND NOT (w.id = ANY($3::text[])))
        OR (b.sample_id IS NOT NULL AND NOT (w.id = ANY($4::text[])))
        THEN 'related_row_locked'
    ELSE NULL
END AS skip_reason, l.resource_key, coalesce(b.state = 'discarded', false) AS discard_batch
FROM background_work w
LEFT JOIN resource_leases l ON l.resource_key = 'work:' || w.id
LEFT JOIN traffic_sample_batches b ON b.sample_id = w.id
WHERE w.id = ANY($1::text[])
