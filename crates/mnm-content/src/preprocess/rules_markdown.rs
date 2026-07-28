//! Strip rules 5-8 (spec Strip rules / Markdown and MDX): HTML comments,
//! MDX comments, badge-only lines, MDX ESM imports/exports, and solo JSX
//! component-tag lines.
//!
//! All rules run OUTSIDE fenced code blocks only.

use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

/// A line consisting solely of one-or-more linked-image badges.
static BADGE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(\[!\[[^\]]*\]\([^)]*\)\]\([^)]*\)\s*)+$").expect("static regex")
});

/// A line consisting solely of JSX component tags (capitalized names),
/// opening / closing / self-closing, one or more per line.
static JSX_SOLO_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(</?[A-Z][A-Za-z0-9_.]*(\s[^<>]*)?/?>\s*)+$").expect("static regex")
});

/// Everything `apply_markdown_rules` decided.
#[derive(Debug, Default)]
pub struct MarkdownRuleOutcome {
    /// Sorted, non-overlapping `(range, replacement)` edits.
    pub edits: Vec<(Range<usize>, String)>,
    /// Bytes removed by the HTML/MDX comment rules (5-6).
    pub html_comment_bytes: usize,
    /// Bytes removed by the MDX ESM import/export rule (7); always 0 for `.md`.
    pub mdx_esm_bytes: usize,
    /// Bytes removed by the solo JSX component-tag rule (8); always 0 for `.md`.
    pub mdx_jsx_bytes: usize,
    /// Bytes removed by the badge-only-line rule.
    pub badge_bytes: usize,
}

/// Track fence state across lines. Returns per-line `(start, len, in_fence)`.
fn fence_map(body: &str) -> Vec<(usize, usize, bool)> {
    let mut lines = Vec::new();
    let mut in_fence: Option<(char, usize)> = None; // (fence char, min close length)
    let mut offset = 0usize;
    for line in body.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let content = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = content.trim_start();
        let indent = content.len() - trimmed.len();
        let fence_here = indent <= 3 && (trimmed.starts_with("```") || trimmed.starts_with("~~~"));
        match (&in_fence, fence_here) {
            (None, true) => {
                let ch = trimmed.chars().next().expect("fence char");
                let len = trimmed.chars().take_while(|&c| c == ch).count();
                // The fence line itself counts as "in fence" (untouchable).
                lines.push((start, line.len(), true));
                in_fence = Some((ch, len));
            }
            (Some((ch, len)), true) => {
                let tch = trimmed.chars().next().expect("fence char");
                let tlen = trimmed.chars().take_while(|&c| c == tch).count();
                lines.push((start, line.len(), true));
                if tch == *ch && tlen >= *len && trimmed[tlen..].trim().is_empty() {
                    in_fence = None;
                }
            }
            (Some(_), false) => lines.push((start, line.len(), true)),
            (None, false) => lines.push((start, line.len(), false)),
        }
    }
    lines
}

/// True when byte position `pos` (relative to `line`) sits inside an inline
/// code span - i.e. an odd number of backticks precede it on the line.
fn in_inline_code(line: &str, pos: usize) -> bool {
    line[..pos].matches('`').count() % 2 == 1
}

/// Apply rules 5-8. `is_mdx` gates rules 7 (ESM) and 8 (JSX).
#[must_use]
pub fn apply_markdown_rules(body: &str, is_mdx: bool) -> MarkdownRuleOutcome {
    let mut out = MarkdownRuleOutcome::default();
    let lines = fence_map(body);

    // --- Rules 5+6: comments. Handled with a scan supporting multi-line
    // spans; a comment OPENING inside a fence or inline code is skipped.
    for (open, close) in [("<!--", "-->"), ("{/*", "*/}")] {
        let mut search_from = 0usize;
        while let Some(rel) = body[search_from..].find(open) {
            let abs = search_from + rel;
            search_from = abs + open.len();
            let Some(&(ls, ll, in_fence)) = lines
                .iter()
                .find(|&&(ls, ll, _)| ls <= abs && abs < ls + ll)
            else {
                break;
            };
            if in_fence {
                continue;
            }
            let line = &body[ls..ls + ll];
            if in_inline_code(line, abs - ls) {
                continue;
            }
            let Some(close_rel) = body[abs..].find(close) else {
                continue; // unterminated: leave it
            };
            let mut end = abs + close_rel + close.len();
            // If closing sits inside a fence, leave the comment alone
            // (pathological; accepted).
            if lines
                .iter()
                .any(|&(ls2, ll2, f2)| f2 && ls2 < end && end - 1 < ls2 + ll2)
            {
                continue;
            }
            // Extend over a following newline when the strip empties the line.
            let before_on_line = body[ls..abs].trim();
            let after = &body[end..];
            let after_on_line = after.split('\n').next().unwrap_or("").trim();
            if before_on_line.is_empty() && after_on_line.is_empty() {
                if let Some(nl) = after.find('\n') {
                    end += nl + 1;
                }
            }
            out.html_comment_bytes += end - abs;
            out.edits.push((abs..end, String::new()));
            search_from = end;
        }
    }

    // --- Line-based rules: badges (all markdown), ESM + JSX (mdx only).
    let mut i = 0usize;
    while i < lines.len() {
        let (ls, ll, in_fence) = lines[i];
        i += 1;
        if in_fence {
            continue;
        }
        let already = out
            .edits
            .iter()
            .any(|(r, _)| r.start < ls + ll && ls < r.end);
        if already {
            continue;
        }
        let line = &body[ls..ls + ll];
        let content = line.strip_suffix('\n').unwrap_or(line);

        if BADGE_LINE.is_match(content) {
            out.badge_bytes += line.len();
            out.edits.push((ls..ls + ll, String::new()));
            continue;
        }
        if is_mdx {
            let trimmed = content.trim_start();
            if trimmed.starts_with("import ")
                || trimmed.starts_with("import{")
                || trimmed.starts_with("export ")
            {
                // Consume the ESM statement across lines until it terminates:
                // a line ending in `;`, or ending in a quoted specifier, with
                // a 20-line bail-out.
                let start_byte = ls;
                let mut end_byte = ls + ll;
                let mut consumed = 1usize;
                let mut cur = content.trim_end();
                while !esm_terminated(cur) && consumed < 20 && i < lines.len() {
                    let (nls, nll, nfence) = lines[i];
                    if nfence {
                        break;
                    }
                    cur = body[nls..nls + nll].trim_end_matches('\n').trim_end();
                    end_byte = nls + nll;
                    i += 1;
                    consumed += 1;
                }
                out.mdx_esm_bytes += end_byte - start_byte;
                out.edits.push((start_byte..end_byte, String::new()));
                continue;
            }
            if JSX_SOLO_LINE.is_match(content) {
                out.mdx_jsx_bytes += line.len();
                out.edits.push((ls..ls + ll, String::new()));
            }
        }
    }

    // Sort + drop overlaps (comment edits were pushed before line edits and
    // line edits skip overlapping ranges already, but be defensive).
    out.edits.sort_by_key(|(r, _)| r.start);
    out.edits.dedup_by(|(b, _), (a, _)| b.start < a.end);
    out
}

