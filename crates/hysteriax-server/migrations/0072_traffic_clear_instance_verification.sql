-- NULL preserves the semantics of captures completed by earlier collectors.
-- New destructive responses are saved with false before any follow-up SSH
-- observation. Recovery can bill interval bytes even if verification is lost.
ALTER TABLE traffic_sample_batches ADD COLUMN clear_instance_verified boolean;
