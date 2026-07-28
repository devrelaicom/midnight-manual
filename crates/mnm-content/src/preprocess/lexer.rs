//! Lightweight comment lexer. Line-oriented: classifies each line as
//! comment-only / code / blank while tracking naive string state and block
//! comments, then groups comment lines into [`CommentBlock`]s.
//!
//! The `strings` field in [`CommentSyntax`] stays in the syntax table for
//! future refinement but is not used by the lexer for whole-line
//! classification. This simplification is valid because every strip rule
//! operates only on comment-only lines or standalone blocks; comment tokens
//! appearing mid-line after code are classified `Code` and untouched. The one
//! theoretical false positive is a multi-line string continuation line that
//! *begins* with a comment token — the spec's accepted two-factor risk (must
//! also match a license at ≥0.9 or be pure punctuation).

use std::ops::Range;

use super::comment_syntax::CommentSyntax;

/// One comment block (spec §Comment lexer: a block comment, or a maximal
/// blank-line-free run of line-comment-only lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentBlock {
    /// Byte range in the source, marker-inclusive, spanning whole lines.
    pub span: Range<usize>,
    /// Comment text with markers stripped, lines joined by `\n`.
    pub text: String,
    /// 0-indexed first line of the block.
    pub first_line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum LineClass {
    Blank,
    CommentOnly,
    Code,
}