/// An ESM statement is terminated when the line ends with `;` or with a
/// quoted module specifier (`'x'` / `"x"`), optionally + `;`.
fn esm_terminated(line: &str) -> bool {
    let l = line.trim_end_matches(';').trim_end();
    (line.ends_with(';') && !l.ends_with(',') && !l.ends_with('{'))
        || (l.ends_with('\'') && l.len() > 1)
        || (l.ends_with('"') && l.len() > 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preprocess::rules_code::apply_edits;

    fn run(body: &str, mdx: bool) -> String {
        let out = apply_markdown_rules(body, mdx);
        apply_edits(body, &out.edits)
    }

    #[test]
    fn strips_html_and_mdx_comments() {
        let body = "# T\n<!-- markdownlint-disable -->\nText {/* mdx note */} here.\n";
        let s = run(body, true);
        assert!(!s.contains("markdownlint"));
        assert!(!s.contains("mdx note"));
        assert!(s.contains("Text  here."));
    }

    #[test]
    fn multiline_html_comment_stripped() {
        let body = "a\n<!--\nline1\nline2\n-->\nb\n";
        let s = run(body, false);
        assert!(!s.contains("line1"));
        assert!(s.contains("a\n"));
        assert!(s.contains("b\n"));
    }

    #[test]
    fn fenced_blocks_are_untouchable() {
        let body = "```tsx\nimport Tabs from '@theme/Tabs';\n<!-- kept -->\n<Tabs>\n```\ntext\n";
        let s = run(body, true);
        assert!(s.contains("import Tabs"));
        assert!(s.contains("<!-- kept -->"));
        assert!(s.contains("<Tabs>"));
    }

    #[test]
    fn tilde_fences_and_indented_fences_tracked() {
        let body = "~~~\n<!-- kept -->\n~~~\n   ```\n<!-- also kept -->\n   ```\n<!-- gone -->\n";
        let s = run(body, false);
        assert!(s.contains("<!-- kept -->"));
        assert!(s.contains("<!-- also kept -->"));
        assert!(!s.contains("<!-- gone -->"));
    }

    #[test]
    fn inline_code_spans_protect_comments() {
        let body = "Use `<!-- like this -->` to comment.\n";
        assert_eq!(run(body, false), body);
    }

    #[test]
    fn badge_only_lines_drop_but_plain_images_stay() {
        let body = "[![CI](https://img.shields.io/badge/ci.svg)](https://ci.example) [![Docs](https://img.shields.io/badge/docs.svg)](https://docs.example)\n\n![architecture diagram](arch.png)\n";
        let s = run(body, false);
        assert!(!s.contains("shields.io"));
        assert!(s.contains("architecture diagram"));
    }

    #[test]
    fn mdx_esm_imports_and_solo_jsx_tags_drop_in_mdx_only() {
        let body = "import Tabs from '@theme/Tabs';\nimport TabItem from '@theme/TabItem';\n\n<Tabs>\n<TabItem value=\"ts\" label=\"TypeScript\">\ncontent here\n</TabItem></Tabs>\n";
        let s = run(body, true);
        assert!(!s.contains("import Tabs"));
        assert!(!s.contains("<Tabs>"));
        assert!(!s.contains("</TabItem></Tabs>"));
        assert!(s.contains("content here"));
        // Same body as .md: imports/tags survive.
        let s_md = run(body, false);
        assert!(s_md.contains("import Tabs"));
        assert!(s_md.contains("<Tabs>"));
    }

    #[test]
    fn multiline_import_consumed_until_terminator() {
        let body = "import {\n  A,\n  B,\n} from '@site/src';\n\ntext\n";
        let s = run(body, true);
        assert!(!s.contains("from '@site/src'"));
        assert!(s.contains("text"));
    }

    #[test]
    fn multiline_jsx_open_tag_is_kept() {
        // Accepted simplification (spec rule 8): attribute-spanning tags stay.
        let body = "<TabItem\n  value=\"ts\">\ncontent\n</TabItem>\n";
        let s = run(body, true);
        assert!(s.contains("<TabItem\n"));
        assert!(!s.contains("</TabItem>"));
        assert!(s.contains("content"));
    }

    #[test]
    fn lowercase_html_tags_untouched_in_mdx() {
        let body = "<div>\nhi\n</div>\n";
        assert_eq!(run(body, true), body);
    }
}
