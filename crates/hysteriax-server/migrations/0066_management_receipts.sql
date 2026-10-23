-- One reservation per administrator, operation and caller key. It lives only
-- inside the business transaction; a successful response must exist at commit.
-- No FK to admin_tokens: historical receipts survive administrator removal.
CREATE TABLE management_receipts (
    actor_id TEXT NOT NULL,
    operation TEXT NOT NULL,
    idempotency_key TEXT NOT NULL CHECK (octet_length(idempotency_key) BETWEEN 1 AND 200),
    request_digest TEXT NOT NULL CHECK (length(request_digest)=64),
    response_enc TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (actor_id,operation,idempotency_key)
);

CREATE FUNCTION hysteriax_management_receipt_complete() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    -- Read the final row, not the INSERT transition tuple with its placeholder.
    IF EXISTS (SELECT 1 FROM management_receipts
        WHERE actor_id=NEW.actor_id AND operation=NEW.operation
          AND idempotency_key=NEW.idempotency_key AND response_enc='') THEN
        RAISE EXCEPTION 'management receipt is incomplete' USING ERRCODE='23514';
    END IF;
    RETURN NULL;
END $$;
CREATE CONSTRAINT TRIGGER management_receipt_complete
AFTER INSERT OR UPDATE ON management_receipts
DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
EXECUTE FUNCTION hysteriax_management_receipt_complete();
