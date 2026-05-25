//! `mnm manifest <subcommand>` dispatcher. See §1 of the ingest-UX
//! design spec.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

pub mod init;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: ManifestCmd,
}

#[derive(Debug, Subcommand)]
pub enum ManifestCmd {
    /// Write an empty starter manifest with comments.
    Init(init::Args),
}

pub async fn run(args: Args) -> Result<()> {
    match args.cmd {
        ManifestCmd::Init(a) => init::run(a),
    }
}
