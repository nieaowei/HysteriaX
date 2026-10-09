CREATE INDEX dns_records_name_type_id_idx ON dns_records(name COLLATE "C", record_type COLLATE "C", id);
CREATE INDEX dns_records_zone_name_type_id_idx ON dns_records(zone_id, name COLLATE "C", record_type COLLATE "C", id);
