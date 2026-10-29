-- Existing users keep one selected connection password; all nodes use it after upgrade.
ALTER TABLE users ADD COLUMN connection_credential_hash text;
ALTER TABLE users ADD COLUMN connection_credential_enc text;
UPDATE users u SET connection_credential_hash=a.credential_hash,connection_credential_enc=a.credential_enc
FROM (SELECT DISTINCT ON(user_id) user_id,credential_hash,credential_enc FROM node_assignments ORDER BY user_id,node_id) a WHERE a.user_id=u.id;
CREATE UNIQUE INDEX users_connection_credential_hash ON users(connection_credential_hash) WHERE connection_credential_hash IS NOT NULL;
ALTER TABLE node_assignments DROP COLUMN credential_hash, DROP COLUMN credential_enc;
ALTER TABLE authorization_groups ADD COLUMN deleting boolean NOT NULL DEFAULT false;

CREATE TABLE authorization_changes (
 job_id text PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
 target_type text NOT NULL CHECK(target_type IN ('group','user')),
 target_id text NOT NULL,
 action text NOT NULL CHECK(action IN ('create','update','delete','memberships')),
 actor_id text NOT NULL,
 input jsonb NOT NULL,
 phase text NOT NULL DEFAULT 'sources',
 source_step integer NOT NULL DEFAULT 0,
 source_cursor text NOT NULL DEFAULT '',
 cursor_user text NOT NULL DEFAULT '',
 cursor_node text NOT NULL DEFAULT '',
 page_user text,
 additions bigint NOT NULL DEFAULT 0,
 removals bigint NOT NULL DEFAULT 0,
 complete boolean NOT NULL DEFAULT false
);
CREATE UNIQUE INDEX authorization_changes_active_target ON authorization_changes(target_type,target_id) WHERE NOT complete;
CREATE TABLE authorization_change_items (
 job_id text NOT NULL REFERENCES authorization_changes(job_id) ON DELETE CASCADE,
 kind text NOT NULL,
 item_id text NOT NULL,
 PRIMARY KEY(job_id,kind,item_id)
);
-- Exact source exclusions deny new connections immediately, including while a job is failed.
CREATE TABLE authorization_exclusions (
 job_id text NOT NULL REFERENCES authorization_changes(job_id) ON DELETE CASCADE,
 group_id text NOT NULL REFERENCES authorization_groups(id) ON DELETE CASCADE,
 user_id text NOT NULL DEFAULT '',
 node_id text NOT NULL DEFAULT '',
 PRIMARY KEY(job_id,group_id,user_id,node_id)
);
CREATE INDEX authorization_exclusions_source ON authorization_exclusions(group_id,user_id,node_id);
CREATE TRIGGER cache_row_changed AFTER INSERT OR DELETE OR UPDATE ON authorization_exclusions FOR EACH ROW EXECUTE FUNCTION hysteriax_cache_row_changed('groups,user_config','true');
CREATE FUNCTION hysteriax_authorized(who text,where_node text) RETURNS boolean LANGUAGE sql STABLE AS $$
 SELECT EXISTS(SELECT 1 FROM authorization_group_users gu JOIN authorization_group_nodes gn USING(group_id)
 JOIN authorization_groups g ON g.id=gu.group_id
 WHERE gu.user_id=who AND gn.node_id=where_node AND NOT g.deleting
 AND NOT EXISTS(SELECT 1 FROM authorization_exclusions x WHERE x.group_id=gu.group_id
 AND x.user_id IN ('',who) AND x.node_id IN ('',where_node)))
$$;
