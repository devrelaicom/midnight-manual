//! Auto-discover sitemap URLs from a site's `/robots.txt`.
//!
//! Per the de-facto Robots Exclusion Protocol convention (RFC 9309 §2.2.2
//! and earlier robotstxt.org guidance), a `robots.txt` may include one or
//! more `Sitemap: <url>` directives that point at the site's XML sitemap
//! (or sitemap index). We parse them out conservatively:
//!
//! - case-insensitive directive match (`Sitemap:`, `sitemap:`, `SITEMAP:`)
//! - one URL per line
//! - lines after `#` are comments and ignored
//! - malformed or blank lines are silently skipped (never panic / never error)
//!
//! Spec: follow-up item 5 in the manifest-ingest UX design.

use url::Url;

/// Parse the body of a `robots.txt` and return every `Sitemap:` URL it
/// declares, in document order, de-duplicated.
///
/// Returns an empty list when no `Sitemap:` directives are present or the
/// body is otherwise unusable. Never errors — `robots.txt` is permissive by
/// convention and we treat any malformed input as "no hint available".
#[must_use]
pub fn parse_sitemap_directives(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw_line in body.lines() {
        // Strip comments (everything from `#` onward) and trim whitespace.
        let line = raw_line
            .find('#')
            .map_or(raw_line, |i| &raw_line[..i])
            .trim();
        if line.is_empty() {
            continue;
        }
        // Split on the first ':' — directive name on the left, value on the right.
        let Some((directive, value)) = line.split_once(':') else {
            continue;
        };
        if !directive.trim().eq_ignore_ascii_case("sitemap") {
            continue;
        }
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        // Guard against parser-confusion: only accept http(s) URLs.
        // robots.txt is meant to carry absolute URLs here.
        if !(value.starts_with("http://") || value.starts_with("https://")) {
            continue;
        }
        if !out.iter().any(|existing| existing == value) {
            out.push(value.to_owned());
        }
    }
    out
}

/// Heuristic: does this spec already look like a sitemap URL/path? If so,
/// we skip robots.txt discovery and use it directly.
///
/// Conservative — we err on the side of "this looks like a sitemap" to
/// avoid an extra HTTP roundtrip when the user has clearly supplied one.
#[must_use]
pub fn looks_like_sitemap_spec(spec: &str) -> bool {
    // Strip a query string / fragment before extension matching so that
    // `https://example.com/foo.xml?x=1` still counts as a sitemap URL.
    let path_only = spec.split_once(['?', '#']).map_or(spec, |(p, _)| p);
    let bytes = path_only.as_bytes();
    let has_xml_ext =
        path_only.len() >= 4 && bytes[bytes.len() - 4..].eq_ignore_ascii_case(b".xml");
    let has_xml_gz_ext =
        path_only.len() >= 7 && bytes[bytes.len() - 7..].eq_ignore_ascii_case(b".xml.gz");
    if has_xml_ext || has_xml_gz_ext {
        return true;
    }
    spec.to_ascii_lowercase().contains("sitemap")
}

/// Build the canonical `<origin>/robots.txt` URL from any URL on the same
/// origin. Returns `None` if the origin is opaque (e.g. `data:` URLs).
#[must_use]
pub fn robots_url_for(base: &Url) -> Option<Url> {
    let scheme = base.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = base.host_str()?;
    let mut s = format!("{scheme}://{host}");
    if let Some(port) = base.port() {
        use std::fmt::Write as _;
        let _ = write!(s, ":{port}");
    }
    s.push_str("/robots.txt");
    Url::parse(&s).ok()
}

