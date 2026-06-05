//! Integration tests for `GET /v1/facets`.
#![cfg(feature = "integration")]
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use mn_core::provenance::Provenance;
use mn_core::types::{DocumentKind, NodeKind, SourceKind};
use mn_server::{app, config::ServerConfig};
use mn_store::entities::{document, embedding_model, node, source, source_version};
use tower::ServiceExt as _;
use uuid::Uuid;

fn cfg() -> ServerConfig {
    ServerConfig::default()
}

/// Oneshot `GET /v1/facets` -> parsed JSON body.
async fn get_facets_body(app: Router) -> serde_json::Value {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/facets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Seed one finalized (active) `source_version` with one document per supplied
/// `(language, provenance)` pair, all hung off a single root + group structure.
async fn seed_documents(pool: &sqlx::PgPool, docs: &[(&str, Provenance)]) {
    let model_id = embedding_model::upsert(pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let slug = format!("facets-route-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Facets", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, "0.1.0", "h")
        .await
        .unwrap();
    let root = node::insert(pool, sv_id, None, NodeKind::Root, "root", 0)
        .await
        .unwrap();

    for (i, (language, provenance)) in docs.iter().enumerate() {
        let order = i32::try_from(i).unwrap();
        let name = format!("doc{i}.md");
        let doc_node = node::insert(pool, sv_id, Some(root), NodeKind::Document, &name, order)
            .await
            .unwrap();
        document::insert(
            pool,
            document::NewDocument {
                source_version_id: sv_id,
                node_id: doc_node,
                kind: DocumentKind::Markdown,
                source_url: None,
                published_url: None,
                source_path: &name,
                language: Some(language),
                content_hash: "h",
                source_modified_at: None,
                frontmatter: None,
                provenance,
                package_id: None,
                char_count: 0,
                token_count: 0,
            },
        )
        .await
        .unwrap();
    }

    // REQUIRED: flips is_active=true, which the corpus_values queries filter on.
    source_version::finalize(pool, sv_id).await.unwrap();
}

#[tokio::test]
async fn facets_lists_modes_and_closed_enums() {
    let h = common::boot().await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");
    let body = get_facets_body(app).await;
    assert_eq!(body["modes"], serde_json::json!(["hybrid", "vector", "fts"]));
    let kind = body["filters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "kind")
        .expect("kind facet");
    assert_eq!(kind["type"], "enum");
    assert_eq!(kind["values"], serde_json::json!(["markdown", "code", "plaintext"]));
}

#[tokio::test]
async fn language_values_include_seeded() {
    let h = common::boot().await;
    seed_documents(
        &h.pool,
        &[
            ("compact", Provenance::default()),
            ("rust", Provenance::default()),
        ],
    )
    .await;
    // Build the app AFTER seeding so its fresh per-app cache is populated from
    // the seeded corpus on the first request.
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");
    let body = get_facets_body(app).await;
    let lang = body["filters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "language")
        .expect("language facet");
    let vals = lang["values"].as_array().expect("language has values");
    let set: std::collections::HashSet<&str> = vals.iter().filter_map(|v| v.as_str()).collect();
    assert!(set.contains("compact") && set.contains("rust"), "got {vals:?}");
    assert_eq!(lang["truncated"], serde_json::json!(false));
}

#[tokio::test]
async fn tags_truncated_when_over_cap() {
    let h = common::boot().await;
    // Seed one finalized document with 205 distinct tags (> VALUE_CAP = 200).
    let tags: Vec<String> = (0..205).map(|i| format!("tag{i:03}")).collect();
    let provenance = Provenance {
        tags: tags.clone(),
        ..Default::default()
    };
    seed_documents(&h.pool, &[("compact", provenance)]).await;
    // Build the app AFTER seeding so its fresh per-app cache reflects the corpus.
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");
    let body = get_facets_body(app).await;
    let facet = body["filters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "tags")
        .expect("tags facet");
    assert_eq!(facet["truncated"], serde_json::json!(true), "got {facet}");
    // total is capped at VALUE_CAP + 1 = 201 ("201 or more"); values capped at 200.
    assert_eq!(facet["total"], serde_json::json!(201));
    assert_eq!(facet["values"].as_array().unwrap().len(), 200);
}

#[tokio::test]
async fn symbol_and_heading_path_advertise_type_only() {
    let h = common::boot().await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");
    let body = get_facets_body(app).await;
    let filters = body["filters"].as_array().unwrap();
    let symbol = filters
        .iter()
        .find(|f| f["key"] == "symbol")
        .expect("symbol facet");
    assert_eq!(symbol["type"], "object_set");
    assert!(symbol.get("values").is_none(), "symbol must not enumerate values: {symbol}");
    let heading = filters
        .iter()
        .find(|f| f["key"] == "heading_path")
        .expect("heading_path facet");
    assert_eq!(heading["type"], "open_set");
    assert!(
        heading.get("values").is_none(),
        "heading_path must not enumerate values: {heading}"
    );
}

#[tokio::test]
async fn tags_small_fixture_not_truncated() {
    let h = common::boot().await;
    let provenance = Provenance {
        tags: vec!["quickstart".into(), "intro".into()],
        ..Default::default()
    };
    seed_documents(&h.pool, &[("compact", provenance)]).await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");
    let body = get_facets_body(app).await;
    let tags = body["filters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "tags")
        .expect("tags facet");
    assert_eq!(tags["truncated"], serde_json::json!(false));
    let vals = tags["values"].as_array().expect("tags has values");
    let set: std::collections::HashSet<&str> = vals.iter().filter_map(|v| v.as_str()).collect();
    assert!(set.contains("quickstart") && set.contains("intro"), "got {vals:?}");
}
