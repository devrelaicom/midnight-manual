//! `mn-cli` library — exposes [`cli::run`] so the two bin shims
//! (midnight-manual and mnm) defer to a single entrypoint.

#![doc(html_root_url = "https://docs.rs/mn-cli/0.1.0")]
// CLI surface is verbose by nature; re-enable selectively as the surface
// stabilizes in later phases.
#![allow(
    clippy::doc_markdown,
    clippy::too_long_first_doc_paragraph,
    clippy::unused_async,
    clippy::option_if_let_else,
    clippy::format_in_format_args,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod cli;
pub mod commands;
pub mod progress;
pub mod shared;

/// Crate version stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
