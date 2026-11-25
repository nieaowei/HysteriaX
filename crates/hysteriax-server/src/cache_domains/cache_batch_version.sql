SELECT coalesce((SELECT generation FROM batch_cache_versions WHERE batch_id=b.id),0) revision,
 coalesce((SELECT generation FROM batch_cache_versions WHERE batch_id=b.id)::text,'0') digest,
 NOT EXISTS(SELECT 1 FROM jobs j JOIN credential_batch_items i ON i.job_id=j.id WHERE i.batch_id=b.id AND j.status IN ('queued','running'))
 AND NOT EXISTS(SELECT 1 FROM jobs j JOIN dns_credential_batch_items i ON i.job_id=j.id WHERE i.batch_id=b.id AND j.status IN ('queued','running'))
 -- A CA batch still projects live eligibility, trust, deadlines and node tasks
 -- after publication. It is never an immutable terminal ordinary batch: use
 -- domain-versioned short caching so both memory and Redis see these changes.
 AND NOT EXISTS(SELECT 1 FROM mtls_rotations r WHERE r.batch_id=b.id) terminal
FROM credential_batches b WHERE b.id=$1
