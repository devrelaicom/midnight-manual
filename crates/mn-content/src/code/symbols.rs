//! Generic symbol-path extraction.
//!
//! Each language supplies a [`KindTable`] mapping tree-sitter node kinds to how
//! they contribute to a symbol path; the walker is otherwise language-agnostic.

use mn_core::types::SymbolSegment;

/// Maps a tree-sitter node-kind string to how it contributes to a symbol path.
pub struct KindEntry {
    /// tree-sitter node kind, e.g. `"impl_item"`, `"function_item"`.
    pub node_kind: &'static str,
    /// Symbol-path kind label, e.g. "impl", "fn".
    pub label: &'static str,
    /// Field name holding the identifier (e.g. "name"); `None` → use the first
    /// `identifier`/`type_identifier` descendant.
    pub name_field: Option<&'static str>,
}

/// A language's full node-kind → symbol mapping.
pub type KindTable = &'static [KindEntry];

/// Build the symbol path for the node containing `byte_offset`.
///
/// Walks from the root down to the deepest node containing the offset,
/// collecting a segment for each node whose kind is in `table`.
#[must_use]
pub fn symbol_path_at(
    tree: &tree_sitter::Tree,
    src: &str,
    byte_offset: usize,
    table: KindTable,
) -> Vec<SymbolSegment> {
    let mut path = Vec::new();
    let mut node = tree.root_node();
    loop {
        if let Some(entry) = table.iter().find(|e| e.node_kind == node.kind()) {
            // `node_name` returns `None` when no identifying child exists (e.g.
            // markup-language structural nodes whose header is a `bare_key` /
            // `tag_name` child rather than an `*identifier*` child).  For those
            // cases we still emit the path segment with an empty name so that
            // callers can match on `kind` alone.
            let name = node_name(node, src, entry.name_field).unwrap_or_default();
            path.push(SymbolSegment {
                kind: entry.label.to_string(),
                name,
            });
        }
        let next = {
            let mut cursor = node.walk();
            let found = node
                .named_children(&mut cursor)
                .find(|c| c.start_byte() <= byte_offset && byte_offset < c.end_byte());
            found
        };
        match next {
            Some(child) => node = child,
            None => break,
        }
    }
    path
}

fn node_name(node: tree_sitter::Node<'_>, src: &str, field: Option<&str>) -> Option<String> {
    let n = if let Some(f) = field {
        node.child_by_field_name(f)?
    } else {
        let mut w = node.walk();
        let found = node
            .named_children(&mut w)
            .find(|c| c.kind().contains("identifier"));
        found?
    };
    src.get(n.start_byte()..n.end_byte()).map(str::to_string)
}

#[cfg(all(test, feature = "core-grammars"))]
mod tests {
    use super::*;

    // Minimal inline table for the test (the real rust_kind_table lands in Task 13).
    const fn mini_rust_table() -> KindTable {
        &[
            KindEntry {
                node_kind: "impl_item",
                label: "impl",
                name_field: Some("type"),
            },
            KindEntry {
                node_kind: "function_item",
                label: "fn",
                name_field: Some("name"),
            },
        ]
    }

    #[test]
    fn rust_symbol_path_for_byte_range() {
        let src = "impl Foo {\n    fn bar(&self) {}\n}\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let off = src.find("fn bar").unwrap() + 1; // a byte offset inside `fn bar`
        let path = symbol_path_at(&tree, src, off, mini_rust_table());
        assert_eq!(
            path.iter()
                .map(|s| (s.kind.as_str(), s.name.as_str()))
                .collect::<Vec<_>>(),
            vec![("impl", "Foo"), ("fn", "bar")]
        );
    }
}
