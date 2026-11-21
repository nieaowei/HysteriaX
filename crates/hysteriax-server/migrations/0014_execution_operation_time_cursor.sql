-- Keep unresolved execution-operation pages ordered by their creation time.
CREATE INDEX execution_operations_time_recheck_page
    ON execution_operations (created_at DESC, operation_id DESC)
    WHERE state IN ('dispatching', 'reconciling', 'needs_attention');

CREATE INDEX execution_operations_work_time_recheck_page
    ON execution_operations (work_id, created_at DESC, operation_id DESC)
    WHERE state IN ('dispatching', 'reconciling', 'needs_attention');
