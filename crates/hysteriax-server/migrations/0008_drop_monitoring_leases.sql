-- Run through --migrate-only with every API replica stopped.
-- The 2026-10-22 proxy sampling refactor replaced the per-node monitoring lease
-- with background_work execution leases (resource_leases plus the work id) and
-- issued probe credentials through monitoring_probe_tokens. No Rust or embedded
-- SQL statement, database function, view, trigger or foreign key references this
-- table since then; the remaining rows are expired leases written before the
-- production database was moved, and they carry no state that must be kept.
-- Dropping the table removes its rows, primary key, foreign key and indexes.
DROP TABLE monitoring_leases;
