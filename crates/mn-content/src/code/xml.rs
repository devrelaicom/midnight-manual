//! XML chunker: tree-sitter-xml + token budgeting + symbol paths.

use crate::chunk::{Chunk, ChunkError, Chunker, ChunkerConfig};
use crate::code::symbols::{KindEntry, KindTable};

/// Node-kind → symbol mapping for XML.
///
/// `element` is the structural unit in XML (e.g. `<root>…</root>`).  The
/// element's tag name lives in an `STag`/`ETag` descendant whose kind does not
/// contain `"identifier"`, so the generic walker cannot extract it; the path
/// entry is emitted with an empty name.  Callers should assert on
/// `kind == "element"` rather than the name.
#[must_use]
pub const fn xml_kind_table() -> KindTable {
    &[KindEntry {
        node_kind: "element",
        label: "element",
        name_field: None,
    }]
}

/// XML markup chunker backed by `tree-sitter-xml`.
pub struct XmlChunker;

impl Chunker for XmlChunker {
    fn chunk(&self, body: &str, cfg: &ChunkerConfig) -> Result<Vec<Chunk>, ChunkError> {
        crate::code::run_tree_sitter(
            body,
            cfg,
            &tree_sitter_xml::LANGUAGE_XML.into(),
            xml_kind_table(),
            "",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunker, ChunkerConfig};

    #[test]
    fn xml_element_symbol_path() {
        // The outer `<root>` starts at byte 0, so the symbol path should
        // contain at least one entry with kind == "element".
        let src = "<root><child>x</child></root>\n";
        let chunks = XmlChunker.chunk(src, &ChunkerConfig::default()).unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_path.iter().any(|s| s.kind == "element")),
            "expected a symbol-path entry with kind 'element', got: {chunks:?}"
        );
    }
}
