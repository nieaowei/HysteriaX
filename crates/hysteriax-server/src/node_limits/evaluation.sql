WITH input AS MATERIALIZED (
 SELECT * FROM jsonb_to_recordset($1::jsonb) AS e(
  node_id text,usage_bytes bigint,period_id text,next_reset_at timestamptz,
  next_evaluation_at timestamptz,restricted boolean,changed boolean,
  audit_id text,alerts jsonb,reasons jsonb)
), packages AS (
 UPDATE node_packages p SET usage_bytes=e.usage_bytes,period_id=e.period_id,
  next_reset_at=e.next_reset_at,next_evaluation_at=e.next_evaluation_at,restricted=e.restricted
 FROM input e WHERE p.node_id=e.node_id AND
  (p.usage_bytes,p.period_id,p.next_reset_at,p.next_evaluation_at,p.restricted)
  IS DISTINCT FROM (e.usage_bytes,e.period_id,e.next_reset_at,e.next_evaluation_at,e.restricted)
 RETURNING p.node_id
), inactive AS (
 UPDATE node_alerts a SET active=false FROM input e WHERE a.node_id=e.node_id AND a.active
 AND NOT EXISTS(SELECT 1 FROM jsonb_array_elements(e.alerts) d WHERE d->>'kind'=a.kind AND d->>'scope'=a.scope)
), alerts AS (
 INSERT INTO node_alerts(id,node_id,kind,scope,created_at)
 SELECT d->>'id',e.node_id,d->>'kind',d->>'scope',$2 FROM input e
 CROSS JOIN LATERAL jsonb_array_elements(e.alerts) d
 ORDER BY e.node_id,d->>'kind',d->>'scope'
 ON CONFLICT(node_id,kind,scope) DO UPDATE SET active=true WHERE NOT node_alerts.active
), epochs AS (
 INSERT INTO node_limit_epochs(node_id,generation)
 SELECT node_id,1 FROM input WHERE changed ORDER BY node_id
 ON CONFLICT(node_id) DO UPDATE SET generation=node_limit_epochs.generation+1
 RETURNING node_id,generation
), audit AS (
 INSERT INTO audit_records(id,actor,action,entity_type,entity_id,detail_json,created_at)
 SELECT audit_id,'system',CASE WHEN restricted THEN 'node.restricted' ELSE 'node.restored' END,
  'node',node_id,jsonb_build_object('reasons',reasons),$2 FROM input WHERE changed
)
INSERT INTO background_work(id,kind,payload)
 SELECT 'node-limit:'||e.node_id||':'||g.generation,$3,
 jsonb_build_object('node_id',e.node_id,'restriction_generation',g.generation,'restricted',e.restricted,'after_user',null)
 FROM input e JOIN epochs g USING(node_id)
