# Phase 1: Data Model — midnight-manual v1

**Feature**: 001-rag-platform | **Date**: 2026-05-13

Concrete DDL for every table in the v1 schema. All entity shapes are derived from `spec.md` (Story 1 Key Entities + Story 9 admin/auth tables + Story 11 telemetry tables). Migrations live under `crates/mn-store/migrations/` as numbered `.sql` files; `sqlx migrate` orders them by filename.

PostgreSQL 16+ required. The `pgvector` extension MUST be installed in the target database (server fails readiness if absent — EC-59).

## Migration order

| # | File | Contents |
|---|---|---|
| 0001 | `0001_extensions.sql` | `CREATE EXTENSION IF NOT EXISTS vector;` |
| 0002 | `0002_corpus_schema.sql` | Story-1 entities: source, source_version, embedding_model, node, document, package, chunk |
| 0003 | `0003_corpus_indexes.sql` | HNSW on chunk.embedding; GIN on chunk.tsvector; btree FKs |
| 0004 | `0004_admin_schema.sql` | rate_limit_override (+ `user`, `api_key` reserved for future) |
| 0005 | `0005_telemetry_schema.sql` | telemetry_event_raw, telemetry_aggregate_daily |
| 0006 | `0006_seed_embedding_model.sql` | Seed `bge-base-en-v1.5@1` (run by server FR-009.e if absent) |

## 0002 — Corpus schema

```sql
-- ============================================================================
-- source: stable handle for a logical content source
-- ============================================================================
CREATE TABLE source (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    slug              text NOT NULL UNIQUE,
    display_name      text NOT NULL,
    kind              text NOT NULL CHECK (kind IN ('docs_site','code_repo','standalone','mixed')),
    origin_url        text,
    retention_count   integer NOT NULL DEFAULT 5 CHECK (retention_count BETWEEN 1 AND 50),
    created_at        timestamptz NOT NULL DEFAULT now(),
    retired_at        timestamptz
);

-- ============================================================================
-- embedding_model: registry of models the corpus has been encoded with
-- ============================================================================
CREATE TABLE embedding_model (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text NOT NULL,
    revision    integer NOT NULL CHECK (revision >= 1),
    dim         integer NOT NULL CHECK (dim BETWEEN 64 AND 4096),
    provider    text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (name, revision)
);

-- ============================================================================
-- source_version: immutable snapshot of a source
-- Lifecycle (spec.md Story 1): building → active → inactive → retired → deleted.
-- ============================================================================
CREATE TABLE source_version (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_id            uuid NOT NULL REFERENCES source(id) ON DELETE CASCADE,
    revision             integer NOT NULL,
    status               text NOT NULL CHECK (status IN ('building','active','inactive','aborted','retired')),
    is_active            boolean NOT NULL DEFAULT false,
    ingested_at          timestamptz NOT NULL DEFAULT now(),
    ingest_cli_version   text NOT NULL,
    embedding_model_id   uuid NOT NULL REFERENCES embedding_model(id),
    content_hash         text NOT NULL,
    notes                text,
    retired_at           timestamptz,
    UNIQUE (source_id, revision)
);

-- Constitution: at most one active version per source (FR-003, FR-061)
CREATE UNIQUE INDEX uniq_source_version_active
    ON source_version (source_id)
    WHERE is_active;

-- Cross-check: is_active=true implies status='active'
ALTER TABLE source_version ADD CONSTRAINT check_active_status
    CHECK ((is_active = false) OR (status = 'active'));

-- ============================================================================
-- node: hierarchical tree (root → groups → documents → chunks)
-- ============================================================================
CREATE TABLE node (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_version_id   uuid NOT NULL REFERENCES source_version(id) ON DELETE CASCADE,
    parent_node_id      uuid REFERENCES node(id) ON DELETE CASCADE,
    kind                text NOT NULL CHECK (kind IN ('root','group','document','chunk')),
    name                text NOT NULL,
    order_index         integer NOT NULL DEFAULT 0,
    created_at          timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_node_parent ON node (parent_node_id, order_index);
CREATE INDEX idx_node_source_version ON node (source_version_id);

-- ============================================================================
-- package: language-aware grouping of code documents
-- ============================================================================
CREATE TABLE package (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_version_id   uuid NOT NULL REFERENCES source_version(id) ON DELETE CASCADE,
    kind                text NOT NULL CHECK (kind IN ('rust','npm','compact','other')),
    name                text NOT NULL,
    version             text,
    manifest_path       text,
    metadata            jsonb NOT NULL DEFAULT '{}'::jsonb,
    UNIQUE (source_version_id, kind, name)
);

CREATE INDEX idx_package_source_version ON package (source_version_id);

-- ============================================================================
-- document: an ingested page (Markdown) or file (code, plaintext)
-- ============================================================================
CREATE TABLE document (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_version_id    uuid NOT NULL REFERENCES source_version(id) ON DELETE CASCADE,
    node_id              uuid NOT NULL REFERENCES node(id) ON DELETE CASCADE,
    kind                 text NOT NULL CHECK (kind IN ('markdown','code','plaintext')),
    source_url           text,
    published_url        text,
    source_path          text NOT NULL,
    language             text,
    content_hash         text NOT NULL,
    source_modified_at   timestamptz,
    frontmatter          jsonb,
    provenance           jsonb NOT NULL DEFAULT '{}'::jsonb,
    package_id           uuid REFERENCES package(id) ON DELETE SET NULL,
    char_count           integer NOT NULL DEFAULT 0,
    token_count          integer NOT NULL DEFAULT 0,
    created_at           timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_document_content_hash    ON document (content_hash);
CREATE INDEX idx_document_source_version  ON document (source_version_id);
CREATE INDEX idx_document_node            ON document (node_id);

-- ============================================================================
-- chunk: the smallest indexed unit. Carries FTS vector and embedding.
-- Vector dim 768 = bge-base-en-v1.5 default (D14, R-1 in research.md).
-- ============================================================================
CREATE TABLE chunk (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source_version_id    uuid NOT NULL REFERENCES source_version(id) ON DELETE CASCADE,
    document_id          uuid NOT NULL REFERENCES document(id) ON DELETE CASCADE,
    node_id              uuid NOT NULL REFERENCES node(id) ON DELETE CASCADE,
    chunk_index          integer NOT NULL CHECK (chunk_index >= 0),
    total_chunks         integer NOT NULL CHECK (total_chunks >= 1),
    content              text NOT NULL,
    content_hash         text NOT NULL,
    tsvector             tsvector GENERATED ALWAYS AS (to_tsvector('english', content)) STORED,
    embedding            vector(768),
    embedding_model_id   uuid NOT NULL REFERENCES embedding_model(id),
    heading_path         text[] NOT NULL DEFAULT ARRAY[]::text[],
    symbol_path          text[] NOT NULL DEFAULT ARRAY[]::text[],
    start_byte           integer NOT NULL DEFAULT 0,
    end_byte             integer NOT NULL DEFAULT 0,
    token_count          integer NOT NULL DEFAULT 0,
    status               text NOT NULL CHECK (status IN ('ready','embed_failed','deprecated')),
    created_at           timestamptz NOT NULL DEFAULT now()
);

-- Cross-check: chunk.embedding_model_id matches source_version.embedding_model_id (FR-002, EC-10)
-- Implemented via a trigger rather than a CHECK because it crosses tables.
CREATE OR REPLACE FUNCTION check_chunk_embedding_model_match() RETURNS trigger AS $$
BEGIN
    IF NEW.embedding_model_id <> (SELECT embedding_model_id FROM source_version WHERE id = NEW.source_version_id) THEN
        RAISE EXCEPTION 'chunk.embedding_model_id does not match source_version.embedding_model_id';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_chunk_embedding_model_match
    BEFORE INSERT OR UPDATE OF embedding_model_id, source_version_id ON chunk
    FOR EACH ROW EXECUTE FUNCTION check_chunk_embedding_model_match();
```

