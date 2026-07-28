//! Integration tests for `GET /v1/facets`.
#![cfg(feature = "integration")]
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use midnight_manual_server::{app, config::ServerConfig};
use mnm_core::provenance::{Attribution, LanguageTarget, Provenance, SdkDependency};
use mnm_core::types::{DocumentKind, NodeKind, PackageKind, SourceKind};
use mnm_store::entities::{document, embedding_model, node, package, source, source_version};
use tower::ServiceExt as _;
use uuid::Uuid;

fn cfg() -> ServerConfig {
    ServerConfig::default()
}

/// Oneshot `GET <uri>` -> `(status, parsed JSON body)`.
async fn get(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// Oneshot `GET /v1/facets` -> parsed JSON body (asserts 200).
async fn get_facets_body(app: Router) -> serde_json::Value {
    let (status, body) = get(app, "/v1/facets").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Seed one finalized (active) `source_version` with one document per supplied
/// `(language, provenance)` pair, all hung off a single root + group structure.
/// The source is a `DocsSite`; use [`seed_documents_kind`] to control the kind.
async fn seed_documents(pool: &sqlx::PgPool, docs: &[(&str, Provenance)]) {
    seed_documents_kind(pool, SourceKind::DocsSite, docs).await;
}

/// Like [`seed_documents`] but with an explicit source `kind`, so tests can
/// build a corpus of multiple sources with differing kinds + attributions.
async fn seed_documents_kind(pool: &sqlx::PgPool, kind: SourceKind, docs: &[(&str, Provenance)]) {
    let model_id = embedding_model::upsert(pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let slug = format!("facets-route-test-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Facets", kind, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, None, "0.1.0", "h")
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
                license: None,
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
async fn tags_overview_samples_and_drilldown_paginates() {
    let h = common::boot().await;
    // Seed one finalized document with 15 distinct tags (> SAMPLE_CAP = 10).
    let tags: Vec<String> = (0..15).map(|i| format!("tag{i:02}")).collect();
    let provenance = Provenance {
        tags: tags.clone(),
        ..Default::default()
    };
    seed_documents(&h.pool, &[("compact", provenance)]).await;
    // Build the app AFTER seeding so its fresh per-app cache reflects the corpus.
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    // Overview: 10-value sample, exact total, truncated flag. This also
    // populates the overview cache BEFORE the drill-down calls below, proving
    // a drill-down request is never answered from the cached overview body.
    let body = get_facets_body(app.clone()).await;
    let facet = body["filters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "tags")
        .expect("tags facet");
    assert_eq!(facet["truncated"], serde_json::json!(true), "got {facet}");
    assert_eq!(facet["total"], serde_json::json!(15), "got {facet}");
    assert_eq!(facet["values"].as_array().unwrap().len(), 10, "got {facet}");

    // Drill-down page 1: first 10 values in text order + a resume cursor.
    let (status, page1) = get(app.clone(), "/v1/facets?facet=tags&limit=10").await;
    assert_eq!(status, StatusCode::OK, "{page1}");
    assert_eq!(page1["facet"], "tags", "{page1}");
    assert_eq!(page1["total"], 15, "{page1}");
    assert_eq!(page1["values"], serde_json::json!(&tags[..10]), "{page1}");
    let cursor = page1["next_cursor"]
        .as_str()
        .expect("page 1 has next_cursor");

    // Drill-down page 2: the remaining 5, no further pages.
    let (status, page2) =
        get(app.clone(), &format!("/v1/facets?facet=tags&limit=10&cursor={cursor}")).await;
    assert_eq!(status, StatusCode::OK, "{page2}");
    assert_eq!(page2["total"], 15, "{page2}");
    assert_eq!(page2["values"], serde_json::json!(&tags[10..]), "{page2}");
    assert!(page2["next_cursor"].is_null(), "{page2}");
}

#[tokio::test]
async fn drilldown_invalid_params_return_typed_400() {
    let h = common::boot().await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    for uri in [
        "/v1/facets?facet=kind",  // closed enum: not drillable
        "/v1/facets?facet=bogus", // unknown facet
        "/v1/facets?facet=tags&limit=0",
        "/v1/facets?facet=tags&limit=201",
        "/v1/facets?facet=tags&cursor=!!!not-base64!!!",
    ] {
        let (status, v) = get(app.clone(), uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} → {v}");
        assert_eq!(v["error"]["code"], "invalid_request", "{uri} → {v}");
        assert!(v["error"]["remediation"].is_string(), "{uri} → {v}");
    }
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

/// Seed one finalized (active) `source_version` carrying a single package row
/// with the given name + version, so the `package` version drill has data.
async fn seed_package(pool: &sqlx::PgPool, name: &str, version: &str) {
    let model_id = embedding_model::upsert(pool, "voyage-code-3", 1, 1024, "voyageai")
        .await
        .unwrap();
    let slug = format!("facets-route-pkg-{}", Uuid::new_v4());
    let source_id = source::insert(pool, &slug, "Facets pkg", SourceKind::DocsSite, None, 5)
        .await
        .unwrap();
    let (sv_id, _) = source_version::create_building(pool, source_id, model_id, None, "0.1.0", "h")
        .await
        .unwrap();
    package::upsert(pool, sv_id, PackageKind::Npm, name, Some(version), None)
        .await
        .unwrap();
    // REQUIRED: flips is_active=true, which the drill queries filter on.
    source_version::finalize(pool, sv_id).await.unwrap();
}

#[tokio::test]
async fn language_target_two_level_drill() {
    let h = common::boot().await;
    let provenance = Provenance {
        language_targets: vec![LanguageTarget {
            name: "compact".into(),
            version_constraint: Some(">=0.23".into()),
        }],
        ..Default::default()
    };
    seed_documents(&h.pool, &[("compact", provenance)]).await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    // Level 1: enumerate the language-target names.
    let (status, names) = get(app.clone(), "/v1/facets?facet=language_target").await;
    assert_eq!(status, StatusCode::OK, "{names}");
    assert_eq!(names["values"], serde_json::json!(["compact"]), "{names}");

    // Level 2: enumerate the version constraints within `compact`.
    let (status, versions) =
        get(app.clone(), "/v1/facets?facet=language_target&within=compact").await;
    assert_eq!(status, StatusCode::OK, "{versions}");
    assert_eq!(versions["values"], serde_json::json!([">=0.23"]), "{versions}");
    assert_eq!(versions["within"], "compact", "{versions}");
}

#[tokio::test]
async fn sdk_dependency_level_one_composite() {
    let h = common::boot().await;
    let provenance = Provenance {
        sdk_dependencies: vec![SdkDependency {
            kind: "npm".into(),
            name: "@midnight-ntwrk/midnight-js".into(),
            version_constraint: Some("^1.4.0".into()),
        }],
        ..Default::default()
    };
    seed_documents(&h.pool, &[("typescript", provenance)]).await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    let (status, names) = get(app.clone(), "/v1/facets?facet=sdk_dependency").await;
    assert_eq!(status, StatusCode::OK, "{names}");
    assert_eq!(
        names["values"],
        serde_json::json!(["npm:@midnight-ntwrk/midnight-js"]),
        "{names}"
    );
}

#[tokio::test]
async fn package_version_drill() {
    let h = common::boot().await;
    seed_package(&h.pool, "@midnight-ntwrk/midnight-js", "1.4.0").await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    let (status, versions) =
        get(app.clone(), "/v1/facets?facet=package&within=@midnight-ntwrk/midnight-js").await;
    assert_eq!(status, StatusCode::OK, "{versions}");
    assert_eq!(versions["values"], serde_json::json!(["1.4.0"]), "{versions}");
    assert_eq!(versions["within"], "@midnight-ntwrk/midnight-js", "{versions}");
}

#[tokio::test]
async fn version_drill_invalid_params_return_typed_400() {
    let h = common::boot().await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    // `verified` is a bool facet: not drillable at all.
    let (status, v) = get(app.clone(), "/v1/facets?facet=verified").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], "invalid_request", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not drillable"),
        "{v}"
    );

    // `language` is drillable at level 1 but has no `within` second level.
    let (status, v) = get(app.clone(), "/v1/facets?facet=language&within=x").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(v["error"]["code"], "invalid_request", "{v}");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has no `within` drill level"),
        "{v}"
    );
}

/// Issue #139: the NO-ARG overview carries a compact `corpus` cold-start block
/// derived from the seeded corpus; a drill-down response does NOT.
#[tokio::test]
async fn overview_carries_corpus_block_drilldown_does_not() {
    let h = common::boot().await;
    // Two DISTINCT sources with DIFFERING kinds AND attributions, so `sum ==
    // total` is non-trivial (2, split 1/1) and the mid-rank by_attribution CASE
    // branches (partner=2, community=4) are exercised — not just Foundation.
    //
    // Source A: a docs_site of partner-attributed docs, one carrying a
    // language_target so version_coverage has data.
    let partner_compact = Provenance {
        attribution: Attribution::Partner,
        language_targets: vec![LanguageTarget {
            name: "compact".into(),
            version_constraint: Some(">=0.23".into()),
        }],
        tags: vec!["quickstart".into(), "privacy".into()],
        ..Default::default()
    };
    let partner_ts = Provenance {
        attribution: Attribution::Partner,
        ..Default::default()
    };
    seed_documents_kind(
        &h.pool,
        SourceKind::DocsSite,
        &[("compact", partner_compact), ("typescript", partner_ts)],
    )
    .await;
    // Source B: a code_repo of community-attributed docs.
    let community_rust = Provenance {
        attribution: Attribution::Community,
        ..Default::default()
    };
    seed_documents_kind(&h.pool, SourceKind::CodeRepo, &[("rust", community_rust)]).await;
    let app = app::build_resolved(h.pool.clone(), cfg())
        .await
        .expect("build app");

    // Overview: the corpus block is present and well-formed.
    let body = get_facets_body(app.clone()).await;
    let corpus = &body["corpus"];
    assert!(corpus.is_object(), "no-arg overview must carry a corpus block: {body}");

    // sources: exactly the 2 seeded sources (the DB is isolated per test), and
    // by_kind/by_attribution each sum to total — a non-trivial 1/1 split.
    let total = corpus["sources"]["total"]
        .as_i64()
        .expect("sources.total is int");
    assert_eq!(total, 2, "two sources seeded: {corpus}");
    let sum_map = |m: &serde_json::Value| -> i64 {
        m.as_object()
            .expect("map object")
            .values()
            .filter_map(serde_json::Value::as_i64)
            .sum()
    };
    assert_eq!(sum_map(&corpus["sources"]["by_kind"]), total, "by_kind sums to total: {corpus}");
    assert_eq!(
        sum_map(&corpus["sources"]["by_attribution"]),
        total,
        "by_attribution sums to total: {corpus}"
    );
    // The two differing kinds are each counted once.
    assert_eq!(corpus["sources"]["by_kind"]["docs_site"].as_i64(), Some(1), "{corpus}");
    assert_eq!(corpus["sources"]["by_kind"]["code_repo"].as_i64(), Some(1), "{corpus}");
    // The two differing mid-rank attributions are each counted once (partner=2,
    // community=4 CASE branches, not just Foundation).
    assert_eq!(corpus["sources"]["by_attribution"]["partner"].as_i64(), Some(1), "{corpus}");
    assert_eq!(corpus["sources"]["by_attribution"]["community"].as_i64(), Some(1), "{corpus}");
    assert!(
        corpus["sources"]["by_attribution"]
            .get("foundation")
            .is_none(),
        "no foundation source was seeded: {corpus}"
    );

    // languages: the seeded languages appear.
    let langs: std::collections::HashSet<&str> = corpus["languages"]
        .as_array()
        .expect("languages array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        langs.contains("compact") && langs.contains("typescript") && langs.contains("rust"),
        "{corpus}"
    );

    // version_coverage: the compact target's declared constraint is listed.
    let cov = corpus["version_coverage"]
        .as_array()
        .expect("version_coverage array")
        .iter()
        .find(|e| e["target"] == "compact")
        .expect("compact version coverage");
    assert_eq!(cov["declared_constraints"], serde_json::json!([">=0.23"]), "{corpus}");

    // freshness: both timestamps present (RFC3339 strings) for a populated corpus.
    assert!(corpus["freshness"]["oldest_ingested_at"].is_string(), "{corpus}");
    assert!(corpus["freshness"]["newest_ingested_at"].is_string(), "{corpus}");

    // tags_sample: the seeded tags appear.
    let tags: std::collections::HashSet<&str> = corpus["tags_sample"]
        .as_array()
        .expect("tags_sample array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(tags.contains("quickstart") && tags.contains("privacy"), "{corpus}");

    // Budget: the serialized corpus block stays within 2 KB.
    let bytes = serde_json::to_vec(corpus).unwrap().len();
    assert!(bytes <= 2048, "corpus block must stay ≤2 KB, got {bytes} bytes: {corpus}");

    // Drill-down: the same server, a facet drill, must NOT carry a corpus block.
    let (status, drill) = get(app, "/v1/facets?facet=language").await;
    assert_eq!(status, StatusCode::OK, "{drill}");
    assert!(
        drill.get("corpus").is_none(),
        "drill-down must not carry a corpus block: {drill}"
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
