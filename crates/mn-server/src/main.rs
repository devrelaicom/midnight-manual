//! `mn-server` — midnight-manual cloud HTTP server.
//!
//! Phase-3 onwards lands the routes, middleware, sweep job, and Fly.io deploy
//! plumbing. This stub keeps the crate compilable so Phase 1's workspace can land
//! independently. See [`specs/001-rag-platform/tasks.md`](../../../specs/001-rag-platform/tasks.md).

fn main() {
    eprintln!(
        "midnight-manual-server v{} — server implementation lands in Phase 3+.",
        env!("CARGO_PKG_VERSION"),
    );
}
