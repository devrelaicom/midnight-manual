//! D3: NO chunker emits overlapping chunks. Property-tested across the
//! markdown, code (rust), and line-window chunkers.

use mn_content::chunk::{Chunk, Chunker, ChunkerConfig};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

fn assert_disjoint(chunks: &[Chunk]) -> Result<(), TestCaseError> {
    for w in chunks.windows(2) {
        prop_assert!(
            w[1].start_byte >= w[0].end_byte,
            "overlap: [{}, {}) then [{}, {})",
            w[0].start_byte,
            w[0].end_byte,
            w[1].start_byte,
            w[1].end_byte
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn line_window_disjoint(lines in proptest::collection::vec("[a-z ]{0,80}", 1..200)) {
        let body = lines.join("\n");
        let cfg = ChunkerConfig { max_tokens: 64, ..ChunkerConfig::default() };
        let chunks = mn_content::code::line_window::LineWindowChunker.chunk(&body, &cfg).unwrap();
        assert_disjoint(&chunks)?;
    }

    #[test]
    fn markdown_disjoint(paras in proptest::collection::vec("[a-z ]{1,120}", 1..40)) {
        let body = paras.iter().enumerate().fold(String::new(), |mut s, (i, p)| {
            use std::fmt::Write as _;
            let _ = write!(s, "## H{i}\n\n{p}.\n\n");
            s
        });
        let cfg = ChunkerConfig { max_tokens: 64, ..ChunkerConfig::default() };
        let chunks = mn_content::markdown::MarkdownChunker.chunk(&body, &cfg).unwrap();
        assert_disjoint(&chunks)?;
    }

    // Valid items + trailing garbage exercise both the tree-sitter path
    // (symbol extraction + greedy coalescing) and the parser-error fallback.
    #[cfg(feature = "core-grammars")]
    #[test]
    fn rust_code_disjoint(
        fns in proptest::collection::vec("[a-z]{1,12}", 1..30),
        garbage in "[a-z{( ]{0,40}",
    ) {
        let mut body = fns.iter().enumerate().fold(String::new(), |mut s, (i, name)| {
            use std::fmt::Write as _;
            let _ = writeln!(s, "fn {name}_{i}() {{ let x = {i}; }}");
            s
        });
        body.push_str(&garbage);
        let cfg = ChunkerConfig { max_tokens: 64, ..ChunkerConfig::default() };
        let chunks = mn_content::code::rust::RustChunker.chunk(&body, &cfg).unwrap();
        assert_disjoint(&chunks)?;
    }
}
