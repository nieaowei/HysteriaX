-- Run through --migrate-only with API replicas stopped.
-- All authorization changes now commit directly; preview tokens are obsolete.
-- Dropping this table removes its data, indexes and owned constraints atomically.
DROP TABLE authorization_group_previews;
