WITH items AS (
 UPDATE traffic_sample_items SET processed_at=clock_timestamp()
 WHERE sample_id=$1 AND user_id=ANY($2::text[]) AND processed_at IS NULL
), finished AS (
 UPDATE traffic_sample_batches SET processed_count=$3,state=CASE WHEN $4 THEN 'complete' ELSE 'ready' END,
  completed_at=CASE WHEN $4 THEN clock_timestamp() ELSE NULL END WHERE sample_id=$1
 RETURNING node_id,cycle_id,sampled_at,sample_order
), node_update AS (
 UPDATE nodes n SET state=CASE WHEN n.state='unreachable' THEN 'deployed' ELSE n.state END,
  last_seen_at=greatest(n.last_seen_at,b.sampled_at),last_sample_at=greatest(n.last_sample_at,b.sampled_at),updated_at=clock_timestamp()
 FROM finished b WHERE n.id=b.node_id AND $4 AND NOT EXISTS(
  SELECT 1 FROM traffic_sample_batches newer WHERE newer.node_id=b.node_id
  AND newer.sample_order>b.sample_order AND newer.state='complete')
), online AS (
 UPDATE online_samples s SET traffic_status=$5 FROM finished b
 WHERE $4 AND s.node_id=b.node_id AND s.cycle_id=b.cycle_id
)
UPDATE data_gaps g SET resolved_at=clock_timestamp() FROM finished b
 WHERE $4 AND $5='ok' AND g.node_id=b.node_id AND g.resolved_at IS NULL AND g.opened_at<=b.sampled_at
