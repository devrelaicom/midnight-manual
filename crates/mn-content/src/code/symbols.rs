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
                path: Vec::new(),
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

/// Find the start byte of the first node (in document order) whose kind is in
/// `table` and that begins within `[range_start, range_end)`.
///
/// Used to give a meaningful symbol path to chunks that begin with file-level
/// preamble — doc comments, `use`/`import` statements, attributes — before the
/// first named item. When [`symbol_path_at`] at a chunk's start byte is empty
/// (the chunk opens with such preamble), the caller retries at this offset so a
/// small single-chunk file still records the symbol it contains.
#[must_use]
pub fn first_symbol_start(
    tree: &tree_sitter::Tree,
    range_start: usize,
    range_end: usize,
    table: KindTable,
) -> Option<usize> {
    // Pre-order (parent-before-children, siblings left-to-right) cursor walk,
    // returning the first in-table node that begins within the range.
    let mut cursor = tree.root_node().walk();
    loop {
        let node = cursor.node();
        let start = node.start_byte();
        if start >= range_start
            && start < range_end
            && table.iter().any(|e| e.node_kind == node.kind())
        {
            return Some(start);
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return None;
            }
        }
    }
}

/// Enclosing in-table symbol headers for the node at `byte_offset`, outermost
/// first.
///
/// Each entry is `(node_start_byte, first_line)` where `first_line` is the
/// symbol's opening source line — trimmed, with a trailing `{` removed.
///
/// Mirrors [`symbol_path_at`]'s descent but captures node geometry, so callers
/// can tell which symbols a chunk *opens* (`node_start == chunk start`) from
/// those it is merely *inside* (`node_start < chunk start`).
#[must_use]
pub fn enclosing_symbol_headers(
    tree: &tree_sitter::Tree,
    src: &str,
    byte_offset: usize,
    table: KindTable,
) -> Vec<(usize, String)> {
    let mut headers = Vec::new();
    let mut node = tree.root_node();
    loop {
        if table.iter().any(|e| e.node_kind == node.kind()) {
            let start = node.start_byte();
            let line_end = src[start..].find('\n').map_or(src.len(), |off| start + off);
            let first_line = src
                .get(start..line_end)
                .unwrap_or_default()
                .trim()
                .trim_end_matches('{')
                .trim_end()
                .to_string();
            headers.push((start, first_line));
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
    headers
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

    #[test]
    fn enclosing_headers_capture_signature_lines() {
        let src = "namespace Big {\n  function big(x: number): number {\n    return x;\n  }\n}\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        let tree = parser.parse(src, None).unwrap();
        let off = src.find("return x").unwrap();
        let table = crate::code::ts::ts_kind_table();
        let headers = enclosing_symbol_headers(&tree, src, off, table);
        let lines: Vec<&str> = headers.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(lines, vec!["namespace Big", "function big(x: number): number"]);
        // Outermost first; node_start strictly ascending and < the offset.
        assert!(headers[0].0 < headers[1].0 && headers[1].0 < off);
    }
}
