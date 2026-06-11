//! End-to-end integration coverage for contextualized dual embeddings
//! (spec 2026-06-10, plan Task 18).
//!
//! Walks the full HTTP lifecycle against a real Postgres (via
//! `common::boot()`, migrations 0001–0011): start an ingest run with BOTH
//! `embedding_model: voyage-context-3@1` and
//! `code_embedding_model: voyage-code-3@1`, upload one markdown document
//! (general embedding only) and one code document (general + code embedding,
//! synthetic 1024-dim vectors), finalize, then exercise `POST /v1/search`
//! across the three `code_mode` values. Also covers the opt-out path (a run
//! started WITHOUT a code model rejects `code_embedding` uploads with 400 and
//! its chunks never join the code-vector candidate list) and the migration
//! 0011 trigger (a direct chunk insert carrying `code_embedding` under a
//! code-model-less source_version raises).
//!
//! `fts` + explicit `code_mode` → 400 is covered by
//! `search_route.rs::fts_mode_rejects_explicit_code_mode_400`.

#![cfg(feature = "integration")]
#![allow(
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::cast_precision_loss
)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use mn_auth::Keypair;
use mn_core::provenance::Provenance;
use mn_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mn_server::{app, config::ServerConfig};
use mn_store::entities::{chunk, document, embedding_model, node, source, source_version};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const GENERAL_MODEL: &str = "voyage-context-3@1";
const CODE_MODEL: &str = "voyage-code-3@1";

/// Deterministic 1024-dim vector (the voyage-context-3 / voyage-code-3 width).
/// Not normalized; pgvector's cosine operator handles arbitrary magnitudes.
fn vec1024(seed: f32) -> Vec<f32> {
    (0..1024_i32)
        .map(|i| (i as f32).mul_add(0.0001, seed))
        .collect()
}

// ── HTTP + auth helpers (mirroring admin_ingest_endpoints.rs) ───────────────

fn cfg_with_auth(user_store_body: String, jwt_secret_bytes: Vec<u8>) -> ServerConfig {
    ServerConfig {
        user_store_body: Some(user_store_body),
        jwt_secret: Some(jwt_secret_bytes),
        ..Default::default()
    }
}

fn user_store_for(user_id: &str, kp: &Keypair) -> String {
    format!(
        r#"
schema_version = 1

[[users]]
user_id = "{user_id}"
role = "admin"
public_key = "{wire}"
created_at = "2026-05-14"
"#,
        wire = kp.public_wire(),
    )
}

async fn json_call(
    app: axum::Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    let req = if let Some(b) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    };
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 8 * 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn mint_admin_token(app: axum::Router, user_id: &str, kp: &Keypair) -> String {
    let (_, body) = json_call(
        app.clone(),
        "POST",
        "/v1/auth/challenge",
        None,
        Some(json!({"user_id": user_id})),
    )
    .await;
    let challenge_id = body["challenge_id"].as_str().unwrap().to_owned();
    let nonce = STANDARD_NO_PAD
        .decode(body["nonce_b64"].as_str().unwrap())
        .unwrap();
    let signature_b64 = STANDARD_NO_PAD.encode(kp.sign(&nonce));
    let (_, body) = json_call(
        app,
        "POST",
        "/v1/auth/verify",
        None,
        Some(json!({"challenge_id": challenge_id, "signature_b64": signature_b64})),
    )
    .await;
    body["token"].as_str().unwrap().to_owned()
}

/// Boot an app with admin auth, mint a token, and seed a fresh source.
/// Returns `(app, token, slug, source_id)`.
async fn auth_app_with_source(pool: &sqlx::PgPool) -> (axum::Router, String, String, Uuid) {
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("aaron", &kp), vec![7u8; 32]);
    // `build_resolved`: on a fresh post-0011 schema (no active source_versions)
    // the general corpus model falls back to the newest registry row —
    // voyage-context-3@1 — and the code model resolves from config
    // (voyage-code-3@1). Finalize re-resolves the shared handle in place.
    let app = app::build_resolved(pool.clone(), cfg)
        .await
        .expect("build app");
    let token = mint_admin_token(app.clone(), "aaron", &kp).await;
    let slug = format!("dual-embed-e2e-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Dual Embed E2E", SourceKind::Mixed, None, 5)
        .await
        .expect("seed source");
    (app, token, slug, source_id)
}

