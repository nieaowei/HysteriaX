CREATE TABLE subscription_templates (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    format TEXT NOT NULL CHECK (format IN ('mihomo', 'singbox')),
    revision BIGINT NOT NULL DEFAULT 1,
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    published_version BIGINT,
    draft_enc TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (NOT archived OR NOT is_default),
    CHECK (NOT is_default OR published_version IS NOT NULL)
);
CREATE UNIQUE INDEX subscription_templates_default ON subscription_templates(format) WHERE is_default;
CREATE TABLE subscription_template_versions (
    template_id TEXT NOT NULL REFERENCES subscription_templates(id),
    version BIGINT NOT NULL,
    document_enc TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY(template_id, version)
);
ALTER TABLE subscription_templates ADD CONSTRAINT subscription_template_published
    FOREIGN KEY (id, published_version) REFERENCES subscription_template_versions(template_id, version)
    DEFERRABLE INITIALLY DEFERRED;
CREATE TABLE subscription_template_initialization (id INTEGER PRIMARY KEY CHECK (id = 1));
