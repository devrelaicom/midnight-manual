-- 0004 — admin schema (rate-limit overrides + reserved user/api_key tables).
--
-- The Ed25519 user store is a TOML file (D20), NOT a DB table. The `user` and
-- `api_key` tables below are reserved for potential v2 role-based features
-- and never written to in v1. Keeping them here means later versions can adopt
-- them without a breaking migration.

-- ============================================================================
-- rate_limit_override: per-CIDR temporary rate-limit ceiling (D11, FR-031)
-- ============================================================================
CREATE TABLE rate_limit_override (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    cidr       cidr NOT NULL,
    limit_rps  integer NOT NULL CHECK (limit_rps > 0),
    expires_at timestamptz NOT NULL,
    note       text,
    created_by text NOT NULL,                          -- user_id from JWT sub claim
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Plain btree on expires_at — queries filter `WHERE expires_at > now()` at
-- query time. A partial index with `WHERE expires_at > now()` would be
-- rejected (now() is not IMMUTABLE) and is the wrong semantics anyway: the
-- predicate would be frozen at index-creation time.
CREATE INDEX idx_rate_limit_override_active
    ON rate_limit_override (expires_at);

-- ============================================================================
-- Reserved tables (not used in v1; documented for forward compatibility)
-- ============================================================================
CREATE TABLE "user" (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    text NOT NULL UNIQUE,
    public_key bytea NOT NULL,
    role       text NOT NULL CHECK (role IN ('admin','writer','reader')),
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz
);

CREATE TABLE api_key (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    text REFERENCES "user"(user_id) ON DELETE CASCADE,
    token_hash bytea NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    revoked_at timestamptz,
    note       text
);