fn hash_of(s: &str) -> String {
    // Trivial FNV-style fold — only determinism matters; the server stores
    // whatever hash the client sends.
    let mut acc: u64 = 1_469_598_103_934_665_603;
    for b in s.bytes() {
        acc = acc
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(u64::from(b));
    }
    format!("{acc:016x}")
}

/// Markdown document payload: one chunk with a general embedding only.
fn md_doc_payload(path: &str, content: &str, embedding: &[f32]) -> Value {
    json!({
        "path": path, "kind": "markdown",
        "content_hash": format!("h:{path}:{}", hash_of(content)),
        "char_count": content.len(), "token_count": 0, "provenance": {},
        "chunks": [{
            "chunk_index": 0, "total_chunks": 1, "content": content,
            "content_hash": format!("c:{path}:{}", hash_of(content)),
            "heading_path": [], "symbol_path": [],
            "start_byte": 0, "end_byte": content.len(), "token_count": 0,
            "embedding": embedding,
        }],
    })
}

/// Code document payload: one chunk with a general embedding and an optional
/// code embedding.
fn code_doc_payload(
    path: &str,
    content: &str,
    embedding: &[f32],
    code_embedding: Option<&[f32]>,
) -> Value {
    let mut chunk = json!({
        "chunk_index": 0, "total_chunks": 1, "content": content,
        "content_hash": format!("c:{path}:{}", hash_of(content)),
        "heading_path": [],
        "symbol_path": [{"kind": "function", "name": "frobnicate_widget"}],
        "start_byte": 0, "end_byte": content.len(), "token_count": 0,
        "embedding": embedding,
    });
    if let Some(cv) = code_embedding {
        chunk["code_embedding"] = json!(cv);
    }
    json!({
        "path": path, "kind": "code", "language": "rust",
        "content_hash": format!("h:{path}:{}", hash_of(content)),
        "char_count": content.len(), "token_count": 0, "provenance": {},
        "chunks": [chunk],
    })
}

const MD_CONTENT: &str = "Conceptual overview of the midnight zswap ledger";
const CODE_CONTENT: &str = "pub fn frobnicate_widget(input: &Widget) -> Frob { input.frob() }";

/// Seed vectors. The query pair (general 0.11 / code 0.51) sits nearest the
/// markdown chunk's general vector and the code chunk's code vector; the code
/// chunk's GENERAL vector is deliberately far so the code chunk's top ranking
/// in code-vector lists comes from the code column, not the general one.
const MD_GENERAL_SEED: f32 = 0.10;
const CODE_GENERAL_SEED: f32 = 0.90;
const CODE_CODE_SEED: f32 = 0.50;

// The `_id` postfix is load-bearing here (these are chunk/source_version ids,
// not the entities themselves).
#[allow(clippy::struct_field_names)]
struct DualCorpus {
    sv_id: Uuid,
    md_chunk_id: Uuid,
    code_chunk_id: Uuid,
}

