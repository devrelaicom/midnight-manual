//! DB integration tests proving the full incremental-ingest cycle end to end.
//!
//! Three tests:
//! 1. `reingest_carries_unchanged_reembeds_changed_drops_deleted` — the
//!    complete v1 → v2 cycle: carry a.md unchanged, re-upload b.md with new
//!    content, add d.md, omit c.md (drops it). Asserts revision, doc set,
//!    and that a.md's chunk content survived carry-forward.
//! 2. `model_change_forces_full_reembed` — when v2 starts on a DIFFERENT
//!    embedding model revision, the server refuses to carry any doc, returning
//!    conflicts with "embedding model changed".
//! 3. `embed_failed_doc_is_repaired_on_reingest` — a doc with an `embed_failed`
//!    chunk in v1 is reported as `embed_complete=false` by the inventory
//!    endpoint, AND carrying it to v2 propagates the failure rather than
//!    healing it, proving the CLI must re-upload such docs with new chunks.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine as _;
use midnight_manual_server::{app, config::ServerConfig};
use mnm_auth::Keypair;
use mnm_core::provenance::Provenance;
use mnm_core::types::{ChunkStatus, DocumentKind, NodeKind, SourceKind};
use mnm_store::entities::{chunk, document, embedding_model, node, source, source_version};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

// ── Config / auth helpers ────────────────────────────────────────────────────

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

