SELECT n.id FROM node_packages p JOIN nodes n ON n.id=p.node_id
 WHERE p.next_evaluation_at<=statement_timestamp()
 AND n.state NOT IN ('deleting','delete_failed')
 ORDER BY p.next_evaluation_at,p.node_id LIMIT $1
 FOR NO KEY UPDATE OF n SKIP LOCKED
