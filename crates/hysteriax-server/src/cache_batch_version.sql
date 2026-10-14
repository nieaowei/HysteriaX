SELECT (SELECT generation FROM batch_cache_versions WHERE batch_id=b.id) revision,md5(jsonb_build_array(to_jsonb(b),
 (SELECT jsonb_agg(jsonb_build_array(i.node_id,i.user_id,i.job_id,j.status,j.stage,j.error_message,n.name) ORDER BY i.node_id,i.user_id) FROM credential_batch_items i JOIN jobs j ON j.id=i.job_id LEFT JOIN nodes n ON n.id=i.node_id WHERE i.batch_id=b.id),
 (SELECT jsonb_agg(jsonb_build_array(i.connection_id,i.job_id,c.name,j.status,j.stage,j.error_message) ORDER BY c.name,i.connection_id) FROM dns_credential_batch_items i JOIN dns_connections c ON c.id=i.connection_id JOIN jobs j ON j.id=i.job_id WHERE i.batch_id=b.id)
)::text) digest,
 NOT EXISTS(SELECT 1 FROM jobs j JOIN credential_batch_items i ON i.job_id=j.id WHERE i.batch_id=b.id AND j.status IN ('queued','running'))
 AND NOT EXISTS(SELECT 1 FROM jobs j JOIN dns_credential_batch_items i ON i.job_id=j.id WHERE i.batch_id=b.id AND j.status IN ('queued','running')) terminal
FROM credential_batches b WHERE b.id=$1