## 0003 — Corpus indexes

```sql
-- FTS index (GIN on tsvector) — FR-011
CREATE INDEX idx_chunk_tsvector ON chunk USING GIN (tsvector);

-- Vector ANN index (HNSW on embedding) — FR-011
-- m=16, ef_construction=64 are the defaults; tune per benchmark.
CREATE INDEX idx_chunk_embedding ON chunk USING hnsw (embedding vector_cosine_ops)
    WITH (m = 16, ef_construction = 64);

-- Filter index for the active-version filter (FR-027)
CREATE INDEX idx_chunk_source_version_status ON chunk (source_version_id, status);

-- Document siblings and parents access patterns
CREATE INDEX idx_chunk_document_index ON chunk (document_id, chunk_index);
CREATE INDEX idx_chunk_node ON chunk (node_id);
```

## 0004 — Admin schema

```sql
-- ============================================================================
-- rate_limit_override: CIDR override entries (D11)
-- ============================================================================
CREATE TABLE rate_limit_override (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    cidr         cidr NOT NULL,
    limit_rps    integer NOT NULL CHECK (limit_rps > 0),
    expires_at   timestamptz NOT NULL,
    note         text,
    created_by   text NOT NULL,                          -- user_id from JWT sub claim
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_rate_limit_override_active
    ON rate_limit_override (expires_at)
    WHERE expires_at > now();

-- ============================================================================
-- Reserved tables for future role-based features (not used in v1 directly).
-- The Ed25519 user store is a TOML file (D20), NOT this table.
-- These remain for potential v2 use without breaking schema.
-- ============================================================================
CREATE TABLE "user" (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     text NOT NULL UNIQUE,
    public_key  bytea NOT NULL,
    role        text NOT NULL CHECK (role IN ('admin','writer','reader')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    revoked_at  timestamptz
);

CREATE TABLE api_key (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     text REFERENCES "user"(user_id) ON DELETE CASCADE,
    token_hash  bytea NOT NULL UNIQUE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    revoked_at  timestamptz,
    note        text
);
```

