-- 0009 — embedding token-limit overrides + restart-durability snapshot.

CREATE TABLE token_limit_override (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    subject_kind text NOT NULL CHECK (subject_kind IN ('cidr','user')),
    subject      text NOT NULL,   -- CIDR text (network-normalised) or user id
    hourly       bigint NOT NULL CHECK (hourly >= 0),
    daily        bigint NOT NULL CHECK (daily  >= 0),
    expires_at   timestamptz NOT NULL,
    note         text,
    created_by   text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX idx_token_limit_override_active ON token_limit_override (expires_at);

-- In-memory usage is the source of truth on the hot path; this is only a
-- periodic snapshot so a restart can reload ~last 24h of hourly buckets.
CREATE TABLE token_usage_snapshot (
    subject_kind text   NOT NULL CHECK (subject_kind IN ('ip','user')),
    subject      text   NOT NULL,
    hour_epoch   bigint NOT NULL,            -- floor(unix_secs / 3600)
    tokens       bigint NOT NULL CHECK (tokens >= 0),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (subject_kind, subject, hour_epoch)
);
CREATE INDEX idx_token_usage_snapshot_hour ON token_usage_snapshot (hour_epoch);
