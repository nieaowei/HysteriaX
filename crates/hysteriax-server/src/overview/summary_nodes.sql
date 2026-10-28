WITH node_jobs AS (
    SELECT COALESCE(j.node_id,r.node_id) AS node_id,
        COALESCE(j.pending,0) AS pending_revocations,
        COALESCE(r.pending_disconnects,0) AS pending_disconnects,
        COALESCE(r.failed_disconnects,0) AS failed_disconnects
    FROM job_node_kick_totals j
    FULL JOIN (
        SELECT node_id,count(*) FILTER(WHERE state='active') AS pending_disconnects,
            count(*) FILTER(WHERE state IN ('waiting_recovery','needs_attention')) AS failed_disconnects
        FROM kick_requests WHERE reasons ? 'node_limit' AND state NOT IN ('completed','cancelled')
        GROUP BY node_id
    ) r ON r.node_id=j.node_id
), gaps AS (
    SELECT node_id, count(*)::bigint AS open_gaps
    FROM data_gaps WHERE resolved_at IS NULL GROUP BY node_id
), alerts AS (
    SELECT node_id,
        jsonb_agg(jsonb_build_object('id', id, 'kind', kind, 'created_at', created_at) ORDER BY created_at, id) AS alerts
    FROM node_alerts WHERE active GROUP BY node_id
)
SELECT n.id, n.name, n.state, n.desired_revision AS revision, n.deployed_revision,
    n.updated_at, n.last_sample_at, acme.status AS acme_status,
    COALESCE(g.open_gaps, 0) AS open_gaps,
    COALESCE(j.pending_revocations, 0) AS pending_revocations,
    p.node_id IS NOT NULL AS has_package,
    COALESCE(p.config, '{}'::jsonb) AS config,
    COALESCE(p.usage_bytes, 0) AS usage_bytes,
    p.next_reset_at, p.interface, p.sampled_at, p.gap_reason,
    COALESCE(a.alerts, '[]'::jsonb) AS alerts,
    COALESCE(j.pending_disconnects, 0) AS pending_disconnects,
    COALESCE(j.failed_disconnects, 0) AS failed_disconnects,
    o.sampled_at AS online_sampled_at, o.status AS online_status, o.connections AS online_connections,
    probe.sampled_at AS probe_sampled_at, probe.revision AS probe_revision,
    probe.status AS probe_status, probe.external_status AS probe_external_status,
    probe.connection_ms AS probe_connection_ms, probe.latency_ms AS probe_latency_ms,
    probe.reason AS probe_reason, health.health AS probe_health
FROM nodes n
LEFT JOIN node_acme_observations acme ON acme.node_id=n.id AND acme.revision=n.deployed_revision AND acme.deployment_digest=n.deployed_content_sha256
LEFT JOIN node_packages p ON p.node_id = n.id
LEFT JOIN node_jobs j ON j.node_id = n.id
LEFT JOIN gaps g ON g.node_id = n.id
LEFT JOIN alerts a ON a.node_id = n.id
LEFT JOIN LATERAL (
    SELECT sampled_at, status, connections FROM online_samples
    WHERE node_id = n.id ORDER BY sampled_at DESC NULLS LAST, id DESC LIMIT 1
) o ON true
LEFT JOIN LATERAL (
    SELECT sampled_at, revision, status, external_status, connection_ms, latency_ms, reason
    FROM proxy_probe_samples WHERE node_id = n.id ORDER BY sampled_at DESC NULLS LAST LIMIT 1
) probe ON true
LEFT JOIN monitoring_probe_state health ON health.node_id = n.id AND health.revision = probe.revision
WHERE ($1::text[] IS NULL OR n.id=ANY($1))
ORDER BY lower(n.name) COLLATE "C", n.name COLLATE "C", n.id