/// Lex `body` into comment blocks using `syntax`.
#[must_use]
pub fn lex_blocks(body: &str, syntax: &CommentSyntax) -> Vec<CommentBlock> {
    let mut blocks = Vec::new();
    // Current run of line comments: (start_byte, first_line, texts).
    let mut run: Option<(usize, usize, Vec<String>)> = None;
    let mut in_block: Option<(&'static str, usize, usize, Vec<String>)> = None; // (close, start_byte, first_line, texts)

    let mut offset = 0usize;
    for (line_no, line) in body.split_inclusive('\n').enumerate() {
        let line_start = offset;
        offset += line.len();
        let trimmed_nl = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = trimmed_nl.trim_start();

        if let Some((close, sb, fl, mut texts)) = in_block.take() {
            // Inside a block comment: look for the closer.
            if let Some(pos) = trimmed_nl.find(close) {
                let after = trimmed_nl[pos + close.len()..].trim();
                texts.push(clean_block_line(&trimmed_nl[..pos]));
                if after.is_empty() {
                    blocks.push(CommentBlock {
                        span: sb..(line_start + line.len()),
                        text: texts.join("\n").trim().to_owned(),
                        first_line: fl,
                    });
                } else {
                    // `*/ code` — block closed mid-line with trailing code:
                    // not a standalone comment block; discard.
                }
            } else {
                texts.push(clean_block_line(trimmed_nl));
                in_block = Some((close, sb, fl, texts));
            }
            continue;
        }

        let class = classify(trimmed, line_no, syntax);
        match class {
            LineClass::CommentOnly => {
                // Which opener? Block openers first (a `/* …` line enters block
                // state unless it also closes on the same line).
                if let Some(&(open, close)) = syntax
                    .block
                    .iter()
                    .find(|&&(open, _)| trimmed.starts_with(open))
                {
                    let rest = &trimmed[open.len()..];
                    if let Some(pos) = rest.find(close) {
                        // Single-line block comment. Only a standalone one
                        // (nothing after the closer) forms a block.
                        if rest[pos + close.len()..].trim().is_empty() {
                            flush_run(&mut run, &mut blocks, line_start);
                            blocks.push(CommentBlock {
                                span: line_start..(line_start + line.len()),
                                text: rest[..pos].trim().to_owned(),
                                first_line: line_no,
                            });
                        }
                    } else {
                        flush_run(&mut run, &mut blocks, line_start);
                        in_block = Some((close, line_start, line_no, vec![clean_block_line(rest)]));
                    }
                } else if let Some(open) =
                    syntax.line.iter().find(|open| trimmed.starts_with(*open))
                {
                    let text = trimmed[open.len()..].trim().to_owned();
                    match &mut run {
                        Some((_, _, texts)) => texts.push(text),
                        None => run = Some((line_start, line_no, vec![text])),
                    }
                }
            }
            LineClass::Blank | LineClass::Code => flush_run(&mut run, &mut blocks, line_start),
        }
    }
    let end = body.len();
    flush_run(&mut run, &mut blocks, end);
    // An unterminated block comment at EOF: treat as a block to EOF.
    if let Some((_, sb, fl, texts)) = in_block {
        blocks.push(CommentBlock {
            span: sb..end,
            text: texts.join("\n").trim().to_owned(),
            first_line: fl,
        });
    }
    blocks
}

fn flush_run(
    run: &mut Option<(usize, usize, Vec<String>)>,
    blocks: &mut Vec<CommentBlock>,
    end: usize,
) {
    if let Some((sb, fl, texts)) = run.take() {
        blocks.push(CommentBlock {
            span: sb..end,
            text: texts.join("\n").trim().to_owned(),
            first_line: fl,
        });
    }
}

/// Strip leading block-comment continuation decoration (` * foo` → `foo`).
fn clean_block_line(line: &str) -> String {
    let t = line.trim();
    t.strip_prefix('*').map_or(t, str::trim).to_owned()
}

/// Classify one (start-trimmed) line. `CommentOnly` = the line begins with a
/// comment opener (so any string-state concern is moot for OUR use: we only
/// ever strip whole comment-only lines/blocks; comment openers appearing
/// mid-line after code are classified `Code` and never touched). A shebang
/// (`#!` on line 0) is `Code` so `#`-comment languages don't lex it.
fn classify(trimmed: &str, line_no: usize, syntax: &CommentSyntax) -> LineClass {
    if trimmed.is_empty() {
        return LineClass::Blank;
    }
    if line_no == 0 && trimmed.starts_with("#!") {
        return LineClass::Code;
    }

    // Check for line comment openers.
    if syntax.line.iter().any(|o| trimmed.starts_with(o)) {
        return LineClass::CommentOnly;
    }

    // Check for block comment openers. If a block opens and closes on the same
    // line with trailing code, treat it as Code (not CommentOnly) so the line
    // is processed through the Code path and flushes any preceding comment run.
    if let Some(&(open, close)) = syntax
        .block
        .iter()
        .find(|&&(open, _)| trimmed.starts_with(open))
    {
        let rest = &trimmed[open.len()..];
        if let Some(pos) = rest.find(close) {
            let after = rest[pos + close.len()..].trim();
            if !after.is_empty() {
                // Mid-line close with trailing code: treat as Code.
                return LineClass::Code;
            }
        }
        // Block opener with no close on same line, or close-only: CommentOnly.
        return LineClass::CommentOnly;
    }

    LineClass::Code
}

/// How many leading `blocks` form the file's head run: every block whose span
/// starts before the first `Code`-classified line. Blank lines between head
/// blocks are fine.
#[must_use]
pub fn head_len(body: &str, blocks: &[CommentBlock]) -> usize {
    // Find the first byte offset of real code, skipping bytes covered by
    // comment blocks, blank lines, and a line-0 shebang.
    let mut code_start = body.len();
    let mut offset = 0usize;
    'lines: for (line_no, line) in body.split_inclusive('\n').enumerate() {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim();
        if trimmed.is_empty() || (line_no == 0 && trimmed.starts_with("#!")) {
            continue;
        }
        for b in blocks {
            if b.span.start <= start && start < b.span.end {
                continue 'lines;
            }
        }
        code_start = start;
        break;
    }
    blocks
        .iter()
        .take_while(|b| b.span.start < code_start)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::language::Language;
    use crate::preprocess::comment_syntax::comment_syntax;

    fn rust_syntax() -> crate::preprocess::comment_syntax::CommentSyntax {
        comment_syntax(Language::Rust).unwrap()
    }

    #[test]
    fn groups_consecutive_line_comments_into_one_block() {
        let body = "// a\n// b\n\n// c\nfn main() {}\n";
        let blocks = lex_blocks(body, &rust_syntax());
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].text, "a\nb");
        assert_eq!(blocks[1].text, "c");
        assert_eq!(&body[blocks[0].span.clone()], "// a\n// b\n");
    }

    #[test]
    fn block_comment_is_one_block_with_markers_stripped() {
        let body = "/* Copyright\n * Foo Corp\n */\nfn x() {}\n";
        let blocks = lex_blocks(body, &rust_syntax());
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].text.contains("Copyright"));
        assert!(blocks[0].text.contains("Foo Corp"));
        assert!(!blocks[0].text.contains("/*"));
    }

    #[test]
    fn trailing_comment_after_code_is_not_a_block() {
        let body = "let x = 1; // not a banner\n";
        assert!(lex_blocks(body, &rust_syntax()).is_empty());
    }

    #[test]
    fn comment_lookalike_inside_string_is_not_a_block() {
        let body = "let s = \"// not a comment\";\nlet t = \"/* nope */\";\n";
        assert!(lex_blocks(body, &rust_syntax()).is_empty());
    }

    #[test]
    fn head_len_counts_leading_blocks_shebang_tolerant() {
        let syn = comment_syntax(Language::Bash).unwrap();
        let body = "#!/usr/bin/env bash\n# copyright me\n\necho hi\n# later\n";
        let blocks = lex_blocks(body, &syn);
        // The shebang line itself is not a comment block; `# copyright me` is
        // head; `# later` (after real code) is not.
        assert_eq!(head_len(body, &blocks), 1);
    }

    #[test]
    fn hash_line_syntax_ignores_shebang() {
        let syn = comment_syntax(Language::Bash).unwrap();
        let body = "#!/bin/sh\necho hi\n";
        assert!(lex_blocks(body, &syn).is_empty());
    }

    #[test]
    fn mid_line_closed_block_with_trailing_code_flushes_run() {
        let body = "// header a\n// header b\n/* inline */ let x = 1;\nlet y = 2;\n";
        let blocks = lex_blocks(body, &rust_syntax());
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "header a\nheader b");
        assert_eq!(&body[blocks[0].span.clone()], "// header a\n// header b\n");
    }
}
