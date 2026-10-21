-- Logical epochs cannot be reused by a backwards wall clock.
CREATE TABLE user_access_restriction_epochs (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    generation BIGINT NOT NULL CHECK(generation>0)
);
CREATE FUNCTION hysteriax_track_access_restriction_epoch() RETURNS trigger
LANGUAGE plpgsql AS $$ BEGIN
    INSERT INTO user_access_restriction_epochs(user_id,generation) VALUES(NEW.id,1)
    ON CONFLICT(user_id) DO UPDATE SET generation=user_access_restriction_epochs.generation+1;
    RETURN NEW;
END $$;
CREATE TRIGGER track_access_restriction_epoch AFTER UPDATE OF access_kick_enqueued_at ON users
FOR EACH ROW WHEN (OLD.access_kick_enqueued_at IS DISTINCT FROM NEW.access_kick_enqueued_at)
EXECUTE FUNCTION hysteriax_track_access_restriction_epoch();

-- One indexed due range avoids sorting the entire disabled/expired/quota union
-- on every finite scan, especially after a long service interruption.
CREATE TABLE user_access_checks (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    next_action_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX user_access_checks_due_idx ON user_access_checks(next_action_at,user_id);
CREATE FUNCTION hysteriax_refresh_user_access_check(
    subject TEXT, active BOOLEAN, expiry TIMESTAMPTZ, quota BIGINT, usage BIGINT, marker TIMESTAMPTZ
) RETURNS void LANGUAGE plpgsql AS $$
DECLARE due TIMESTAMPTZ;
BEGIN
    IF marker IS NOT NULL THEN
        DELETE FROM user_access_checks WHERE user_id=subject;
        RETURN;
    END IF;
    due := CASE WHEN NOT active OR usage>=quota THEN clock_timestamp() ELSE expiry END;
    IF due IS NULL THEN
        DELETE FROM user_access_checks WHERE user_id=subject;
    ELSE
        INSERT INTO user_access_checks(user_id,next_action_at) VALUES(subject,due)
        ON CONFLICT(user_id) DO UPDATE SET next_action_at=excluded.next_action_at
        WHERE user_access_checks.next_action_at IS DISTINCT FROM excluded.next_action_at;
    END IF;
END $$;
CREATE FUNCTION hysteriax_update_user_access_check() RETURNS trigger
LANGUAGE plpgsql AS $$ BEGIN
    PERFORM hysteriax_refresh_user_access_check(NEW.id,NEW.enabled,NEW.expires_at,NEW.quota_bytes,NEW.usage_bytes,NEW.access_kick_enqueued_at);
    RETURN NEW;
END $$;
CREATE TRIGGER schedule_new_user_access_check AFTER INSERT ON users
FOR EACH ROW EXECUTE FUNCTION hysteriax_update_user_access_check();
CREATE TRIGGER schedule_changed_user_access_check AFTER UPDATE ON users
FOR EACH ROW WHEN (
    ROW(OLD.enabled,OLD.expires_at,OLD.quota_bytes,OLD.access_kick_enqueued_at)
        IS DISTINCT FROM ROW(NEW.enabled,NEW.expires_at,NEW.quota_bytes,NEW.access_kick_enqueued_at)
    OR (OLD.quota_bytes IS NOT NULL AND OLD.usage_bytes>=OLD.quota_bytes)
        IS DISTINCT FROM (NEW.quota_bytes IS NOT NULL AND NEW.usage_bytes>=NEW.quota_bytes)
) EXECUTE FUNCTION hysteriax_update_user_access_check();
