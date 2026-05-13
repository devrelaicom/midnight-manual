-- 0002 — corpus schema (Story 1 entities).
--
-- Tables defined in spec/data-model.md §0002. All foreign keys cascade on
-- delete to keep the sweep job's single-transaction retention pass cheap.

-- ============================================================================
-- source: stable handle for a logical content source
-- ============================================================================
CREATE TABLE source (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    slug            text NOT NULL UNIQUE,
    display_name    text NOT NULL,
    kind            text NOT NULL CHECK (kind IN ('docs_site','code_repo','standalone','mixed')),
    origin_url      text,
    retention_count integer NOT NULL DEFAULT 5 CHECK (retention_count BETWEEN 1 AND 50),
    created_at      timestamptz NOT NULL DEFAULT now(),
    retired_at      timestamptz
);

-- ============================================================================
-- embedding_model: registry of models the corpus has been encoded with
-- ============================================================================
CREATE TABLE embedding_model (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name       text NOT NULL,
    revision   integer NOT NULL CHECK (revision >= 1),
    dim        integer NOT NULL CHECK (dim BETWEEN 64 AND 4096),
    provider   text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (name, revision)
);

-- ============================================================================
-- source_version: immutable snapshot of a source
-- Lifecycle (spec Story 1): building -> active -> inactive -> retired -> deleted.
-- ============================================================================
CREATE TABLE source_version (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_id          uuid NOT NULL REFERENCES source(id) ON DELETE CASCADE,
    revision           integer NOT NULL,
    status             text NOT NULL CHECK (status IN ('building','active','inactive','aborted','retired')),
    is_active          boolean NOT NULL DEFAULT false,
    ingested_at        timestamptz NOT NULL DEFAULT now(),
    ingest_cli_version text NOT NULL,
    embedding_model_id uuid NOT NULL REFERENCES embedding_model(id),
    content_hash       text NOT NULL,
    notes              text,
    retired_at         timestamptz,
    UNIQUE (source_id, revision)
);

-- At most one active version per source (FR-003, FR-061, EC-04)
CREATE UNIQUE INDEX uniq_source_version_active
    ON source_version (source_id)
    WHERE is_active;

-- Cross-check: is_active=true implies status='active'
ALTER TABLE source_version ADD CONSTRAINT check_active_status
    CHECK ((is_active = false) OR (status = 'active'));

-- ============================================================================
-- node: hierarchical tree (root -> groups -> documents -> chunks)
-- ============================================================================
CREATE TABLE node (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_version_id uuid NOT NULL REFERENCES source_version(id) ON DELETE CASCADE,
    parent_node_id    uuid REFERENCES node(id) ON DELETE CASCADE,
    kind              text NOT NULL CHECK (kind IN ('root','group','document','chunk')),
    name              text NOT NULL,
    order_index       integer NOT NULL DEFAULT 0,
    created_at        timestamptz NOT NULL DEFAULT now()
);

-- ============================================================================
-- package: language-aware grouping of code documents
-- ============================================================================
CREATE TABLE package (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_version_id uuid NOT NULL REFERENCES source_version(id) ON DELETE CASCADE,
    kind              text NOT NULL CHECK (kind IN ('rust','npm','compact','other')),
    name              text NOT NULL,
    version           text,
    manifest_path     text,
    metadata          jsonb NOT NULL DEFAULT '{}'::jsonb,
    UNIQUE (source_version_id, kind, name)
);

-- ============================================================================
-- document: an ingested page (Markdown) or file (code, plaintext)
-- ============================================================================
CREATE TABLE document (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_version_id  uuid NOT NULL REFERENCES source_version(id) ON DELETE CASCADE,
    node_id            uuid NOT NULL REFERENCES node(id) ON DELETE CASCADE,
    kind               text NOT NULL CHECK (kind IN ('markdown','code','plaintext')),
    source_url         text,
    published_url      text,
    source_path        text NOT NULL,
    language           text,
    content_hash       text NOT NULL,
    source_modified_at timestamptz,
    frontmatter        jsonb,
    provenance         jsonb NOT NULL DEFAULT '{}'::jsonb,
    package_id         uuid REFERENCES package(id) ON DELETE SET NULL,
    char_count         integer NOT NULL DEFAULT 0,
    token_count        integer NOT NULL DEFAULT 0,
    created_at         timestamptz NOT NULL DEFAULT now()
);

-- ============================================================================
-- chunk: the smallest indexed unit. Carries FTS vector and embedding.
-- Vector dim 768 = bge-base-en-v1.5 default (D14, R-1).
-- ============================================================================
CREATE TABLE chunk (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_version_id  uuid NOT NULL REFERENCES source_version(id) ON DELETE CASCADE,
    document_id        uuid NOT NULL REFERENCES document(id) ON DELETE CASCADE,
    node_id            uuid NOT NULL REFERENCES node(id) ON DELETE CASCADE,
    chunk_index        integer NOT NULL CHECK (chunk_index >= 0),
    total_chunks       integer NOT NULL CHECK (total_chunks >= 1),
    content            text NOT NULL,
    content_hash       text NOT NULL,
    tsvector           tsvector GENERATED ALWAYS AS (to_tsvector('english', content)) STORED,
    embedding          vector(768),
    embedding_model_id uuid NOT NULL REFERENCES embedding_model(id),
    heading_path       text[] NOT NULL DEFAULT ARRAY[]::text[],
    symbol_path        text[] NOT NULL DEFAULT ARRAY[]::text[],
    start_byte         integer NOT NULL DEFAULT 0,
    end_byte           integer NOT NULL DEFAULT 0,
    token_count        integer NOT NULL DEFAULT 0,
    status             text NOT NULL CHECK (status IN ('ready','embed_failed','deprecated')),
    created_at         timestamptz NOT NULL DEFAULT now()
);

-- Cross-check: chunk.embedding_model_id matches its source_version's
-- embedding_model_id (FR-002, EC-10). Implemented as a trigger because the
-- check crosses tables.
CREATE OR REPLACE FUNCTION check_chunk_embedding_model_match() RETURNS trigger AS $fn$
DECLARE
    sv_model uuid;
BEGIN
    SELECT embedding_model_id INTO sv_model FROM source_version WHERE id = NEW.source_version_id;
    IF NEW.embedding_model_id <> sv_model THEN
        RAISE EXCEPTION 'chunk.embedding_model_id (%) does not match source_version.embedding_model_id (%) for source_version %',
            NEW.embedding_model_id, sv_model, NEW.source_version_id;
    END IF;
    RETURN NEW;
END;
$fn$ LANGUAGE plpgsql;

CREATE TRIGGER trg_chunk_embedding_model_match
    BEFORE INSERT OR UPDATE OF embedding_model_id, source_version_id ON chunk
    FOR EACH ROW EXECUTE FUNCTION check_chunk_embedding_model_match();