/// Fetch `<origin>/robots.txt` and return any `Sitemap:` URLs it declares.
///
/// Returns an empty list (never an error) when the fetch fails, the
/// response is non-2xx, the body isn't valid UTF-8, or no `Sitemap:`
/// lines are present. This is a best-effort hint, not a hard requirement.
pub async fn discover_sitemaps(client: &reqwest::Client, site: &Url) -> Vec<Url> {
    let Some(robots) = robots_url_for(site) else {
        return Vec::new();
    };
    let resp = match client.get(robots).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };
    let Ok(body) = resp.text().await else {
        return Vec::new();
    };
    parse_sitemap_directives(&body)
        .into_iter()
        .filter_map(|s| Url::parse(&s).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_sitemap_line() {
        let body = "User-agent: *\nDisallow:\nSitemap: https://example.com/sitemap.xml\n";
        assert_eq!(
            parse_sitemap_directives(body),
            vec!["https://example.com/sitemap.xml".to_owned()]
        );
    }

    #[test]
    fn parses_multiple_sitemap_lines_in_order() {
        let body = "Sitemap: https://example.com/sitemap-1.xml\n\
                    Sitemap: https://example.com/sitemap-2.xml\n";
        assert_eq!(
            parse_sitemap_directives(body),
            vec![
                "https://example.com/sitemap-1.xml".to_owned(),
                "https://example.com/sitemap-2.xml".to_owned(),
            ]
        );
    }

    #[test]
    fn directive_match_is_case_insensitive() {
        let body = "sitemap: https://example.com/a.xml\n\
                    SITEMAP: https://example.com/b.xml\n\
                    SiTeMaP: https://example.com/c.xml\n";
        let v = parse_sitemap_directives(body);
        assert_eq!(v.len(), 3);
        assert!(v.contains(&"https://example.com/a.xml".to_owned()));
        assert!(v.contains(&"https://example.com/c.xml".to_owned()));
    }

    #[test]
    fn empty_when_no_directives_present() {
        let body = "User-agent: *\nDisallow: /private/\n";
        assert!(parse_sitemap_directives(body).is_empty());
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let body = "\n# leading comment\n\
                    \n\
                    Sitemap: https://example.com/sitemap.xml  # trailing comment\n\
                    # Sitemap: https://example.com/should-not-appear.xml\n";
        assert_eq!(
            parse_sitemap_directives(body),
            vec!["https://example.com/sitemap.xml".to_owned()]
        );
    }

    #[test]
    fn skips_non_http_values() {
        let body = "Sitemap: ftp://example.com/sitemap.xml\n\
                    Sitemap: /relative/sitemap.xml\n\
                    Sitemap: https://example.com/ok.xml\n";
        assert_eq!(parse_sitemap_directives(body), vec!["https://example.com/ok.xml".to_owned()]);
    }

    #[test]
    fn skips_blank_values() {
        let body = "Sitemap:\nSitemap:    \n";
        assert!(parse_sitemap_directives(body).is_empty());
    }

    #[test]
    fn deduplicates_repeated_urls() {
        let body = "Sitemap: https://example.com/sitemap.xml\n\
                    Sitemap: https://example.com/sitemap.xml\n";
        assert_eq!(
            parse_sitemap_directives(body),
            vec!["https://example.com/sitemap.xml".to_owned()]
        );
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let body = "this is garbage\n\
                    not even a directive\n\
                    \0\0\0\n\
                    Sitemap https://no-colon.example.com/sitemap.xml\n\
                    Sitemap: https://example.com/real.xml\n";
        assert_eq!(parse_sitemap_directives(body), vec!["https://example.com/real.xml".to_owned()]);
    }

    #[test]
    fn looks_like_sitemap_recognises_common_forms() {
        assert!(looks_like_sitemap_spec("https://example.com/sitemap.xml"));
        assert!(looks_like_sitemap_spec("https://example.com/SITEMAP.xml"));
        assert!(looks_like_sitemap_spec("https://example.com/sitemap-1.xml"));
        assert!(looks_like_sitemap_spec("https://example.com/foo.xml"));
        assert!(looks_like_sitemap_spec("https://example.com/foo.xml.gz"));
        assert!(looks_like_sitemap_spec("./sitemap.xml"));
    }

    #[test]
    fn looks_like_sitemap_rejects_bare_site_urls() {
        assert!(!looks_like_sitemap_spec("https://example.com"));
        assert!(!looks_like_sitemap_spec("https://example.com/"));
        assert!(!looks_like_sitemap_spec("https://docs.example.com/getting-started/"));
    }

    #[test]
    fn robots_url_strips_path_and_query() {
        let u = Url::parse("https://docs.example.com/foo/bar?q=1#frag").unwrap();
        assert_eq!(robots_url_for(&u).unwrap().as_str(), "https://docs.example.com/robots.txt");
    }

    #[test]
    fn robots_url_preserves_explicit_port() {
        let u = Url::parse("http://localhost:8080/whatever").unwrap();
        assert_eq!(robots_url_for(&u).unwrap().as_str(), "http://localhost:8080/robots.txt");
    }

    #[test]
    fn robots_url_rejects_non_http_schemes() {
        let u = Url::parse("file:///tmp/x").unwrap();
        assert!(robots_url_for(&u).is_none());
    }
}
