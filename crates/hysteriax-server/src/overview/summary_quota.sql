-- Use the same effective usage as package snapshots when a reset is due.
-- Exact numeric ratios and stable name/ID ties keep the top five deterministic.
SELECT n.id
FROM nodes n JOIN node_packages p ON p.node_id = n.id
WHERE (p.config->>'quota_bytes')::bigint > 0
ORDER BY
    (CASE WHEN p.next_reset_at <= $1 THEN 0 ELSE p.usage_bytes END)::numeric
        / (p.config->>'quota_bytes')::numeric DESC,
    lower(n.name) COLLATE "C", n.name COLLATE "C", n.id
LIMIT 5
