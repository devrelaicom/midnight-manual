-- 0008 — switch the corpus embedding model to VoyageAI voyage-code-3 (1024-dim).
--
-- A vector(1024) column cannot hold the prior 768-dim vectors, so this clears
-- existing embeddings (greenfield/test data only). Chunk rows are preserved.
-- The HNSW index is bound to the column dimension and must be recreated.

-- 1. Register + (implicitly) make voyage-code-3 the newest model. get_active()
--    returns the most-recently-created model when no source_version is active.
INSERT INTO embedding_model (name, revision, dim, provider)
VALUES ('voyage-code-3', 1, 1024, 'voyageai')
ON CONFLICT (name, revision) DO NOTHING;

-- 2. Drop the HNSW index (bound to vector(768)).
DROP INDEX IF EXISTS idx_chunk_embedding;

-- 3. Clear old-dim vectors and re-type the column to vector(1024).
--    NOTE: ALTER COLUMN TYPE takes ACCESS EXCLUSIVE on chunk and rewrites every
--    row. On the current greenfield/test corpus this is instantaneous; against a
--    future populated corpus a zero-downtime path (shadow column + backfill)
--    would be needed instead.
UPDATE chunk SET embedding = NULL WHERE embedding IS NOT NULL;
ALTER TABLE chunk ALTER COLUMN embedding TYPE vector(1024);

-- 4. Recreate the HNSW index on the new column (skips NULLs automatically).
CREATE INDEX idx_chunk_embedding ON chunk USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64);
