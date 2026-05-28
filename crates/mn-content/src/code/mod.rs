//! Code chunkers: tree-sitter + text-splitter per language, plus the shared
//! line-window fallback. Dispatch lands in a later task.

pub mod line_window;
