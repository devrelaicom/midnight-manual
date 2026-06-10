//! Integration coverage for `POST /v1/search` facet filters, query modes, and
//! fail-fast filter validation (Phase B).
#![cfg(feature = "integration")]
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::cast_precision_loss,
    clippy::missing_const_for_fn
)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use mn_core::provenance::Provenance;
use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mn_server::{app, config::ServerConfig};
use mn_store::entities::{chunk, document, embedding_model, node, source, source_version};
use tower::ServiceExt as _;
use uuid::Uuid;

fn unit_vector(seed: f32) -> Vec<f32> {
    // Deterministic 1024-dim vector for tests (voyage-code-3 width). Not
    // normalized; pgvector cosine operator handles arbitrary magnitudes.
    #[allow(clippy::cast_precision_loss)]
    (0..1024_i32)
        .map(|i| (i as f32).mul_add(0.0001, seed))
        .collect()
}

fn cfg() -> ServerConfig {
    // The corpus model is resolved from the DB by `app::build_resolved`. Each
    // test seeds an active voyage-code-3@1 source_version before building, so
    // resolution yields voyage-code-3@1/1024.
    ServerConfig::default()
}

/// Seed one finalized (active) `source_version` carrying two distinguishable
/// documents, and return `(chunk_a, chunk_b)`:
///
/// - Doc A: `Markdown`, `language = compact`, content is markdown prose. Its
///   single chunk is `chunk_a`.
/// - Doc B: `Code`, `language = typescript`, content is a TS code example. Its
///   single chunk is `chunk_b`.
///
/// The slug is randomized so parallel CI runs don't collide on the unique
/// constraint, and the two chunks use distinct content words so FTS can target
/// exactly one of them.
async fn seed(pool: &sqlx::PgPool) -> (Uuid, Uuid) {
    let model_id = embedding_model::upsert(pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let slug = format!("filters-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Filters", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, None, "0.1.0", "h")
        .await
        .unwrap();
    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let provenance = Provenance::default();

    // Doc A — markdown / compact.
    let doc_node_a = node::insert(pool, sv_id, Some(root), NodeKind::Document, "a.md", 0)
        .await
        .unwrap();
    let doc_a = document::insert(
        pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node_a,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: None,
            source_path: "a.md",
            language: Some("compact"),
            content_hash: "ha-doc",
            source_modified_at: None,
            frontmatter: None,
            provenance: &provenance,
            package_id: None,
            char_count: 0,
            token_count: 0,
        },
    )
    .await
    .unwrap();
    let chunk_node_a = node::insert(pool, sv_id, Some(doc_node_a), NodeKind::Chunk, "a", 0)
        .await
        .unwrap();
    let chunk_a = chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_a,
            node_id: chunk_node_a,
            chunk_index: 0,
            total_chunks: 1,
            content: "compact markdown prose about ledger state",
            content_hash: "ha",
            embedding: Some(unit_vector(0.10)),
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 40,
            token_count: 8,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .unwrap();

    // Doc B — code / typescript.
    let doc_node_b = node::insert(pool, sv_id, Some(root), NodeKind::Document, "b.ts", 1)
        .await
        .unwrap();
    let doc_b = document::insert(
        pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node_b,
            kind: DocumentKind::Code,
            source_url: None,
            published_url: None,
            source_path: "b.ts",
            language: Some("typescript"),
            content_hash: "hb-doc",
            source_modified_at: None,
            frontmatter: None,
            provenance: &provenance,
            package_id: None,
            char_count: 0,
            token_count: 0,
        },
    )
    .await
    .unwrap();
    let chunk_node_b = node::insert(pool, sv_id, Some(doc_node_b), NodeKind::Chunk, "b", 0)
        .await
        .unwrap();
    let chunk_b = chunk::insert(
        pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_b,
            node_id: chunk_node_b,
            chunk_index: 0,
            total_chunks: 1,
            content: "typescript code example calling deployContract",
            content_hash: "hb",
            embedding: Some(unit_vector(0.12)),
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 47,
            token_count: 8,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .unwrap();

    // REQUIRED: flips is_active=true so retrieval considers these chunks.
    source_version::finalize(pool, sv_id).await.unwrap();
    (chunk_a, chunk_b)
}

