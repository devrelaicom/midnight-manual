//! `mnm manifest <subcommand>` dispatcher. See §1 of the ingest-UX
//! design spec.

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

pub mod check;
pub mod generate;
pub mod init;

/// Top-level arguments for `mnm manifest`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The manifest subcommand to execute.
    #[command(subcommand)]
    pub cmd: ManifestCmd,
}

/// `mnm manifest` subcommands.
#[derive(Debug, Subcommand)]
pub enum ManifestCmd {
    /// Write an empty starter manifest with comments.
    Init(init::Args),
    /// Populate a hierarchy.yaml from globs + optional sitemap.
    Generate(generate::Args),
    /// Validate a manifest locally: schema, paths, file existence.
    Check(check::Args),
}

/// Dispatch `mnm manifest <subcommand>`.
pub async fn run(args: Args) -> Result<()> {
    match args.cmd {
        ManifestCmd::Init(a) => init::run(&a),
        ManifestCmd::Generate(a) => generate::run(a).await,
        ManifestCmd::Check(a) => check::run(a).await,
    }
}
