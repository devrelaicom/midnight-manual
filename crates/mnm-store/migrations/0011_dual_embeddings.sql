-- 0011 — contextualized + dual code embeddings (spec 2026-06-10).
--
-- 1. Register voyage-context-3 as the general corpus model. It becomes the
--    newest embedding_model row, so get_active()'s fresh-DB fallback resolves
--    it once existing source_versions are deactivated below.
INSERT INTO embedding_model (name, revision, dim, provider)
VALUES ('voyage-context-3', 1, 1024, 'voyageai')
ON CONFLICT (name, revision) DO NOTHING;

-- 2. Second, code-specialised embedding per chunk (voyage-code-3). Nullable:
--    only DocumentKind::Code chunks of opted-in sources carry it.
ALTER TABLE chunk ADD COLUMN code_embedding vector(1024);

-- Partial HNSW keeps the code ANN graph restricted to code chunks.
CREATE INDEX idx_chunk_code_embedding ON chunk
    USING hnsw (code_embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64)
    WHERE code_embedding IS NOT NULL;

-- 3. Which code model a version's code_embeddings use. NULL ⇔ code
--    embeddings disabled (or no code files) for that version.
ALTER TABLE source_version
    ADD COLUMN code_embedding_model_id uuid REFERENCES embedding_model(id);

-- 4. Extend the cross-table model invariant: a chunk carrying a
--    code_embedding requires its source_version to declare a code model.
--    (CREATE OR REPLACE in a NEW migration — 0002 is applied and immutable.)
CREATE OR REPLACE FUNCTION check_chunk_embedding_model_match() RETURNS trigger AS $fn$
DECLARE
    sv_model uuid;
    sv_code_model uuid;
BEGIN
    SELECT embedding_model_id, code_embedding_model_id
        INTO sv_model, sv_code_model
        FROM source_version WHERE id = NEW.source_version_id;
    IF NEW.embedding_model_id <> sv_model THEN
        RAISE EXCEPTION 'chunk.embedding_model_id (%) does not match source_version.embedding_model_id (%) for source_version %',
            NEW.embedding_model_id, sv_model, NEW.source_version_id;
    END IF;
    IF NEW.code_embedding IS NOT NULL AND sv_code_model IS NULL THEN
        RAISE EXCEPTION 'chunk has code_embedding but source_version % has no code_embedding_model_id',
            NEW.source_version_id;
    END IF;
    RETURN NEW;
END;
$fn$ LANGUAGE plpgsql;

DROP TRIGGER trg_chunk_embedding_model_match ON chunk;
CREATE TRIGGER trg_chunk_embedding_model_match
    BEFORE INSERT OR UPDATE OF embedding_model_id, source_version_id, code_embedding ON chunk
    FOR EACH ROW EXECUTE FUNCTION check_chunk_embedding_model_match();

-- 5. Hard cutover (pre-1.0, D-summary): the corpus is re-ingested against
--    voyage-context-3. Deactivate every source_version (so boot resolution
--    falls back to the newest model = voyage-context-3) and clear vectors
--    that no longer match any active model.
UPDATE source_version SET is_active = false, status = 'inactive'
    WHERE is_active = true;
UPDATE chunk SET embedding = NULL WHERE embedding IS NOT NULL;
