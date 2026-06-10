//! Shared test fixtures for store-level integration tests.
//!
//! These helpers are intentionally independent of `mn-content` so the
//! store tests don't gain a transitive dep on the chunker/embedder.
//! They drive the entity helpers directly.

#![allow(
    dead_code,            // each integration test pulls a subset of helpers
    missing_docs,         // fixture helpers are internal test infrastructure
    clippy::too_many_lines, // fixture setup is verbose by design
)]

use mn_core::provenance::Provenance;
use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mn_store::entities::{chunk, document, embedding_model, node, source, source_version};
use sqlx::PgPool;
use uuid::Uuid;

/// IDs returned by [`ingest_minimal_two_chunk_doc`].
pub struct MinimalDocFixture {
    /// The source UUID.
    pub source_id: Uuid,
    /// The `source_version` UUID.
    pub source_version_id: Uuid,
    /// The document UUID.
    pub document_id: Uuid,
    /// Always contains exactly 2 entries: `[chunk_index_0_id, chunk_index_1_id]`.
    pub chunk_ids: Vec<Uuid>,
}

/// Insert a fresh source with one `source_version` + one document + two chunks.
///
/// The document's `published_url` is `https://example.com/<slug>/first/`.
/// Both chunks have `status = 'ready'` and share the same node hierarchy
/// under a single root node.
///
/// The slug is used verbatim; callers should append a `Uuid::new_v4()` suffix
/// when uniqueness across parallel tests is required, or pass a stable slug
/// when a specific value is asserted in the test (e.g. `"with-context"`).
pub async fn ingest_minimal_two_chunk_doc(pool: &PgPool, slug: &str) -> MinimalDocFixture {
    // Resolve (or create) the canonical bge-base-en-v1.5 rev-1 embedding model.
    // `upsert` is idempotent on (name, revision), so concurrent tests are safe.
    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .expect("upsert embedding model");

    let source_id =
        source::insert(pool, slug, &format!("{slug} (fixture)"), SourceKind::DocsSite, None, 5)
            .await
            .expect("insert source");

    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, None, "0.1.0", "h")
        .await
        .expect("create source_version");

    // Node hierarchy: root → document node → 2 chunk nodes
    let root_node = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .expect("insert root node");

    let doc_node = node::insert(pool, sv_id, Some(root_node), NodeKind::Document, "first.md", 0)
        .await
        .expect("insert document node");

    let provenance = Provenance::default();
    let published_url = format!("https://example.com/{slug}/first/");

    let document_id = document::insert(
        pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: Some(&published_url),
            source_path: "first.md",
            language: Some("en"),
            content_hash: "fixture-hash-first",
            source_modified_at: None,
            frontmatter: None,
            provenance: &provenance,
            package_id: None,
            char_count: 40,
            token_count: 10,
        },
    )
    .await
    .expect("insert document");

    let chunk_node_0 = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "c0", 0)
        .await
        .expect("insert chunk node 0");

    let chunk_node_1 = node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, "c1", 1)
        .await
        .expect("insert chunk node 1");

    let chunk_id_0 = chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id,
            node_id: chunk_node_0,
            chunk_index: 0,
            total_chunks: 2,
            content: "First chunk of the fixture document.",
            content_hash: "fixture-chunk-hash-0",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 36,
            token_count: 7,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .expect("insert chunk 0");

    let chunk_id_1 = chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id,
            node_id: chunk_node_1,
            chunk_index: 1,
            total_chunks: 2,
            content: "Second chunk of the fixture document.",
            content_hash: "fixture-chunk-hash-1",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 37,
            end_byte: 73,
            token_count: 7,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .expect("insert chunk 1");

    MinimalDocFixture {
        source_id,
        source_version_id: sv_id,
        document_id,
        chunk_ids: vec![chunk_id_0, chunk_id_1],
    }
}

/// Insert a fresh source with one `source_version` + one document + `n` chunks
/// (indices `0..n`).
///
/// The document's `published_url` is `https://example.com/<slug>/first/`.
/// All chunks have `status = 'ready'`.
///
/// This is a generalisation of [`ingest_minimal_two_chunk_doc`]; callers that
/// previously used that helper can call this one with `n = 2` and get the
/// same fixture.
pub async fn ingest_n_chunk_doc(pool: &PgPool, slug: &str, n: usize) -> MinimalDocFixture {
    assert!(n >= 1, "ingest_n_chunk_doc requires at least 1 chunk");
    let n = i32::try_from(n).expect("chunk count fits in i32");

    let model_id = embedding_model::upsert(pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .expect("upsert embedding model");

    let source_id =
        source::insert(pool, slug, &format!("{slug} (fixture)"), SourceKind::DocsSite, None, 5)
            .await
            .expect("insert source");

    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, None, "0.1.0", "h")
        .await
        .expect("create source_version");

    let root_node = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .expect("insert root node");

    let doc_node = node::insert(pool, sv_id, Some(root_node), NodeKind::Document, "first.md", 0)
        .await
        .expect("insert document node");

    let provenance = Provenance::default();
    let published_url = format!("https://example.com/{slug}/first/");

    let document_id = document::insert(
        pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: Some(&published_url),
            source_path: "first.md",
            language: Some("en"),
            content_hash: "fixture-hash-first",
            source_modified_at: None,
            frontmatter: None,
            provenance: &provenance,
            package_id: None,
            char_count: 40,
            token_count: 10,
        },
    )
    .await
    .expect("insert document");

    let mut chunk_ids = Vec::with_capacity(usize::try_from(n).unwrap_or(0));
    for i in 0..n {
        let chunk_node =
            node::insert(pool, sv_id, Some(doc_node), NodeKind::Chunk, &format!("c{i}"), i)
                .await
                .expect("insert chunk node");

        let chunk_id = chunk::insert(
            pool,
            chunk::NewChunk {
                source_version_id: sv_id,
                document_id,
                node_id: chunk_node,
                chunk_index: i,
                total_chunks: n,
                content: &format!("Chunk {i} of the fixture document."),
                content_hash: &format!("fixture-chunk-hash-{i}"),
                embedding: None,
                embedding_model_id: model_id,
                code_embedding: None,
                heading_path: &[],
                symbol_path: &[],
                start_byte: i * 40,
                end_byte: (i + 1) * 40,
                token_count: 7,
                status: ChunkStatus::Ready,
            },
        )
        .await
        .expect("insert chunk");

        chunk_ids.push(chunk_id);
    }

    MinimalDocFixture {
        source_id,
        source_version_id: sv_id,
        document_id,
        chunk_ids,
    }
}

/// Set a chunk's status to `embed_failed` in-place.
///
/// Used by navigation tests to verify that `list_next` / `list_prev` skip
/// failed chunks.
pub async fn mark_chunk_failed(pool: &PgPool, chunk_id: Uuid) {
    sqlx::query("UPDATE chunk SET status = 'embed_failed' WHERE id = $1")
        .bind(chunk_id)
        .execute(pool)
        .await
        .expect("mark chunk embed_failed");
}
