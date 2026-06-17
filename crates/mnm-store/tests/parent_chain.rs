//! US1 acceptance #2: chunks carry a `parent_chain` walking from immediate
//! parent up to the source root. Implemented as a recursive CTE in
//! `mnm_store::entities::node::parent_chain`.

#![cfg(feature = "integration")]
#![allow(clippy::too_many_lines, clippy::doc_markdown)]

mod common;

use mnm_core::types::{NodeKind, SourceKind};
use mnm_store::entities::{embedding_model, node, source, source_version};
use uuid::Uuid;

#[tokio::test]
async fn parent_chain_walks_to_root() {
    let h = common::boot().await;

    let model_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("parent-chain-test-{}", Uuid::new_v4());
    let source_id =
        source::insert(&h.pool, &slug, "Parent Chain Test", SourceKind::DocsSite, None, 5)
            .await
            .unwrap();
    let (sv_id, _) =
        source_version::create_building(&h.pool, source_id, model_id, None, "0.1.0", "h")
            .await
            .unwrap();

    // Build a 4-level hierarchy: root -> group("docs") -> group("getting-started") -> document("quickstart.md")
    let root = node::insert(&h.pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let docs_group = node::insert(&h.pool, sv_id, Some(root), NodeKind::Group, "docs", 0)
        .await
        .unwrap();
    let getting_started =
        node::insert(&h.pool, sv_id, Some(docs_group), NodeKind::Group, "getting-started", 0)
            .await
            .unwrap();
    let doc =
        node::insert(&h.pool, sv_id, Some(getting_started), NodeKind::Document, "quickstart.md", 0)
            .await
            .unwrap();
    let chunk = node::insert(&h.pool, sv_id, Some(doc), NodeKind::Chunk, "c1", 0)
        .await
        .unwrap();

    let chain = node::parent_chain(&h.pool, chunk).await.unwrap();
    assert_eq!(chain.len(), 4, "chain should have 4 ancestors");
    assert_eq!(chain[0].name, "quickstart.md");
    assert_eq!(chain[0].kind, NodeKind::Document);
    assert_eq!(chain[1].name, "getting-started");
    assert_eq!(chain[2].name, "docs");
    assert_eq!(chain[3].name, "root");
    assert_eq!(chain[3].kind, NodeKind::Root);
    assert!(chain[3].parent_node_id.is_none());
}

#[tokio::test]
async fn parent_chain_of_root_is_empty() {
    let h = common::boot().await;

    let model_id = embedding_model::upsert(&h.pool, "bge-base-en-v1.5", 1, 768, "baai")
        .await
        .unwrap();
    let slug = format!("parent-chain-test-2-{}", Uuid::new_v4());
    let source_id =
        source::insert(&h.pool, &slug, "Parent Chain Test 2", SourceKind::DocsSite, None, 5)
            .await
            .unwrap();
    let (sv_id, _) =
        source_version::create_building(&h.pool, source_id, model_id, None, "0.1.0", "h")
            .await
            .unwrap();

    let root = node::insert(&h.pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();
    let chain = node::parent_chain(&h.pool, root).await.unwrap();
    assert!(chain.is_empty(), "root has no ancestors above it");
}
