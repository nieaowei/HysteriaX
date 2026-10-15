CREATE INDEX nodes_monitoring_search_trgm ON nodes USING gin(hysteriax_search_text(id,name,public_host,ssh_host) public.gin_trgm_ops);
