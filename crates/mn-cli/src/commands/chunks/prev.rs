//! `mnm chunks prev <chunk-id>` — fetch the previous N chunks.

use anyhow::Result;
use clap::Args as ClapArgs;
use uuid::Uuid;

#[derive(Debug, ClapArgs)]
pub struct Args {
    pub chunk_id: Uuid,
    #[arg(long, default_value_t = 5)]
    pub count: u32,
    #[arg(long)]
    pub full: bool,
}

pub async fn run(args: Args, server: Option<&str>, json: bool) -> Result<()> {
    // Reuse next::Args shape — they're identical.
    let next_args = super::next::Args {
        chunk_id: args.chunk_id,
        count: args.count,
        full: args.full,
    };
    super::run_chunk_list(next_args, server, json, "prev").await
}
