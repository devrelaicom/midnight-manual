//! Parse `<urlset>` and `<sitemapindex>` XML into a flat list of URLs.
//!
//! Implements the sitemap-URL parsing described in §1.2 of the ingest UX design.

use std::path::Path;
use thiserror::Error;
use url::Url;

/// Errors that can occur when parsing a sitemap XML document.
#[derive(Debug, Error)]
pub enum SitemapError {
    /// Failed to parse XML body.
    #[error("invalid sitemap XML: {0}")]
    Parse(String),
    /// A `<loc>` element contained an invalid URL.
    #[error("invalid URL in sitemap: {0}")]
    BadUrl(String),
}

/// Parse a sitemap body. Returns the URLs from `<urlset><url><loc>...`,
/// or the index `<sitemapindex><sitemap><loc>...` entries (callers fetch
/// those recursively).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// A `<urlset>` with a list of content URLs.
    Urls(Vec<Url>),
    /// A `<sitemapindex>` with a list of sitemap URLs (fetch these recursively).
    Index(Vec<Url>),
}

/// Parse a sitemap or sitemap index XML document.
///
/// # Errors
///
/// Returns [`SitemapError::Parse`] if the body is not valid XML,
/// or [`SitemapError::BadUrl`] if a `<loc>` element contains an invalid URL.
pub fn parse(body: &str) -> Result<Parsed, SitemapError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(body);
    reader.trim_text(true);

    let mut urls: Vec<Url> = Vec::new();
    let mut is_index = false;
    let mut in_loc = false;
    let mut buf = Vec::new();
    let mut loc_buf = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().0.to_ascii_lowercase();
                if name == b"sitemapindex" {
                    is_index = true;
                }
                if name == b"loc" {
                    in_loc = true;
                    loc_buf.clear();
                }
            }
            Ok(Event::Text(t)) if in_loc => {
                loc_buf.push_str(
                    &t.unescape()
                        .map_err(|e| SitemapError::Parse(e.to_string()))?,
                );
            }
            Ok(Event::End(e)) if e.name().0.eq_ignore_ascii_case(b"loc") => {
                in_loc = false;
                let parsed = Url::parse(loc_buf.trim())
                    .map_err(|_| SitemapError::BadUrl(loc_buf.trim().to_owned()))?;
                urls.push(parsed);
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(SitemapError::Parse(e.to_string())),
            _ => {}
        }
        buf.clear();
    }
    Ok(if is_index {
        Parsed::Index(urls)
    } else {
        Parsed::Urls(urls)
    })
}

/// Check if a spec string is an HTTP(S) URL.
#[must_use]
pub fn is_http(spec: &str) -> bool {
    spec.starts_with("http://") || spec.starts_with("https://")
}

/// Load a sitemap from a file path.
///
/// # Errors
///
/// Returns an error if the file cannot be read or the XML is invalid.
pub fn load_from_path(path: &Path) -> Result<Vec<Url>, SitemapError> {
    let body = std::fs::read_to_string(path)
        .map_err(|e| SitemapError::Parse(format!("read {}: {e}", path.display())))?;
    match parse(&body)? {
        Parsed::Urls(v) => Ok(v),
        Parsed::Index(_) => Ok(Vec::new()), // file-form indexes are uncommon; ignore
    }
}

/// Load a sitemap from an HTTP(S) URL, recursing one level into `<sitemapindex>`.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the XML is invalid.
pub async fn load_from_url(client: &reqwest::Client, url: &Url) -> Result<Vec<Url>, SitemapError> {
    let body = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| SitemapError::Parse(e.to_string()))?
        .text()
        .await
        .map_err(|e| SitemapError::Parse(e.to_string()))?;
    match parse(&body)? {
        Parsed::Urls(v) => Ok(v),
        Parsed::Index(children) => {
            let mut all = Vec::new();
            for child in children {
                let body = client
                    .get(child)
                    .send()
                    .await
                    .map_err(|e| SitemapError::Parse(e.to_string()))?
                    .text()
                    .await
                    .map_err(|e| SitemapError::Parse(e.to_string()))?;
                if let Ok(Parsed::Urls(v)) = parse(&body) {
                    all.extend(v);
                }
            }
            Ok(all)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urlset() {
        let body = r#"<?xml version="1.0"?>
<urlset>
  <url><loc>https://docs.example.com/a/</loc></url>
  <url><loc>https://docs.example.com/b/</loc></url>
</urlset>"#;
        match parse(body).unwrap() {
            Parsed::Urls(v) => assert_eq!(v.len(), 2),
            Parsed::Index(_) => panic!("expected urlset"),
        }
    }

    #[test]
    fn parses_sitemap_index() {
        let body = r#"<?xml version="1.0"?>
<sitemapindex>
  <sitemap><loc>https://docs.example.com/sitemap-1.xml</loc></sitemap>
</sitemapindex>"#;
        assert!(matches!(parse(body).unwrap(), Parsed::Index(v) if v.len() == 1));
    }

    #[test]
    fn load_from_file_returns_urls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sitemap.xml");
        std::fs::write(&path, r"<urlset><url><loc>https://example.com/x/</loc></url></urlset>")
            .unwrap();
        let urls = load_from_path(&path).unwrap();
        assert_eq!(urls.len(), 1);
    }
}
