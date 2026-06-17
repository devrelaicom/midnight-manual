-- 0001 — required Postgres extensions for the midnight-manual corpus schema.
--
-- The pgvector extension is REQUIRED — the server's /readyz endpoint reports
-- 503 if it is missing (EC-59). pgcrypto provides gen_random_uuid() on
-- Postgres < 16; we require Postgres 16+ where gen_random_uuid is built into
-- the core, so pgcrypto is only loaded as a no-op safety net for hosts that
-- have surfaced gen_random_uuid via the extension instead.
--
-- Managed Postgres providers (Fly MPG, Neon, RDS, …) generally don't grant
-- the per-app role CREATE-EXTENSION privilege; instead the platform either
-- pre-installs the extension (`fly mpg create --pgvector`) or the operator
-- installs it once as the cluster admin role. Either way the per-app role
-- can't reinstall it.
--
-- `CREATE EXTENSION IF NOT EXISTS` does NOT bypass the permission check —
-- Postgres still validates privilege at plan time before the IF-NOT-EXISTS
-- branch runs. So we guard with a DO block that consults `pg_extension`
-- first and skips the `CREATE` statement entirely when the extension is
-- already loaded. On self-hosted Postgres where the app role IS a
-- superuser, the `CREATE` branch fires once and behaves exactly like the
-- original statement.

DO $do$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'vector') THEN
        CREATE EXTENSION vector;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pgcrypto') THEN
        CREATE EXTENSION pgcrypto;
    END IF;
END
$do$;
