//! Code chunkers: tree-sitter + text-splitter per language, plus the shared
//! line-window fallback. Dispatch lands in a later task.

pub mod language;
pub mod line_window;
pub mod splitter;
pub mod symbols;

use crate::chunk::Chunker;
use language::Language;
use line_window::LineWindowChunker;

/// Return the chunker for `lang`.
///
/// Languages whose grammar feature is not compiled fall back to line-window
/// (graceful degradation). Per-language arms are added as each grammar lands
/// (Task 13+); until then every language uses the line-window fallback.
#[must_use]
pub fn chunker_for(_lang: Language) -> Box<dyn Chunker> {
    Box::new(LineWindowChunker)
}
