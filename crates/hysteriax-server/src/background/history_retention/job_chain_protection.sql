SELECT CASE
    WHEN NOT EXISTS (SELECT 1 FROM jobs WHERE id=$3 AND retry_of_job_id IS NULL)
      OR EXISTS (
          SELECT 1 FROM jobs j WHERE j.retry_of_job_id=ANY($1) AND j.id<>ALL($1)
      ) OR EXISTS (
          SELECT 1 FROM jobs j WHERE j.id=ANY($1) AND j.id<>$3
            AND (j.retry_of_job_id IS NULL OR j.retry_of_job_id<>ALL($1))
      ) THEN 'chain_changed'
    WHEN EXISTS (
        SELECT 1 FROM jobs j WHERE j.id=ANY($1) AND (
            j.status NOT IN ('succeeded','failed','rolled_back','cancelled')
            OR j.finished_at IS NULL OR j.finished_at >= $2
            OR j.stage IN ('needs_attention','reconciliation_required')
        )
    ) THEN 'job_active_recent_or_unresolved'
    WHEN EXISTS (SELECT 1 FROM job_events e WHERE e.job_id=ANY($1)) THEN 'events_retained'
    WHEN EXISTS (SELECT 1 FROM credential_application_receipts r WHERE r.job_id=ANY($1))
      OR EXISTS (SELECT 1 FROM credential_batch_items i WHERE i.job_id=ANY($1))
      OR EXISTS (SELECT 1 FROM deployment_probe_tokens p WHERE p.job_id=ANY($1))
      OR EXISTS (SELECT 1 FROM dns_credential_batch_items i WHERE i.job_id=ANY($1))
      OR EXISTS (SELECT 1 FROM dns_operations o WHERE o.job_id=ANY($1))
      OR EXISTS (SELECT 1 FROM kick_requests r WHERE r.latest_job_id=ANY($1))
      OR EXISTS (SELECT 1 FROM mtls_activations a WHERE a.job_id=ANY($1))
      OR EXISTS (SELECT 1 FROM mtls_cleanup_nodes c WHERE c.job_id=ANY($1))
      OR EXISTS (SELECT 1 FROM mtls_node_operations o WHERE o.job_id=ANY($1))
      OR EXISTS (SELECT 1 FROM mtls_rotation_nodes r WHERE r.job_id=ANY($1))
      OR EXISTS (SELECT 1 FROM authorization_changes a WHERE a.job_id=ANY($1))
      THEN 'business_reference'
    WHEN EXISTS (
        SELECT 1 FROM unnest($1::text[]) j(id)
        JOIN execution_operations o ON o.work_id=j.id OR o.resource_key='job:'||j.id
    ) OR EXISTS (
        SELECT 1 FROM unnest($1::text[]) j(id)
        JOIN resource_leases l ON l.work_id=j.id OR l.resource_key='job:'||j.id
        WHERE l.owner IS NOT NULL OR l.blocked
    ) OR EXISTS (
        SELECT 1 FROM jobs j JOIN nodes n ON n.id=j.node_id
        WHERE j.id=ANY($1) AND j.kind='record-remove' AND n.state IN ('deleting','delete_failed')
    ) THEN 'execution_evidence'
END
