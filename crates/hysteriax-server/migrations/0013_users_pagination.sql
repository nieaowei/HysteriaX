CREATE INDEX users_name_id_idx ON users(lower(name) COLLATE "C", name COLLATE "C", id);
