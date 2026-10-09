CREATE INDEX nodes_name_id_idx ON nodes(lower(name) COLLATE "C", name COLLATE "C", id);
