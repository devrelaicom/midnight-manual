//! `mn-cli` — midnight-manual / `mnm` binary entrypoint.
//!
//! Phase-3 onwards lands subcommands (`sources`, `versions`, `ingest`, `models`,
//! `mcp serve`, `doctor`, etc.). This stub keeps the crate compilable so Phase 1's
//! workspace can land independently. See
//! [`specs/001-rag-platform/tasks.md`](../../../specs/001-rag-platform/tasks.md).

fn main() {
    eprintln!(
        "midnight-manual v{} — CLI implementation lands in Phase 3+. See specs/001-rag-platform/tasks.md",
        env!("CARGO_PKG_VERSION"),
    );
}
