-- 0006 — seed the default embedding model.
--
-- The server's startup sequence (FR-009.e) also seeds this row if missing, so
-- this migration is purely a convenience for a freshly-migrated DB. ON CONFLICT
-- makes it idempotent regardless.

INSERT INTO embedding_model (name, revision, dim, provider)
VALUES ('bge-base-en-v1.5', 1, 768, 'baai')
ON CONFLICT (name, revision) DO NOTHING;
