-- Retain failed history while relating explicit retry attempts into a linear chain.
ALTER TABLE jobs ADD COLUMN retry_of_job_id TEXT REFERENCES jobs(id);
CREATE UNIQUE INDEX jobs_retry_parent_idx ON jobs(retry_of_job_id)
    WHERE retry_of_job_id IS NOT NULL;
