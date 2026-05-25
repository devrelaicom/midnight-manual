//! `mnm manifest <subcommand>` dispatcher. See §1 of the ingest-UX
//! design spec.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

pub mod generate;
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
    /// Populate a hierarchy.yaml from globs + optional sitemap.
    Generate(generate::Args),
}

pub async fn run(args: Args) -> Result<()> {
    match args.cmd {
        ManifestCmd::Init(a) => init::run(a),
        ManifestCmd::Generate(a) => generate::run(a).await,
    }
}
