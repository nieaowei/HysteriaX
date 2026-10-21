-- Event IDs are allocated before commit and cannot serve as a resume order.
-- Only the publisher locks this row; business writers append to the outbox.
CREATE TABLE event_publication_stream (
    id boolean PRIMARY KEY DEFAULT true CHECK (id),
    epoch text NOT NULL,
    head bigint NOT NULL DEFAULT 0 CHECK (head >= 0)
);
INSERT INTO event_publication_stream(id, epoch) VALUES (true, gen_random_uuid()::text);

CREATE TABLE event_publications (
    publication_seq bigint PRIMARY KEY,
    event_id bigint NOT NULL UNIQUE REFERENCES job_events(id) ON DELETE CASCADE
);
INSERT INTO event_publications(publication_seq, event_id)
SELECT row_number() OVER (ORDER BY id), id FROM job_events;
UPDATE event_publication_stream
SET head = (SELECT coalesce(max(publication_seq), 0) FROM event_publications);

-- A separate transactional pending queue avoids scanning all retained history
-- or updating source events (and their business invalidation triggers).
CREATE TABLE event_publication_pending (
    event_id bigint PRIMARY KEY REFERENCES job_events(id) ON DELETE CASCADE
);
CREATE FUNCTION hysteriax_event_pending() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    INSERT INTO event_publication_pending(event_id) VALUES(NEW.id);
    RETURN NEW;
END $$;
CREATE TRIGGER event_publication_pending AFTER INSERT ON job_events
FOR EACH ROW EXECUTE FUNCTION hysteriax_event_pending();
