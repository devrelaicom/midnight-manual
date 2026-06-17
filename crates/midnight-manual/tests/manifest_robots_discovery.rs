//! Integration tests for robots.txt sitemap auto-discovery in
//! `mnm manifest generate`.

use midnight_manual::commands::manifest::generate::load_sitemaps;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const URLSET_BODY: &str = r#"<?xml version="1.0"?>
<urlset>
  <url><loc>https://docs.example.com/a/</loc></url>
  <url><loc>https://docs.example.com/b/</loc></url>
</urlset>"#;

#[tokio::test]
async fn discovers_sitemap_from_robots_when_given_bare_site_url() {
    let server = MockServer::start().await;
    let site = server.uri();
    let robots_body = format!("User-agent: *\nSitemap: {site}/sitemap.xml\n");

    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(robots_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sitemap.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(URLSET_BODY))
        .mount(&server)
        .await;

    let urls = load_sitemaps(std::slice::from_ref(&site))
        .await
        .expect("load_sitemaps");
    assert_eq!(urls.len(), 2, "expected two URLs from discovered sitemap, got {urls:?}");
    assert!(urls
        .iter()
        .any(|u| u.as_str() == "https://docs.example.com/a/"));
    assert!(urls
        .iter()
        .any(|u| u.as_str() == "https://docs.example.com/b/"));
}

#[tokio::test]
async fn aggregates_multiple_sitemaps_from_robots() {
    let server = MockServer::start().await;
    let site = server.uri();
    let robots_body = format!("Sitemap: {site}/sitemap-a.xml\nSitemap: {site}/sitemap-b.xml\n");
    let body_a = r"<urlset><url><loc>https://docs.example.com/a/</loc></url></urlset>";
    let body_b = r"<urlset><url><loc>https://docs.example.com/b/</loc></url></urlset>";

    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string(robots_body))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sitemap-a.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body_a))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sitemap-b.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body_b))
        .mount(&server)
        .await;

    let urls = load_sitemaps(&[site]).await.expect("load_sitemaps");
    assert_eq!(urls.len(), 2);
    assert!(urls
        .iter()
        .any(|u| u.as_str() == "https://docs.example.com/a/"));
    assert!(urls
        .iter()
        .any(|u| u.as_str() == "https://docs.example.com/b/"));
}

#[tokio::test]
async fn falls_through_to_user_supplied_url_when_robots_is_404() {
    let server = MockServer::start().await;
    let site = server.uri();

    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    // The user explicitly passed the site root — when robots.txt is absent
    // we should still try to load it as a sitemap (and get back nothing
    // useful, but it must not error).
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r"<urlset></urlset>"))
        .mount(&server)
        .await;

    let urls = load_sitemaps(&[site])
        .await
        .expect("must not error on 404 robots");
    assert!(urls.is_empty());
}

#[tokio::test]
async fn skips_robots_discovery_when_spec_looks_like_a_sitemap() {
    let server = MockServer::start().await;
    let site = server.uri();
    let sitemap_url = format!("{site}/sitemap.xml");

    // The robots.txt mock asserts a request — `expect(0)` would make this
    // explicit, but wiremock-rs uses a different shape. We instead mount a
    // robots.txt response containing a different sitemap; if it gets hit,
    // we'd see those URLs in the result.
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(format!("Sitemap: {site}/should-not-be-fetched.xml\n")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sitemap.xml"))
        .respond_with(ResponseTemplate::new(200).set_body_string(URLSET_BODY))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/should-not-be-fetched.xml"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let urls = load_sitemaps(&[sitemap_url]).await.expect("load_sitemaps");
    // If we'd consulted robots.txt we'd have hit the 500 stub and errored;
    // success here proves we skipped it.
    assert_eq!(urls.len(), 2);
}

#[tokio::test]
async fn robots_with_no_sitemap_directive_falls_through() {
    let server = MockServer::start().await;
    let site = server.uri();

    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("User-agent: *\nDisallow: /private/\n"),
        )
        .mount(&server)
        .await;
    // Bare-site fall-through: user gave us the origin, we treat it as a
    // (degenerate) sitemap URL and request it.
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r"<urlset></urlset>"))
        .mount(&server)
        .await;

    let urls = load_sitemaps(&[site]).await.expect("load_sitemaps");
    assert!(urls.is_empty());
}
