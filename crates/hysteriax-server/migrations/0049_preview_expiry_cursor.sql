-- Support bounded, ordered expiry collection without sorting all equal-time rows.
CREATE INDEX authorization_group_previews_expiry_cursor_idx
    ON authorization_group_previews(expires_at, token);
DROP INDEX authorization_group_previews_expiry_idx;
