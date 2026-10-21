-- Restriction episodes are independent of meter/config generations. Editing a
-- still-restricted package must not invalidate its unfinished revocation pages.
CREATE TABLE node_limit_epochs (
    node_id text PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
    generation bigint NOT NULL CHECK (generation > 0)
);
CREATE INDEX kick_requests_node_limit_page_idx ON kick_requests(node_id,user_id)
WHERE reasons ? 'node_limit' AND state NOT IN ('completed','cancelled');
