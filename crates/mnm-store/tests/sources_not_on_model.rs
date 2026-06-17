//! Integration test: `source::list_active_not_on_model` returns sources whose
//! active version is not yet on the target embedding model, ordered by best
//! (lowest-rank) document provenance attribution then slug.

#![cfg(feature = "integration")]
#![allow(missing_docs, clippy::too_many_lines, clippy::similar_names)]

mod common;

use mnm_core::provenance::{Attribution, Provenance};
use mnm_core::types::{DocumentKind, NodeKind, SourceKind};
use mnm_store::entities::{document, embedding_model, node, source, source_version};
use uuid::Uuid;

#[tokio::test]
async fn lists_sources_whose_active_version_is_not_on_target_ordered_by_provenance() {
    let h = common::boot().await;

    // "Old" model — what sources A and B are currently on.
    let old_model_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .expect("seed old model");

    // "Target" model — voyage-code-3 registered by migration 0008.
    let target_model = embedding_model::get_by_name_revision(&h.pool, "voyage-code-3", 1)
        .await
        .expect("voyage-code-3@1 must exist after migrations");

    // Source A: active version on old model, document attribution = "partner".
    let slug_a = format!("src-a-{}", Uuid::new_v4());
    let source_a_id = source::insert(&h.pool, &slug_a, "Source A", SourceKind::DocsSite, None, 5)
        .await
        .expect("insert source A");
    let (sv_a_id, _) = source_version::create_building(
        &h.pool,
        source_a_id,
        old_model_id,
        None,
        "0.1.0",
        "hash-a",
    )
    .await
    .expect("create_building source A");
    let root_a = node::insert(&h.pool, sv_a_id, None, NodeKind::Root, "root", 0)
        .await
        .expect("root node A");
    let doc_node_a =
        node::insert(&h.pool, sv_a_id, Some(root_a), NodeKind::Document, "doc-a.md", 0)
            .await
            .expect("doc node A");
    document::insert(
        &h.pool,
        document::NewDocument {
            source_version_id: sv_a_id,
            node_id: doc_node_a,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: None,
            source_path: "doc-a.md",
            language: Some("en"),
            content_hash: &format!("h-a-{}", Uuid::new_v4()),
            source_modified_at: None,
            frontmatter: None,
            provenance: &Provenance::attributed_to(Attribution::Partner),
            package_id: None,
            char_count: 100,
            token_count: 20,
        },
    )
    .await
    .expect("insert doc A");
    source_version::finalize(&h.pool, sv_a_id)
        .await
        .expect("finalize source A version");

    // Source B: active version on old model, document attribution = "foundation".
    let slug_b = format!("src-b-{}", Uuid::new_v4());
    let source_b_id = source::insert(&h.pool, &slug_b, "Source B", SourceKind::DocsSite, None, 5)
        .await
        .expect("insert source B");
    let (sv_b_id, _) = source_version::create_building(
        &h.pool,
        source_b_id,
        old_model_id,
        None,
        "0.1.0",
        "hash-b",
    )
    .await
    .expect("create_building source B");
    let root_b = node::insert(&h.pool, sv_b_id, None, NodeKind::Root, "root", 0)
        .await
        .expect("root node B");
    let doc_node_b =
        node::insert(&h.pool, sv_b_id, Some(root_b), NodeKind::Document, "doc-b.md", 0)
            .await
            .expect("doc node B");
    document::insert(
        &h.pool,
        document::NewDocument {
            source_version_id: sv_b_id,
            node_id: doc_node_b,
            kind: DocumentKind::Markdown,
            source_url: None,
            published_url: None,
            source_path: "doc-b.md",
            language: Some("en"),
            content_hash: &format!("h-b-{}", Uuid::new_v4()),
            source_modified_at: None,
            frontmatter: None,
            provenance: &Provenance::attributed_to(Attribution::Foundation),
            package_id: None,
            char_count: 80,
            token_count: 15,
        },
    )
    .await
    .expect("insert doc B");
    source_version::finalize(&h.pool, sv_b_id)
        .await
        .expect("finalize source B version");

    // Source C: active version already on the target model — must be excluded.
    let slug_c = format!("src-c-{}", Uuid::new_v4());
    let source_c_id = source::insert(&h.pool, &slug_c, "Source C", SourceKind::DocsSite, None, 5)
        .await
        .expect("insert source C");
    let (sv_c_id, _) = source_version::create_building(
        &h.pool,
        source_c_id,
        target_model.id,
        None,
        "0.1.0",
        "hash-c",
    )
    .await
    .expect("create_building source C");
    source_version::finalize(&h.pool, sv_c_id)
        .await
        .expect("finalize source C version");

    // Execute the query.
    let results = source::list_active_not_on_model(&h.pool, target_model.id)
        .await
        .expect("list_active_not_on_model");

    // Filter results to only the sources we seeded in this test run (the DB may
    // have rows from other parallel test schemas, but within our isolated schema
    // only A, B, C exist — C must be absent).
    let ids: Vec<Uuid> = results.iter().map(|s| s.id).collect();

    assert!(ids.contains(&source_b_id), "source B (foundation) must be in results");
    assert!(ids.contains(&source_a_id), "source A (partner) must be in results");
    assert!(
        !ids.contains(&source_c_id),
        "source C (already on target model) must NOT be in results"
    );

    // Within the results, source B (foundation, rank 1) must precede source A
    // (partner, rank 2).
    let pos_a = ids.iter().position(|&id| id == source_a_id).unwrap();
    let pos_b = ids.iter().position(|&id| id == source_b_id).unwrap();
    assert!(
        pos_b < pos_a,
        "source B (foundation) must come before source A (partner); got pos_b={pos_b} pos_a={pos_a}"
    );
}
