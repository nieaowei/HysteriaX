CREATE INDEX background_work_retention_idx ON background_work (finished_at, id)
    WHERE cycle_id IS NOT NULL AND kind IN ('traffic', 'network', 'proxy')
      AND state IN ('complete', 'failed', 'expired') AND finished_at IS NOT NULL;

-- Includes verified operations. They also retain a foreign key to the lease.
CREATE INDEX execution_operations_resource_idx ON execution_operations (resource_key);

CREATE INDEX traffic_sample_batches_unsettled_cycle_idx ON traffic_sample_batches (cycle_id)
    WHERE state IN ('capturing', 'ready')
       OR (state = 'complete' AND (completed_at IS NULL OR processed_count <> item_count));