## 0005 — Telemetry schema

```sql
-- ============================================================================
-- telemetry_event_raw: per-event rows, auto-deleted after 7 days (FR-110)
-- ============================================================================
CREATE TABLE telemetry_event_raw (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    received_at  timestamptz NOT NULL DEFAULT now(),
    event_type   text NOT NULL CHECK (event_type IN (
        'mcp_tool_call','cli_command','ingest_complete','pull_models','mcp_startup','mcp_shutdown'
    )),
    component    text NOT NULL CHECK (component IN ('cli','mcp','server')),
    version      text NOT NULL,
    fields       jsonb NOT NULL,
    request_id   text
);

CREATE INDEX idx_telemetry_event_raw_received_at ON telemetry_event_raw (received_at);
CREATE INDEX idx_telemetry_event_raw_type        ON telemetry_event_raw (event_type, received_at);

-- ============================================================================
-- telemetry_aggregate_daily: per-day counters retained indefinitely
-- ============================================================================
CREATE TABLE telemetry_aggregate_daily (
    day          date NOT NULL,
    event_type   text NOT NULL,
    component    text NOT NULL,
    count        bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (day, event_type, component)
);
```

## 0006 — Seed embedding_model

```sql
INSERT INTO embedding_model (name, revision, dim, provider)
VALUES ('bge-base-en-v1.5', 1, 768, 'baai')
ON CONFLICT (name, revision) DO NOTHING;
```

## JSONB schemas

### `document.provenance`

Validated by `mn-content` at ingest time and by `mn-server` on receipt; not enforced at the DB level (jsonb is permissive by design — schema drift handled at the application boundary per Constitution VIII).

```jsonc
{
    "attribution": "foundation",                   // enum: foundation | partner | third_party | community | unknown
    "verified": true,
    "verified_by": "midnight-foundation",          // nullable
    "verified_at": "2026-04-01",                   // ISO date, nullable
    "verification_notes": null,
    "language_targets": [                          // array
        { "name": "compact", "version_constraint": ">=0.23" }
    ],
    "sdk_dependencies": [                          // array
        { "kind": "npm", "name": "@midnight-ntwrk/midnight-js", "version_constraint": "^1.4.0" }
    ],
    "deprecation": { "is_deprecated": false, "since": null, "reason": null },
    "tags": ["quickstart","tutorial"],
    "content_type": "tutorial"                     // enum: doc | tutorial | reference | example | contract_source | sdk_source | test | readme
}
```

### `telemetry_event_raw.fields` per event_type

See `crates/mn-telemetry/src/schemas/*.rs` for the Rust types; the JSON Schema documents live in `contracts/telemetry-events.json` (produced from the Rust types via `schemars`).

## Cardinality and storage estimates (v1)

| Table | Rows (v1 target) | Average row size | Storage |
|---|---|---|---|
| source | ~10 | small | <1 KB |
| source_version | ~50 (10 sources × 5 retained) | small | <10 KB |
| embedding_model | 1 | small | <1 KB |
| node | ~120k (root + groups + ~100k chunk-kind) | small | ~10 MB |
| document | ~3,000 (300 MD pages × 5 versions + code files) | small + frontmatter | ~5 MB |
| package | ~50 | small | <10 KB |
| chunk | ~100,000 | 1.5 KB text + 3 KB vector + tsvector | ~600 MB |
| rate_limit_override | <20 | small | <5 KB |
| telemetry_event_raw | 7 days × ~1k events/day = ~7k | small | ~1 MB (rolling) |
| telemetry_aggregate_daily | unbounded; ~10/day × 365/yr = ~3.6k/yr | small | <1 MB |

**Total at v1**: ~700 MB Postgres. Fits comfortably on Fly.io managed Postgres's smallest tier.

## Migration discipline

- All migrations forward-only (D22).
- Renames implemented as add-column + dual-write window + drop-column across three releases.
- Adding a column to `document.provenance` is **not** a migration (jsonb).
- Adding a new `embedding_model` row is a runtime insert via the server's startup seed.
- Index changes (e.g. HNSW parameter tuning) are forward migrations.
- Migration failure fails the server startup with the failing migration name (FR-064).

## Output

- `crates/mn-store/migrations/000{1..6}_*.sql` — produced in Phase 2 from the DDL above.
- `crates/mn-store/src/entities/` — Rust structs mirroring tables, derived `sqlx::FromRow`.
- `crates/mn-store/src/queries/` — compile-time-checked `sqlx::query_as!` invocations.
