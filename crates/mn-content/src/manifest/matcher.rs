//! Match files to sitemap URLs.
//!
//! Order: frontmatter slug → leaf basename → tail-relaxation tie-break.
//! See §1.2 of docs/superpowers/specs/2026-05-25-ingest-ux-design.md

use std::path::Path;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchReason {
    Slug,
    Leaf,
    LeafWithParentDir,
    None,
}

#[derive(Debug, Clone)]
pub struct Match {
    pub url: Option<Url>,
    pub reason: MatchReason,
}

/// Match one file against a flat list of sitemap URLs.
///
/// `slug` is the file's frontmatter slug if present.
#[must_use]
pub fn match_file(file_rel: &Path, slug: Option<&str>, urls: &[Url]) -> Match {
    if let Some(s) = slug {
        if let Some(u) = urls.iter().find(|u| last_segment(u) == Some(s)) {
            return Match {
                url: Some(u.clone()),
                reason: MatchReason::Slug,
            };
        }
    }

    let leaf = file_rel
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if leaf.is_empty() {
        return Match { url: None, reason: MatchReason::None };
    }

    let leaf_hits: Vec<&Url> = urls
        .iter()
        .filter(|u| last_segment(u).map(|s| s == leaf).unwrap_or(false))
        .collect();
    match leaf_hits.len() {
        0 => Match { url: None, reason: MatchReason::None },
        1 => Match {
            url: Some(leaf_hits[0].clone()),
            reason: MatchReason::Leaf,
        },
        _ => disambiguate_by_tail(file_rel, &leaf_hits),
    }
}

/// Strip a leading `docs/` (or any single first dir) from the file's
/// path; then pick the URL whose trailing path-suffix shares the
/// longest tail with the file's path-suffix.
fn disambiguate_by_tail(file_rel: &Path, candidates: &[&Url]) -> Match {
    let file_tail = file_suffix_segments(file_rel);
    let mut best: Option<(&Url, usize)> = None;
    let mut tied = false;
    for cand in candidates {
        let url_tail = url_path_segments(cand);
        let common = common_suffix_len(&file_tail, &url_tail);
        match best {
            None => best = Some((cand, common)),
            Some((_, c)) if common > c => {
                best = Some((cand, common));
                tied = false;
            }
            Some((_, c)) if common == c => tied = true,
            _ => {}
        }
    }
    if tied {
        return Match { url: None, reason: MatchReason::None };
    }
    Match {
        url: best.map(|(u, _)| u.clone()),
        reason: MatchReason::LeafWithParentDir,
    }
}

fn last_segment(u: &Url) -> Option<&str> {
    u.path_segments()?.filter(|s| !s.is_empty()).last()
}

fn url_path_segments(u: &Url) -> Vec<String> {
    u.path_segments()
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// File's path segments minus extension on the last, with the FIRST
/// directory optionally stripped (e.g. `docs/`).
fn file_suffix_segments(file_rel: &Path) -> Vec<String> {
    let mut segs: Vec<String> = file_rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str().map(str::to_owned),
            _ => None,
        })
        .collect();
    if let Some(last) = segs.last_mut() {
        if let Some(idx) = last.rfind('.') {
            last.truncate(idx);
        }
    }
    // Drop a leading "docs" if there's more than one segment to compare.
    if segs.len() > 1 && segs[0] == "docs" {
        segs.remove(0);
    }
    segs
}

fn common_suffix_len(a: &[String], b: &[String]) -> usize {
    let mut n = 0;
    for (x, y) in a.iter().rev().zip(b.iter().rev()) {
        if x == y {
            n += 1;
        } else {
            break;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn slug_match_wins_when_frontmatter_has_one() {
        let urls = vec![
            url("https://docs.example.com/cookbook/auth/"),
            url("https://docs.example.com/sign-in/"),
        ];
        let m = match_file(Path::new("docs/cookbook/auth.md"), Some("sign-in"), &urls);
        assert_eq!(m.reason, MatchReason::Slug);
        assert_eq!(m.url.unwrap().as_str(), "https://docs.example.com/sign-in/");
    }

    #[test]
    fn leaf_match_unique() {
        let urls = vec![url("https://docs.example.com/cookbook/auth/")];
        let m = match_file(Path::new("docs/cookbook/auth.md"), None, &urls);
        assert_eq!(m.reason, MatchReason::Leaf);
    }

    #[test]
    fn leaf_ambiguous_resolved_by_parent_dir() {
        let urls = vec![
            url("https://docs.example.com/cookbook/auth/"),
            url("https://docs.example.com/extras/auth/"),
        ];
        let m = match_file(Path::new("docs/cookbook/auth.md"), None, &urls);
        assert_eq!(m.reason, MatchReason::LeafWithParentDir);
        assert!(m.url.unwrap().path().contains("/cookbook/"));
    }

    #[test]
    fn leaf_ambiguous_still_tied_returns_none() {
        let urls = vec![
            url("https://docs.example.com/auth/"),
            url("https://docs.example.com/v2/auth/"),
        ];
        // File path has no parent context to break the tie.
        let m = match_file(Path::new("auth.md"), None, &urls);
        assert_eq!(m.reason, MatchReason::None);
        assert!(m.url.is_none());
    }
}