/// Oneshot `POST /v1/search` -> `(status, parsed JSON)`. A non-JSON body parses
/// to `serde_json::Value::Null` so callers can branch on status alone.
async fn post_search(app: Router, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/search")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

/// Whether `results` contains a chunk with the given id.
fn contains_chunk(v: &serde_json::Value, id: Uuid) -> bool {
    v["results"].as_array().is_some_and(|rs| {
        rs.iter()
            .any(|r| r["chunk_id"].as_str() == Some(id.to_string().as_str()))
    })
}

#[tokio::test]
async fn kind_filter_narrows_to_code() {
    // `kind: {any_of: ["code"]}` adds `d.kind = ANY(...)` at candidate
    // retrieval, so the markdown doc's chunk can never surface. We assert the
    // markdown chunk is ABSENT (the load-bearing claim) and that the code
    // chunk is present (its vector/text are targeted, so it must hit).
    let h = common::boot().await;
    let (chunk_a, chunk_b) = seed(&h.pool).await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    let (status, v) = post_search(
        app,
        serde_json::json!({
            "query": "typescript code example calling deployContract",
            "vector": unit_vector(0.12),
            "client_embedding_model": "voyage-code-3@1",
            "limit": 100,
            "filters": { "kind": { "any_of": ["code"] } },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let results = v["results"].as_array().expect("results array");
    assert!(!results.is_empty(), "kind=code search must return results");
    // The markdown chunk is dropped by the kind predicate at candidate
    // retrieval, so no returned chunk_id may equal chunk_a.
    let a_str = chunk_a.to_string();
    for r in results {
        assert_ne!(
            r["chunk_id"].as_str(),
            Some(a_str.as_str()),
            "markdown chunk must be filtered out by kind=code"
        );
    }
    // The code chunk's vector + text both target it, so it must be present.
    assert!(contains_chunk(&v, chunk_b), "the code chunk must appear under kind=code");
}

#[tokio::test]
async fn language_none_of_excludes() {
    // `language: {none_of: ["typescript"]}` adds `d.language <> ALL(...)`, so
    // the typescript doc's chunk is excluded from every result.
    let h = common::boot().await;
    let (_chunk_a, chunk_b) = seed(&h.pool).await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    let (status, v) = post_search(
        app,
        serde_json::json!({
            "query": "ledger state typescript deployContract",
            "vector": unit_vector(0.11),
            "client_embedding_model": "voyage-code-3@1",
            "limit": 100,
            "filters": { "language": { "none_of": ["typescript"] } },
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let results = v["results"].as_array().expect("results array");
    let b_str = chunk_b.to_string();
    for r in results {
        assert_ne!(
            r["chunk_id"].as_str(),
            Some(b_str.as_str()),
            "typescript chunk must be excluded by language none_of"
        );
    }
}

#[tokio::test]
async fn fts_mode_runs_without_vector() {
    // mode=fts skips embedding entirely: no `vector`, no
    // `client_embedding_model`. The query words come from chunk_b's content so
    // `websearch_to_tsquery('english', ...)` lexically hits at least one chunk.
    let h = common::boot().await;
    let (_chunk_a, _chunk_b) = seed(&h.pool).await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    let (status, v) = post_search(
        app,
        serde_json::json!({
            "query": "deployContract typescript",
            "mode": "fts",
            "limit": 100,
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let results = v["results"].as_array().expect("results array");
    assert!(
        !results.is_empty(),
        "fts-mode search must return a lexical hit without any vector"
    );
}

#[tokio::test]
async fn unknown_filter_key_is_rejected() {
    // `langauge` (typo) is an unknown field on `SearchFilters`, whose
    // `#[serde(deny_unknown_fields)]` makes the body fail to deserialize. axum
    // 0.7's `Json` extractor maps that deserialization failure to 422
    // Unprocessable Entity (a `JsonDataError`) — NOT 400, which it reserves for
    // malformed JSON syntax. The load-bearing claim is that the typo is
    // REJECTED (a 4xx), proving the old silent-drop is gone; the exact code is
    // axum's. (Primary clients — the MCP `search` tool and the CLI — also
    // validate filters pre-flight and surface a richer message.) The rejection
    // fires at extraction, before the handler, so vector/model are immaterial,
    // but we still send a plausible hybrid body.
    let h = common::boot().await;
    let _ = seed(&h.pool).await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    let (status, _v) = post_search(
        app,
        serde_json::json!({
            "query": "ledger state",
            "vector": unit_vector(0.10),
            "client_embedding_model": "voyage-code-3@1",
            "limit": 100,
            "filters": { "langauge": { "any_of": ["compact"] } },
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unknown filter key must be rejected, not silently dropped"
    );
}

#[tokio::test]
async fn vector_mode_requires_vector_400() {
    // A vector-mode convenience request with no `vector` is rejected:
    // `normalize_queries` requires both `query` and `vector` outside fts mode.
    let h = common::boot().await;
    let _ = seed(&h.pool).await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    let (status, _v) = post_search(
        app,
        serde_json::json!({
            "query": "x",
            "mode": "vector",
        }),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "vector mode with no usable vector must be rejected"
    );
}
