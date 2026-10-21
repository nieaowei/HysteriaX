-- Latest/non-discarded probe selection is local to one node and CA generation.
CREATE INDEX mtls_probe_current_scope_idx
ON mtls_probe_versions(node_id,ca_id,ca_version,credential_version DESC)
WHERE NOT discarded;
