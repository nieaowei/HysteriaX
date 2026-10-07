CREATE TABLE authorization_groups (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 180),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX authorization_groups_name_idx
    ON authorization_groups(lower(name), name, id);

CREATE TABLE authorization_group_users (
    group_id TEXT NOT NULL REFERENCES authorization_groups(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (group_id, user_id)
);
CREATE INDEX authorization_group_users_user_idx
    ON authorization_group_users(user_id, group_id);

CREATE TABLE authorization_group_nodes (
    group_id TEXT NOT NULL REFERENCES authorization_groups(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (group_id, node_id)
);
CREATE INDEX authorization_group_nodes_node_idx
    ON authorization_group_nodes(node_id, group_id);

-- Preview tokens bind a proposed mutation to the exact related state shown to
-- the operator. The guard is recomputed before commit under the global write
-- lock, so stale user, group, node, or credential state cannot be applied.
CREATE TABLE authorization_group_previews (
    token TEXT PRIMARY KEY NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('create', 'update', 'delete', 'update_memberships')),
    group_id TEXT REFERENCES authorization_groups(id) ON DELETE CASCADE,
    user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
    request_json JSONB NOT NULL CHECK (jsonb_typeof(request_json) = 'object'),
    guard_json JSONB NOT NULL CHECK (jsonb_typeof(guard_json) = 'object'),
    result_json JSONB NOT NULL CHECK (jsonb_typeof(result_json) = 'object'),
    created_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    CHECK ((action = 'update_memberships') = (user_id IS NOT NULL)),
    CHECK ((action IN ('update', 'delete')) = (group_id IS NOT NULL))
);
CREATE INDEX authorization_group_previews_expiry_idx
    ON authorization_group_previews(expires_at);

-- Keep the migration non-disruptive: every non-empty existing node set is
-- represented by one group, while node_assignments (and its credentials,
-- certificate bindings, subscriptions, and traffic history) is untouched.
WITH user_sets AS (
    SELECT user_id, array_agg(node_id ORDER BY node_id)::TEXT[] AS node_ids
    FROM node_assignments
    GROUP BY user_id
), grouped_sets AS (
    SELECT node_ids,
           array_agg(user_id ORDER BY user_id)::TEXT[] AS user_ids,
           row_number() OVER (ORDER BY array_to_string(node_ids, chr(31))) AS ordinal
    FROM user_sets
    GROUP BY node_ids
)
INSERT INTO authorization_groups (id, name, revision, created_at, updated_at)
SELECT 'migration-' || md5(array_to_string(node_ids, chr(31))),
       '迁移授权组 ' || ordinal,
       1,
       now(),
       now()
FROM grouped_sets;

WITH user_sets AS (
    SELECT user_id, array_agg(node_id ORDER BY node_id)::TEXT[] AS node_ids
    FROM node_assignments
    GROUP BY user_id
)
INSERT INTO authorization_group_users (group_id, user_id, created_at)
SELECT 'migration-' || md5(array_to_string(node_ids, chr(31))), user_id, now()
FROM user_sets;

WITH user_sets AS (
    SELECT user_id, array_agg(node_id ORDER BY node_id)::TEXT[] AS node_ids
    FROM node_assignments
    GROUP BY user_id
), node_sets AS (
    SELECT DISTINCT node_ids FROM user_sets
)
INSERT INTO authorization_group_nodes (group_id, node_id, created_at)
SELECT 'migration-' || md5(array_to_string(node_ids, chr(31))), node_id, now()
FROM node_sets
CROSS JOIN LATERAL unnest(node_ids) AS node_rows(node_id);

DO $$ DECLARE t text; BEGIN
    FOREACH t IN ARRAY ARRAY[
        'authorization_groups', 'authorization_group_users',
        'authorization_group_nodes', 'authorization_group_previews'
    ] LOOP
        EXECUTE format(
            'CREATE TRIGGER hysteriax_serialize_writes BEFORE INSERT OR UPDATE OR DELETE ON %I FOR EACH STATEMENT EXECUTE FUNCTION hysteriax_serialize_writes()',
            t
        );
    END LOOP;
END $$;