/// Drive the full dual-embedding ingest lifecycle over HTTP:
/// start-run (general + code model) → upload md + code docs → finalize.
async fn ingest_dual_source(
    app: &axum::Router,
    pool: &sqlx::PgPool,
    token: &str,
    slug: &str,
) -> DualCorpus {
    // 1. Start the run, declaring both models.
    let (status, body) = json_call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        Some(token),
        Some(json!({
            "ingest_cli_version": "0.1.0-test",
            "embedding_model": GENERAL_MODEL,
            "code_embedding_model": CODE_MODEL,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start run: {body}");
    let run_id = body["ingest_run_id"].as_str().unwrap().to_owned();
    let sv_id: Uuid = run_id.parse().unwrap();

    // 2. Upload one markdown doc (general only) + one code doc (general + code).
    let (status, body) = json_call(
        app.clone(),
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(token),
        Some(json!({
            "embedding_model": GENERAL_MODEL,
            "documents": [
                md_doc_payload("overview.md", MD_CONTENT, &vec1024(MD_GENERAL_SEED)),
                code_doc_payload(
                    "src/widget.rs",
                    CODE_CONTENT,
                    &vec1024(CODE_GENERAL_SEED),
                    Some(&vec1024(CODE_CODE_SEED)),
                ),
            ],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload: {body}");
    assert_eq!(body["accepted"], 2, "upload: {body}");

    // 3. Finalize → active.
    let (status, body) = json_call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/finalize"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "finalize: {body}");
    assert_eq!(body["is_active"], true, "finalize: {body}");

    let chunk_id_for = |path: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Uuid>(
                "SELECT c.id FROM chunk c JOIN document d ON d.id = c.document_id \
                 WHERE d.source_version_id = $1 AND d.source_path = $2",
            )
            .bind(sv_id)
            .bind(path)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|e| panic!("chunk for {path}: {e}"))
        }
    };
    DualCorpus {
        sv_id,
        md_chunk_id: chunk_id_for("overview.md").await,
        code_chunk_id: chunk_id_for("src/widget.rs").await,
    }
}

/// Oneshot `POST /v1/search` returning `(status, parsed JSON)`.
async fn post_search(app: axum::Router, body: Value) -> (StatusCode, Value) {
    json_call(app, "POST", "/v1/search", None, Some(body)).await
}

fn result_chunk_ids(v: &Value) -> Vec<String> {
    v["results"]
        .as_array()
        .unwrap_or_else(|| panic!("results array missing: {v}"))
        .iter()
        .map(|r| r["chunk_id"].as_str().unwrap().to_owned())
        .collect()
}

// ── Group 1: dual ingest persists both vector columns ───────────────────────

#[tokio::test]
async fn dual_ingest_persists_general_and_code_vectors() {
    let h = common::boot().await;
    let (app, token, slug, _source_id) = auth_app_with_source(&h.pool).await;
    let corpus = ingest_dual_source(&app, &h.pool, &token, &slug).await;

    // The source_version records the code model.
    let code_model_id: Option<Uuid> =
        sqlx::query_scalar("SELECT code_embedding_model_id FROM source_version WHERE id = $1")
            .bind(corpus.sv_id)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    let expected = embedding_model::upsert(&h.pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    assert_eq!(
        code_model_id,
        Some(expected),
        "source_version.code_embedding_model_id must point at voyage-code-3@1"
    );

    // Markdown chunk: general vector only. Code chunk: both vectors.
    let (md_gen, md_code, md_status) = sqlx::query_as::<_, (bool, bool, String)>(
        "SELECT embedding IS NOT NULL, code_embedding IS NOT NULL, status::text \
         FROM chunk WHERE id = $1",
    )
    .bind(corpus.md_chunk_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert!(md_gen, "markdown chunk must carry a general embedding");
    assert!(!md_code, "markdown chunk must NOT carry a code embedding");
    assert_eq!(md_status, "ready");

    let (code_gen, code_code, code_status) = sqlx::query_as::<_, (bool, bool, String)>(
        "SELECT embedding IS NOT NULL, code_embedding IS NOT NULL, status::text \
         FROM chunk WHERE id = $1",
    )
    .bind(corpus.code_chunk_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert!(code_gen, "code chunk must carry a general embedding");
    assert!(code_code, "code chunk must carry a code embedding");
    assert_eq!(code_status, "ready");
}

// ── Group 2: code_mode search behaviors over the dual corpus ────────────────

#[tokio::test]
async fn code_mode_search_behaviors_over_dual_corpus() {
    let h = common::boot().await;
    let (app, token, slug, _source_id) = auth_app_with_source(&h.pool).await;
    let corpus = ingest_dual_source(&app, &h.pool, &token, &slug).await;
    let md_id = corpus.md_chunk_id.to_string();
    let code_id = corpus.code_chunk_id.to_string();

    // (a) Hybrid, code_mode defaulted → effective mode `on`; the code chunk
    // is reachable via the code-vector list (it is the schema's only chunk
    // with a code embedding, so code_vector_candidates == 1).
    let (status, v) = post_search(
        app.clone(),
        json!({
            "queries": [{
                "text": "frobnicate widget",
                "vector": vec1024(0.11),
                "code_vector": vec1024(0.51),
            }],
            "client_embedding_model": GENERAL_MODEL,
            "client_code_embedding_model": CODE_MODEL,
            "limit": 50,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["search_metadata"]["code_mode"], "on");
    let pq = &v["search_metadata"]["per_query"][0];
    assert_eq!(
        pq["code_vector_candidates"], 1,
        "exactly the one code chunk carries a code embedding: {v}"
    );
    let ids = result_chunk_ids(&v);
    assert!(ids.contains(&code_id), "code chunk must be in the fused results: {v}");
    assert!(ids.contains(&md_id), "markdown chunk must be in the fused results: {v}");

    // (b) code_mode=off → pre-cutover behavior: no code list ran
    // (code_vector_candidates == 0) and no code-model fields are required.
    let (status, v) = post_search(
        app.clone(),
        json!({
            "queries": [{ "text": "frobnicate widget", "vector": vec1024(0.11) }],
            "client_embedding_model": GENERAL_MODEL,
            "code_mode": "off",
            "limit": 50,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["search_metadata"]["code_mode"], "off");
    assert_eq!(
        v["search_metadata"]["per_query"][0]["code_vector_candidates"], 0,
        "off must not run the code-vector list: {v}"
    );

    // (c) mode=vector + code_mode=exclusive → the code list REPLACES the
    // general list: an empty general vector is permitted, the markdown chunk
    // (no code embedding) cannot surface, and the code chunk can.
    let (status, v) = post_search(
        app.clone(),
        json!({
            "queries": [{ "text": "", "vector": [], "code_vector": vec1024(0.51) }],
            "mode": "vector",
            "code_mode": "exclusive",
            "client_code_embedding_model": CODE_MODEL,
            "limit": 50,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["search_metadata"]["code_mode"], "exclusive");
    let ids = result_chunk_ids(&v);
    assert!(
        ids.contains(&code_id),
        "code chunk must be reachable via the exclusive code list: {v}"
    );
    assert!(
        !ids.contains(&md_id),
        "markdown chunk must be absent from vector-derived results in exclusive mode: {v}"
    );
}

// ── Group 3: opt-out source (no code_embedding_model on start-run) ──────────

#[tokio::test]
async fn opt_out_source_rejects_code_vectors_and_never_joins_code_candidates() {
    let h = common::boot().await;
    let (app, token, slug, _source_id) = auth_app_with_source(&h.pool).await;

    // Start WITHOUT code_embedding_model.
    let (status, body) = json_call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        Some(&token),
        Some(json!({
            "ingest_cli_version": "0.1.0-test",
            "embedding_model": GENERAL_MODEL,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start run: {body}");
    let run_id = body["ingest_run_id"].as_str().unwrap().to_owned();

    // Uploading a chunk WITH a code_embedding under an opt-out run is a 400.
    let (status, body) = json_call(
        app.clone(),
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(&token),
        Some(json!({
            "embedding_model": GENERAL_MODEL,
            "documents": [code_doc_payload(
                "src/widget.rs",
                CODE_CONTENT,
                &vec1024(CODE_GENERAL_SEED),
                Some(&vec1024(CODE_CODE_SEED)),
            )],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("code_embedding"),
        "error names the offending field: {body}"
    );

    // The same document WITHOUT the code vector is accepted; finalize → active.
    let (status, body) = json_call(
        app.clone(),
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(&token),
        Some(json!({
            "embedding_model": GENERAL_MODEL,
            "documents": [code_doc_payload(
                "src/widget.rs",
                CODE_CONTENT,
                &vec1024(CODE_GENERAL_SEED),
                None,
            )],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accepted"], 1, "{body}");
    let (status, body) = json_call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/finalize"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "finalize: {body}");

    // code_mode=on over the opt-out corpus: the chunk is live (reachable via
    // the general list) but NEVER appears in code-vector candidates.
    let sv_id: Uuid = run_id.parse().unwrap();
    let chunk_id: String = sqlx::query_scalar::<_, Uuid>(
        "SELECT c.id FROM chunk c JOIN document d ON d.id = c.document_id \
         WHERE d.source_version_id = $1",
    )
    .bind(sv_id)
    .fetch_one(&h.pool)
    .await
    .unwrap()
    .to_string();

    let (status, v) = post_search(
        app.clone(),
        json!({
            "queries": [{
                "text": "frobnicate widget",
                // Near the chunk's general vector so the general list finds it.
                "vector": vec1024(CODE_GENERAL_SEED + 0.01),
                "code_vector": vec1024(0.51),
            }],
            "client_embedding_model": GENERAL_MODEL,
            "client_code_embedding_model": CODE_MODEL,
            "code_mode": "on",
            "limit": 50,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v["search_metadata"]["per_query"][0]["code_vector_candidates"], 0,
        "opt-out chunks must never join the code-vector candidate list: {v}"
    );
    assert!(
        result_chunk_ids(&v).contains(&chunk_id),
        "the opt-out chunk is still reachable via the general list: {v}"
    );
}

// ── Group 5: migration-0011 trigger ─────────────────────────────────────────

#[tokio::test]
async fn db_trigger_rejects_code_embedding_without_code_model() {
    let h = common::boot().await;

    // Seed a source_version with NO code_embedding_model_id, straight through
    // the entity layer (this is below the HTTP validation, so the only guard
    // left is the trigger installed by migration 0011).
    let model_id = embedding_model::upsert(&h.pool, "voyage-context-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let slug = format!("trigger-test-{}", Uuid::new_v4());
    let source_id = source::insert(&h.pool, &slug, "Trigger", SourceKind::Mixed, None, 5)
        .await
        .unwrap();
    let (sv_id, _) =
        source_version::create_building(&h.pool, source_id, model_id, None, "0.1.0", "h")
            .await
            .unwrap();
    let root = node::insert(&h.pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let doc_node = node::insert(&h.pool, sv_id, Some(root), NodeKind::Document, "w.rs", 0)
        .await
        .unwrap();
    let provenance = Provenance::default();
    let doc_id = document::insert(
        &h.pool,
        document::NewDocument {
            source_version_id: sv_id,
            node_id: doc_node,
            kind: DocumentKind::Code,
            source_url: None,
            published_url: None,
            source_path: "w.rs",
            language: Some("rust"),
            content_hash: "h",
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
    let chunk_node = node::insert(&h.pool, sv_id, Some(doc_node), NodeKind::Chunk, "c", 0)
        .await
        .unwrap();

    // Inserting a chunk that carries a code_embedding must raise: the owning
    // source_version declares no code_embedding_model_id.
    let err = chunk::insert(
        &h.pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: chunk_node,
            chunk_index: 0,
            total_chunks: 1,
            content: CODE_CONTENT,
            content_hash: "hc",
            embedding: Some(vec1024(0.2)),
            embedding_model_id: model_id,
            code_embedding: Some(vec1024(0.5)),
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 10,
            token_count: 4,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .expect_err("trigger must reject code_embedding under a code-model-less source_version");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("code_embedding_model_id"),
        "trigger exception names the missing column, got: {msg}"
    );

    // The identical insert WITHOUT the code vector succeeds — the trigger
    // only fires on the code_embedding/code-model mismatch.
    chunk::insert(
        &h.pool,
        chunk::NewChunk {
            source_version_id: sv_id,
            document_id: doc_id,
            node_id: chunk_node,
            chunk_index: 0,
            total_chunks: 1,
            content: CODE_CONTENT,
            content_hash: "hc",
            embedding: Some(vec1024(0.2)),
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 10,
            token_count: 4,
            status: ChunkStatus::Ready,
        },
    )
    .await
    .expect("code-vector-less insert must pass the trigger");
}
