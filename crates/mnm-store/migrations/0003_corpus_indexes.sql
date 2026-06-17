-- 0003 — corpus indexes.
--
-- Split into a separate migration so future tunings (HNSW parameters, additional
-- composite indexes) ship without re-running the table-creation migration.
-- HNSW parameters m=16, ef_construction=64 are the conservative defaults; revisit
-- under the load benchmark (SC-013).

-- node tree access patterns
CREATE INDEX idx_node_parent         ON node (parent_node_id, order_index);
CREATE INDEX idx_node_source_version ON node (source_version_id);

-- package access patterns
CREATE INDEX idx_package_source_version ON package (source_version_id);

-- document indexes
CREATE INDEX idx_document_content_hash   ON document (content_hash);
CREATE INDEX idx_document_source_version ON document (source_version_id);
CREATE INDEX idx_document_node           ON document (node_id);

-- FTS index (GIN on tsvector) - FR-011
CREATE INDEX idx_chunk_tsvector ON chunk USING GIN (tsvector);

-- Vector ANN index (HNSW on embedding) - FR-011
CREATE INDEX idx_chunk_embedding ON chunk USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64);

-- Active-version filter (FR-027) + status gating
CREATE INDEX idx_chunk_source_version_status ON chunk (source_version_id, status);

-- Document-internal navigation patterns
CREATE INDEX idx_chunk_document_index ON chunk (document_id, chunk_index);
CREATE INDEX idx_chunk_node          ON chunk (node_id);
