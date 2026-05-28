//! HTML chunker: tree-sitter-html + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for HTML.
///
/// `element` is the structural unit in HTML (e.g. `<div>…</div>`).  The
/// element's tag name lives in a `tag_name` child node whose kind does not
/// contain `"identifier"`, so the generic walker cannot extract it; the path
/// entry is emitted with an empty name.  Callers should assert on
/// `kind == "element"` rather than the name.
#[must_use]
pub const fn html_kind_table() -> KindTable {
    &[KindEntry {
        node_kind: "element",
        label: "element",
        name_field: None,
    }]
}

/// HTML markup chunker backed by `tree-sitter-html`.
pub struct HtmlChunker;

impl Chunker for HtmlChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_html::LANGUAGE.into(),
            html_kind_table(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn html_element_symbol_path() {
        // The outer `<div>` starts at byte 0, so the symbol path should
        // contain at least one entry with kind == "element".
        let src = "<div><span>x</span></div>\n";
        let chunks = HtmlChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_path.iter().any(|s| s.kind == "element")),
            "expected a symbol-path entry with kind 'element', got: {chunks:?}"
        );
    }
}
