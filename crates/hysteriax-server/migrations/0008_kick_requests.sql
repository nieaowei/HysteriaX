-- A request survives user deletion and failed execution attempts.
CREATE TABLE kick_requests (
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    reasons JSONB NOT NULL,
    generation BIGINT NOT NULL DEFAULT 1,
    state TEXT NOT NULL CHECK (state IN ('active','waiting_recovery','needs_attention','completed','cancelled')),
    latest_job_id TEXT REFERENCES jobs(id),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (node_id, user_id)
);

-- Legacy jobs do not reliably identify revocation vs restriction. Preserve the
-- obligation conservatively, including requests for users that were deleted.
INSERT INTO kick_requests(node_id,user_id,reasons,state,latest_job_id)
SELECT node_id,payload_json->>'user_id',
       jsonb_object_agg(CASE WHEN payload_json->>'node_limit'='true' THEN 'node_limit' ELSE COALESCE((
           SELECT CASE a.action WHEN 'user.deleted' THEN 'user_deleted' WHEN 'user.updated' THEN 'access_restricted'
             WHEN 'user.credentials_rotated' THEN 'credentials_revoked' WHEN 'user.unassigned' THEN 'unassigned' END
           FROM audit_records a WHERE a.entity_id=j.payload_json->>'user_id'
           AND a.action IN ('user.deleted','user.updated','user.credentials_rotated','user.unassigned')
           AND abs(extract(epoch FROM a.created_at-j.created_at)) < 1
           ORDER BY abs(extract(epoch FROM a.created_at-j.created_at)) LIMIT 1
       ),'legacy_revocation') END, true),
       'active',(array_agg(id ORDER BY CASE WHEN status IN ('queued','running') THEN 0 ELSE 1 END, CASE WHEN status IN ('queued','running') THEN created_at END, created_at DESC,id))[1]
FROM jobs j WHERE kind='kick' AND status IN ('queued','running','failed')
AND node_id IS NOT NULL AND payload_json->>'user_id' IS NOT NULL
AND (status <> 'failed' OR NOT EXISTS(SELECT 1 FROM jobs done WHERE done.kind='kick' AND done.node_id=j.node_id AND done.payload_json->>'user_id'=j.payload_json->>'user_id' AND done.status='succeeded' AND done.created_at>j.created_at))
GROUP BY node_id,payload_json->>'user_id';

UPDATE jobs j SET payload_json=j.payload_json || jsonb_build_object('kick_generation',r.generation,'kick_reasons',r.reasons)
FROM kick_requests r WHERE j.id=r.latest_job_id;

WITH cancelled AS (
 UPDATE jobs j SET status='cancelled',stage='superseded',finished_at=now(),updated_at=now()
 FROM kick_requests r WHERE j.kind='kick' AND j.node_id=r.node_id
 AND j.payload_json->>'user_id'=r.user_id AND j.id<>r.latest_job_id AND j.status IN ('queued','running')
 RETURNING j.id,j.node_id,j.stage
)
INSERT INTO job_events(job_id,event_type,payload_json,created_at)
SELECT id,'job.cancelled',jsonb_build_object('id',id,'node_id',node_id,'status','cancelled','stage',stage,'reason','duplicate kick request'),now() FROM cancelled;

WITH exhausted AS (
 UPDATE jobs j SET status='failed',stage=CASE WHEN error_message LIKE 'SSH connection failed:%' THEN 'waiting_recovery' ELSE 'needs_attention' END,finished_at=now(),updated_at=now()
 FROM kick_requests r WHERE j.id=r.latest_job_id AND (j.attempts>=5 OR j.status='failed')
 RETURNING j.id,j.node_id,j.stage,j.error_message
)
INSERT INTO job_events(job_id,event_type,payload_json,created_at)
SELECT id,'job.failed',jsonb_build_object('id',id,'node_id',node_id,'status','failed','stage',stage,'error',error_message),now() FROM exhausted;

UPDATE kick_requests r SET state=j.stage FROM jobs j WHERE j.id=r.latest_job_id AND j.stage IN ('waiting_recovery','needs_attention');

CREATE UNIQUE INDEX jobs_active_kick_idx ON jobs(node_id,(payload_json->>'user_id'))
WHERE kind='kick' AND status IN ('queued','running');
