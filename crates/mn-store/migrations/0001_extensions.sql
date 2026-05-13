-- 0001 — required Postgres extensions for the midnight-manual corpus schema.
--
-- The pgvector extension is REQUIRED — the server's /readyz endpoint reports
-- 503 if it is missing (EC-59). pgcrypto provides gen_random_uuid() on
-- Postgres < 16; we require Postgres 16+ where gen_random_uuid is built into
-- the core, so pgcrypto is only loaded as a no-op safety net for hosts that
-- have surfaced gen_random_uuid via the extension instead.

CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pgcrypto;
