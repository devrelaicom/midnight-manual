//! `mnm chunks prev <chunk-id>` — fetch the previous N chunks.

use anyhow::Result;
use clap::Args as ClapArgs;
use uuid::Uuid;

/// Arguments for `mnm chunks prev`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Chunk UUID.
    pub chunk_id: Uuid,
    /// Number of chunks to fetch (clamped to [1,100] server-side).
    #[arg(long, default_value_t = 5)]
    pub count: u32,
    /// Show full content instead of a 240-char preview.
    #[arg(long)]
    pub full: bool,
}

/// Run the `chunks prev` subcommand.
pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    // Reuse next::Args shape — they're identical.
    let next_args = super::next::Args {
        chunk_id: args.chunk_id,
        count: args.count,
        full: args.full,
    };
    super::run_chunk_list(next_args, server, json, "prev").await
}
