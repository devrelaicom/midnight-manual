//! `mnm` — the short-name alias of `midnight-manual`.
//!
//! Both binaries dispatch into the same future `mn_cli` library entrypoint
//! (added in Phase 3). For Phase-1 scaffolding the two binaries share an
//! identical placeholder message so cargo never sees two bin targets pointing
//! at the same `main.rs` file.

fn main() {
    eprintln!(
        "mnm v{} — CLI implementation lands in Phase 3+. See specs/001-rag-platform/tasks.md",
        env!("CARGO_PKG_VERSION"),
    );
}
