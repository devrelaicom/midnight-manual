//! `mnm versions list <slug>` — developer-facing source-version listing.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

/// `mnm versions <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: VersionsCmd,
}

/// `versions` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum VersionsCmd {
    /// List all source_versions for a slug. (Cloud-side; Phase 4 stubs the route.)
    List {
        /// Source slug.
        slug: String,
    },
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error if the cloud route is not yet implemented (returns a
/// human-readable hint pointing at the relevant phase).
pub async fn run(args: Args, _server: Option<&str>, _json: bool) -> Result<()> {
    match args.cmd {
        VersionsCmd::List { slug } => {
            // The `/v1/sources/{slug}/versions` route lands in Phase 7. For now
            // emit a clear hint so callers know what's missing.
            anyhow::bail!(
                "`versions list {slug}` requires the cloud /v1/sources/(slug)/versions endpoint \
                 which lands in Phase 7 of specs/001-rag-platform/tasks.md"
            );
        }
    }
}
