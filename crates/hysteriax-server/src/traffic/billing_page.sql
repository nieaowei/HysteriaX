WITH billed AS MATERIALIZED (
 SELECT * FROM jsonb_to_recordset($5::jsonb) AS b(
  record_id text,user_id text,tx_total bigint,rx_total bigint,
  delta_tx bigint,delta_rx bigint,delta_bytes bigint,gap_reason text)
), baselines AS (
 INSERT INTO traffic_baselines(node_id,user_id,instance_id,tx_total,rx_total,sampled_at,sample_order)
 SELECT $1,user_id,$2,tx_total,rx_total,$3,$4 FROM billed ORDER BY user_id
 ON CONFLICT(node_id,user_id) DO UPDATE SET instance_id=EXCLUDED.instance_id,
  tx_total=EXCLUDED.tx_total,rx_total=EXCLUDED.rx_total,sampled_at=EXCLUDED.sampled_at,sample_order=EXCLUDED.sample_order
), records AS (
 INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,
  delta_tx,delta_rx,gap_reason,sampled_at,baseline_only,sample_id)
 SELECT record_id,$1,user_id,$2,tx_total,rx_total,delta_tx,delta_rx,gap_reason,$3,false,$6
 FROM billed ORDER BY user_id
)
UPDATE users u SET usage_bytes=u.usage_bytes+least(b.delta_bytes,9223372036854775807-u.usage_bytes),
 updated_at=clock_timestamp() FROM billed b WHERE u.id=b.user_id