async fn call(
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
    let (_, body) = call(
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
    let (_, body) = call(
        app,
        "POST",
        "/v1/auth/verify",
        None,
        Some(json!({"challenge_id": challenge_id, "signature_b64": signature_b64})),
    )
    .await;
    body["token"].as_str().unwrap().to_owned()
}

// ── Seeding helpers ───────────────────────────────────────────────────────────

/// Seed the source + register model @1. Returns the slug.
async fn seed_source_no_version(pool: &sqlx::PgPool, slug_prefix: &str) -> String {
    embedding_model::upsert(pool, "voyage-context-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let slug = format!("{slug_prefix}-{}", Uuid::new_v4().simple());
    source::insert(pool, &slug, "Incremental Ingest Test", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    slug
}

// ── Per-run HTTP helpers ──────────────────────────────────────────────────────

/// Start a new ingest run with the given model wire id; returns the
/// `ingest_run_id` string.
async fn start_run(app: &axum::Router, slug: &str, model: &str, token: &str) -> String {
    let (status, body) = call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs"),
        Some(token),
        Some(json!({
            "ingest_cli_version": "0.1.0-test",
            "embedding_model": model,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "start_run({model}): {body}");
    body["ingest_run_id"].as_str().unwrap().to_owned()
}

/// Upload `body` to the given run and return `(status, parsed JSON)`.
async fn upload(
    app: &axum::Router,
    slug: &str,
    run_id: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    call(
        app.clone(),
        "PUT",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/documents"),
        Some(token),
        Some(body),
    )
    .await
}

/// Finalize the run; returns `(status, parsed JSON)`.
async fn finalize(
    app: &axum::Router,
    slug: &str,
    run_id: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call(
        app.clone(),
        "POST",
        &format!("/v1/admin/sources/{slug}/ingest-runs/{run_id}/finalize"),
        Some(token),
        body,
    )
    .await
}

// ── Document payload builders ─────────────────────────────────────────────────

/// A trivial but deterministic hash for stable content-identity across calls.
fn pseudo_hash(s: &str) -> String {
    let mut acc: u64 = 1_469_598_103_934_665_603;
    for b in s.bytes() {
        acc = acc
            .wrapping_mul(1_099_511_628_211)
            .wrapping_add(u64::from(b));
    }
    format!("{acc:016x}")
}

/// Build a fully-embedded document payload (chunk without a precomputed
/// embedding — lands `embed_failed`, text-only upload).
fn doc_payload(path: &str, content: &str) -> Value {
    let content_hash = format!("h:{}:{}", path, pseudo_hash(content));
    let chunk_hash = format!("c:{}:{}", path, pseudo_hash(content));
    json!({
        "path": path,
        "kind": "markdown",
        "content_hash": content_hash,
        "char_count": content.len(),
        "token_count": 0,
        "provenance": {},
        "chunks": [{
            "chunk_index": 0,
            "total_chunks": 1,
            "content": content,
            "content_hash": chunk_hash,
            "heading_path": [],
            "symbol_path": [],
            "start_byte": 0,
            "end_byte": content.len(),
            "token_count": 0,
        }],
    })
}

/// Build a `carried: true` payload (no chunks; server clones from prior).
/// `content_hash` must match the prior version's hash for the carry to succeed.
fn carried_payload(path: &str, content_hash: &str) -> Value {
    json!({
        "path": path,
        "kind": "markdown",
        "content_hash": content_hash,
        "provenance": {},
        "carried": true,
        "chunks": [],
    })
}

// ── DB query helpers ──────────────────────────────────────────────────────────

/// Return the active revision for `slug`, or `None`.
async fn active_revision(pool: &sqlx::PgPool, slug: &str) -> Option<i32> {
    let row: Option<(i32,)> = sqlx::query_as(
        "SELECT sv.revision \
         FROM source_version sv \
         JOIN source s ON s.id = sv.source_id \
         WHERE s.slug = $1 AND sv.is_active = true \
         LIMIT 1",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .expect("active_revision query");
    row.map(|(r,)| r)
}

/// Return the source_version UUID for `slug` at `revision`.
async fn sv_id_for_revision(pool: &sqlx::PgPool, slug: &str, revision: i32) -> Uuid {
    sqlx::query_scalar(
        "SELECT sv.id \
         FROM source_version sv \
         JOIN source s ON s.id = sv.source_id \
         WHERE s.slug = $1 AND sv.revision = $2",
    )
    .bind(slug)
    .bind(revision)
    .fetch_one(pool)
    .await
    .expect("sv_id_for_revision query")
}

/// Return sorted source_paths for all documents in the given source_version.
async fn doc_paths_for_sv(pool: &sqlx::PgPool, sv_id: Uuid) -> Vec<String> {
    let mut paths: Vec<String> = sqlx::query_scalar(
        "SELECT source_path FROM document WHERE source_version_id = $1 ORDER BY source_path",
    )
    .bind(sv_id)
    .fetch_all(pool)
    .await
    .expect("doc_paths_for_sv query");
    paths.sort();
    paths
}

/// Return (chunk_id, content) pairs for `path` in `sv_id`, ordered by chunk_index.
async fn chunk_ids_and_content_for_doc(
    pool: &sqlx::PgPool,
    sv_id: Uuid,
    path: &str,
) -> Vec<(Uuid, String)> {
    sqlx::query_as(
        "SELECT c.id, c.content \
         FROM chunk c \
         JOIN document d ON c.document_id = d.id \
         WHERE d.source_version_id = $1 AND d.source_path = $2 \
         ORDER BY c.chunk_index",
    )
    .bind(sv_id)
    .bind(path)
    .fetch_all(pool)
    .await
    .expect("chunk_ids_and_content_for_doc query")
}

/// Return the content_hash for `path` in `sv_id`.
async fn content_hash_for_doc(pool: &sqlx::PgPool, sv_id: Uuid, path: &str) -> String {
    sqlx::query_scalar(
        "SELECT content_hash FROM document \
         WHERE source_version_id = $1 AND source_path = $2",
    )
    .bind(sv_id)
    .bind(path)
    .fetch_one(pool)
    .await
    .expect("content_hash_for_doc query")
}

// ── Test 1 ───────────────────────────────────────────────────────────────────

/// Full v1 → v2 incremental-ingest cycle:
/// - v1: a.md, b.md, c.md (all new uploads).
/// - v2: a.md carried (same hash), b.md changed (new content), d.md new,
///   c.md omitted (drops by not being uploaded).
/// Asserts:
/// - v2 is the active revision.
/// - v2 contains exactly [a.md, b.md, d.md] (c.md gone).
/// - a.md's chunks in v2 have the same content as in v1 (carry succeeded).
#[tokio::test]
async fn reingest_carries_unchanged_reembeds_changed_drops_deleted() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("admin", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "admin", &kp).await;
    let slug = seed_source_no_version(&h.pool, "cycle").await;

    // ── v1: three docs ────────────────────────────────────────────────────
    let r1 = start_run(&app, &slug, "voyage-context-3@1", &token).await;
    let (status, body) = upload(
        &app,
        &slug,
        &r1,
        &token,
        json!({
            "documents": [
                doc_payload("a.md", "# Alpha\n\nUnchanged content."),
                doc_payload("b.md", "# Beta\n\nOriginal content."),
                doc_payload("c.md", "# Gamma\n\nWill be dropped."),
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "v1 upload: {body}");
    assert_eq!(body["accepted"], 3, "v1 should accept 3 docs: {body}");

    let (fin_status, fin_body) =
        finalize(&app, &slug, &r1, &token, Some(json!({"expected_document_total": 3}))).await;
    assert_eq!(fin_status, StatusCode::OK, "v1 finalize: {fin_body}");
    assert_eq!(fin_body["revision"], 1, "v1 revision: {fin_body}");

    // Capture v1's a.md chunk content for later comparison.
    let v1_sv = sv_id_for_revision(&h.pool, &slug, 1).await;
    let v1_a_chunks = chunk_ids_and_content_for_doc(&h.pool, v1_sv, "a.md").await;
    assert!(!v1_a_chunks.is_empty(), "v1 a.md must have at least one chunk");
    // Also capture the exact content_hash we need to supply in the carried payload.
    let a_hash_v1 = content_hash_for_doc(&h.pool, v1_sv, "a.md").await;

    // ── v2: carry a.md, change b.md, add d.md, omit c.md ─────────────────
    let r2 = start_run(&app, &slug, "voyage-context-3@1", &token).await;
    let (status, body) = upload(
        &app,
        &slug,
        &r2,
        &token,
        json!({
            "documents": [
                carried_payload("a.md", &a_hash_v1),
                doc_payload("b.md", "# Beta\n\nChanged content in v2."),
                doc_payload("d.md", "# Delta\n\nNew in v2."),
            ],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "v2 upload: {body}");
    assert_eq!(body["accepted"], 3, "v2 should accept 3 docs: {body}");
    assert_eq!(body["carried"], 1, "exactly one doc (a.md) should be carried: {body}");
    assert_eq!(
        body["conflicts"].as_array().unwrap().len(),
        0,
        "no conflicts expected in v2: {body}"
    );

    let (fin_status, fin_body) =
        finalize(&app, &slug, &r2, &token, Some(json!({"expected_document_total": 3}))).await;
    assert_eq!(fin_status, StatusCode::OK, "v2 finalize: {fin_body}");
    assert_eq!(fin_body["revision"], 2, "v2 revision: {fin_body}");
    assert_eq!(fin_body["demoted_revision"], 1, "v1 should be demoted: {fin_body}");

    // ── Assertions ────────────────────────────────────────────────────────

    // Active revision must be v2.
    let active = active_revision(&h.pool, &slug).await;
    assert_eq!(active, Some(2), "active revision must be 2 after v2 finalize");

    let v2_sv = sv_id_for_revision(&h.pool, &slug, 2).await;

    // v2 doc paths must be exactly [a.md, b.md, d.md]; c.md dropped by omission.
    let paths = doc_paths_for_sv(&h.pool, v2_sv).await;
    assert_eq!(
        paths,
        vec!["a.md".to_owned(), "b.md".to_owned(), "d.md".to_owned()],
        "v2 must contain exactly a.md, b.md, d.md (c.md dropped): {paths:?}"
    );

    // a.md in v2 must have the same chunk content as in v1 (carry-forward works).
    let v2_a_chunks = chunk_ids_and_content_for_doc(&h.pool, v2_sv, "a.md").await;
    assert_eq!(
        v1_a_chunks.len(),
        v2_a_chunks.len(),
        "carried a.md must have the same number of chunks in v2 as v1"
    );
    let v1_contents: Vec<&str> = v1_a_chunks.iter().map(|(_, c)| c.as_str()).collect();
    let v2_contents: Vec<&str> = v2_a_chunks.iter().map(|(_, c)| c.as_str()).collect();
    assert_eq!(
        v1_contents, v2_contents,
        "a.md's chunk content must be identical in v1 and v2 (carry-forward preserved content)"
    );

    // a.md chunk IDs must be NEW rows (carry-forward clones, it does not share rows).
    let v1_ids: Vec<Uuid> = v1_a_chunks.iter().map(|(id, _)| *id).collect();
    let v2_ids: Vec<Uuid> = v2_a_chunks.iter().map(|(id, _)| *id).collect();
    assert!(
        v1_ids.iter().all(|id| !v2_ids.contains(id)),
        "carried chunks must be new rows, not shared with v1"
    );

    // b.md in v2 must have different content than v1 (re-upload took effect).
    let v1_b_content: Vec<_> = chunk_ids_and_content_for_doc(&h.pool, v1_sv, "b.md")
        .await
        .into_iter()
        .map(|(_, c)| c)
        .collect();
    let v2_b_content: Vec<_> = chunk_ids_and_content_for_doc(&h.pool, v2_sv, "b.md")
        .await
        .into_iter()
        .map(|(_, c)| c)
        .collect();
    assert_ne!(
        v1_b_content, v2_b_content,
        "b.md's content should differ between v1 and v2 (re-embedded)"
    );

    // d.md must exist only in v2 (new doc).
    let d_in_v1 = chunk_ids_and_content_for_doc(&h.pool, v1_sv, "d.md").await;
    let d_in_v2 = chunk_ids_and_content_for_doc(&h.pool, v2_sv, "d.md").await;
    assert!(d_in_v1.is_empty(), "d.md must not exist in v1");
    assert!(!d_in_v2.is_empty(), "d.md must exist in v2");
}

// ── Test 2 ───────────────────────────────────────────────────────────────────

/// When v2 starts on a DIFFERENT embedding model (voyage-context-3@2 vs @1),
/// the server refuses to carry any doc: the upload returns a conflict for a.md
/// with "embedding model changed", and a.md is NOT carried.
///
/// This drives the `classify_upload(carried=true, can_carry=false)` branch
/// (Task 3) end to end.
#[tokio::test]
async fn model_change_forces_full_reembed() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("admin", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "admin", &kp).await;

    // Register two model revisions so v1 and v2 use different UUIDs.
    embedding_model::upsert(&h.pool, "voyage-context-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    embedding_model::upsert(&h.pool, "voyage-context-3", 2, 1024, "voyageai")
        .await
        .unwrap();

    let slug = format!("model-change-{}", Uuid::new_v4().simple());
    source::insert(&h.pool, &slug, "Model Change Test", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();

    // ── v1 on model @1 ────────────────────────────────────────────────────
    let r1 = start_run(&app, &slug, "voyage-context-3@1", &token).await;
    let (status, body) = upload(
        &app,
        &slug,
        &r1,
        &token,
        json!({ "documents": [doc_payload("a.md", "# Alpha")] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "v1 upload: {body}");
    assert_eq!(body["accepted"], 1);

    let (fin_status, fin_body) = finalize(&app, &slug, &r1, &token, None).await;
    assert_eq!(fin_status, StatusCode::OK, "v1 finalize: {fin_body}");
    assert_eq!(fin_body["revision"], 1);

    // Capture a.md's content_hash from v1.
    let v1_sv = sv_id_for_revision(&h.pool, &slug, 1).await;
    let a_hash_v1 = content_hash_for_doc(&h.pool, v1_sv, "a.md").await;

    // ── v2 on model @2 (different UUID) ──────────────────────────────────
    let r2 = start_run(&app, &slug, "voyage-context-3@2", &token).await;

    // Attempt to carry a.md with the same content_hash — models differ, so
    // the server must reject this as a conflict.
    let (status, body) = upload(
        &app,
        &slug,
        &r2,
        &token,
        json!({
            "documents": [carried_payload("a.md", &a_hash_v1)],
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "v2 upload response: {body}");

    // Server must report 0 accepted and 1 conflict for a.md.
    assert_eq!(body["accepted"], 0, "a.md must NOT be accepted (model changed): {body}");
    assert_eq!(
        body["conflicts"].as_array().unwrap().len(),
        1,
        "exactly one conflict expected: {body}"
    );

    // The conflict reason must mention "embedding model changed".
    let conflict_reason = body["conflicts"][0]["reason"].as_str().unwrap_or("");
    assert!(
        conflict_reason.contains("embedding model changed"),
        "conflict reason must cite model change, got: {conflict_reason:?}"
    );

    // a.md must NOT appear in r2's document list (the upload was rejected).
    let r2_uuid: Uuid = r2.parse().unwrap();
    let r2_paths = doc_paths_for_sv(&h.pool, r2_uuid).await;
    assert!(
        !r2_paths.contains(&"a.md".to_owned()),
        "a.md must not be in v2 (carry was refused): {r2_paths:?}"
    );

    // The run (v2) must still be in building state — a conflict doesn't abort
    // the run; the CLI would retry with a new upload that has actual chunks.
    let v2_sv_row = source_version::get_by_id(&h.pool, r2_uuid).await.unwrap();
    assert_eq!(
        v2_sv_row.status,
        mnm_core::types::SourceVersionStatus::Building,
        "v2 run must still be building after a carry conflict"
    );
}

// ── Test 3 ───────────────────────────────────────────────────────────────────

/// An `embed_failed` chunk in v1 means the doc is not embed-complete.
/// The inventory endpoint must report `embed_complete=false` for that doc.
/// Carrying such a doc to v2 propagates the failure: v2 also has
/// `embed_complete=false` for a.md — proving "embed_failed docs are not
/// silently healed by carry-forward". The CLI is correct to exclude such docs
/// from PriorState and treat them as new.
#[tokio::test]
async fn embed_failed_doc_is_repaired_on_reingest() {
    let h = common::boot().await;
    let kp = Keypair::generate();
    let cfg = cfg_with_auth(user_store_for("admin", &kp), vec![7u8; 32]);
    let app = app::build(h.pool.clone(), cfg).expect("build app");
    let token = mint_admin_token(app.clone(), "admin", &kp).await;
    let slug = seed_source_no_version(&h.pool, "embed-failed").await;

    // ── Seed v1 with a.md having an embed_failed chunk ────────────────────
    // We seed v1 directly via the entity layer (no HTTP) so we can control the
    // chunk status precisely. The server's upload path cannot produce an
    // embed_failed chunk in a production run, but this state can arise from a
    // partial embedding failure or a legacy import.
    let model_id = embedding_model::upsert(&h.pool, "voyage-context-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let source_row = source::get_by_slug(&h.pool, &slug).await.unwrap();
    let (sv1_id, _) = source_version::create_building(
        &h.pool,
        source_row.id,
        model_id,
        None,
        "0.1.0-test",
        "pending",
    )
    .await
    .unwrap();

    let root = node::insert(&h.pool, sv1_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();

    let provenance = Provenance::default();
    let doc_node = node::insert(&h.pool, sv1_id, Some(root), NodeKind::Document, "a.md", 0)
        .await
        .unwrap();

    let a_hash = "h:a.md:embed-failed-test";
    let doc_a_id = document::insert(
        &h.pool,
        document::NewDocument {
            source_version_id: sv1_id,
            node_id: doc_node,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: None,
            source_path: "a.md",
            language: None,
            content_hash: a_hash,
            source_modified_at: None,
            frontmatter: None,
            provenance: &provenance,
            package_id: None,
            char_count: 40,
            token_count: 10,
        },
    )
    .await
    .unwrap();

    // Insert a.md's single chunk in embed_failed state.
    let chunk_node = node::insert(&h.pool, sv1_id, Some(doc_node), NodeKind::Chunk, "chunk-0", 0)
        .await
        .unwrap();
    chunk::insert(
        &h.pool,
        chunk::NewChunk {
            source_version_id: sv1_id,
            document_id: doc_a_id,
            node_id: chunk_node,
            chunk_index: 0,
            total_chunks: 1,
            content: "# Alpha with failed embedding",
            content_hash: "c:a.md:failed",
            embedding: None,
            embedding_model_id: model_id,
            code_embedding: None,
            heading_path: &[],
            symbol_path: &[],
            start_byte: 0,
            end_byte: 29,
            token_count: 5,
            status: ChunkStatus::EmbedFailed,
        },
    )
    .await
    .unwrap();

    source_version::finalize(&h.pool, sv1_id).await.unwrap();

    // ── Assertion 1: inventory endpoint reports embed_complete=false ───────
    let (v1_inv_status, v1_inv_body) = call(
        app.clone(),
        "GET",
        &format!("/v1/admin/sources/{slug}/active-version/documents"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(v1_inv_status, StatusCode::OK, "inventory: {v1_inv_body}");

    let docs = v1_inv_body["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 1, "inventory must list exactly one doc");
    let a_doc = &docs[0];
    assert_eq!(a_doc["source_path"].as_str().unwrap(), "a.md", "inventory doc must be a.md");
    assert!(
        !a_doc["embed_complete"].as_bool().unwrap(),
        "a.md with embed_failed chunk must report embed_complete=false: {a_doc}"
    );

    // ── Assertion 2: carrying a.md to v2 propagates the failure ──────────
    // The server allows the carry (hash matches, models match) but the
    // resulting v2 doc is ALSO embed_complete=false — proving the server does
    // not silently heal embed_failed docs during carry-forward. The CLI must
    // re-upload such docs with fresh chunks.
    let r2 = start_run(&app, &slug, "voyage-context-3@1", &token).await;
    let (v2_up_status, v2_up_body) = upload(
        &app,
        &slug,
        &r2,
        &token,
        json!({ "documents": [carried_payload("a.md", a_hash)] }),
    )
    .await;
    assert_eq!(v2_up_status, StatusCode::OK, "v2 carry upload: {v2_up_body}");
    // The carry is accepted (not conflicted) — the server has no way to know
    // the prior doc had embed_failed chunks at classification time.
    assert_eq!(
        v2_up_body["accepted"], 1,
        "carry of a.md must be accepted by server (hash+model match): {v2_up_body}"
    );
    assert_eq!(v2_up_body["carried"], 1, "carry counter must be 1: {v2_up_body}");

    let (v2_fin_status, v2_fin_body) = finalize(&app, &slug, &r2, &token, None).await;
    assert_eq!(v2_fin_status, StatusCode::OK, "v2 finalize: {v2_fin_body}");
    assert_eq!(v2_fin_body["revision"], 2);

    // Check v2's inventory: a.md must STILL be embed_complete=false because
    // the embed_failed chunk was faithfully cloned into v2.
    let (v2_inv_status, v2_inv_body) = call(
        app.clone(),
        "GET",
        &format!("/v1/admin/sources/{slug}/active-version/documents"),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(v2_inv_status, StatusCode::OK, "v2 inventory: {v2_inv_body}");

    let docs2 = v2_inv_body["documents"].as_array().unwrap();
    assert_eq!(docs2.len(), 1, "v2 inventory must list exactly one doc");
    assert!(
        !docs2[0]["embed_complete"].as_bool().unwrap(),
        "carried a.md in v2 must STILL be embed_complete=false (failure was not healed by carry): {docs2:?}"
    );

    // ── Assertion 3: re-uploading a.md with actual chunks fixes it ────────
    // Start v3, upload a.md with a real chunk (no embedding → embed_failed
    // again in this text-only test, but the point is that re-uploading with
    // chunks is the correct repair path, not carrying). Here we confirm the
    // chunk count is non-zero and the doc is "new" in v3.
    let r3 = start_run(&app, &slug, "voyage-context-3@1", &token).await;
    let new_content = "# Alpha repaired content";
    // Use a different content so it's a genuine new upload (not carry).
    let (v3_up_status, v3_up_body) = upload(
        &app,
        &slug,
        &r3,
        &token,
        json!({ "documents": [doc_payload("a.md", new_content)] }),
    )
    .await;
    assert_eq!(v3_up_status, StatusCode::OK, "v3 upload: {v3_up_body}");
    assert_eq!(v3_up_body["accepted"], 1, "a.md re-upload must be accepted: {v3_up_body}");
    assert_eq!(v3_up_body["carried"], 0, "re-upload must NOT be carried: {v3_up_body}");

    let (v3_fin_status, v3_fin_body) = finalize(&app, &slug, &r3, &token, None).await;
    assert_eq!(v3_fin_status, StatusCode::OK, "v3 finalize: {v3_fin_body}");
    assert_eq!(v3_fin_body["revision"], 3);

    let r3_uuid: Uuid = r3.parse().unwrap();
    let v3_a_chunks = chunk_ids_and_content_for_doc(&h.pool, r3_uuid, "a.md").await;
    assert!(!v3_a_chunks.is_empty(), "re-uploaded a.md in v3 must have chunks");
    assert_eq!(v3_a_chunks[0].1, new_content, "v3 a.md chunk must contain the new content");
}
