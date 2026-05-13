//! `mnm config show | get <key> | set <key> <value>`.
//!
//! Phase 5a lands `show` only (effective config dump); `get` and `set` land
//! with Phase 8 admin lifecycle (require writeback to the user's config file).

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

/// `mnm config <subcommand>`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The sub-subcommand.
    #[command(subcommand)]
    pub cmd: ConfigCmd,
}

/// `config` sub-subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Print the effective configuration (file + env + defaults merged).
    Show {
        /// Resolve env + flag overrides into the output.
        #[arg(long)]
        effective: bool,
    },
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error on config-discovery failure.
pub async fn run(args: Args, explicit_path: Option<&std::path::Path>, json: bool) -> Result<()> {
    let ConfigCmd::Show { effective: _ } = args.cmd;
    let env = mn_core::config::StdEnv;
    let (cfg, resolved) = mn_core::config::Config::discover(explicit_path, &env)?;

    if json {
        let payload = serde_json::json!({
            "config_path": resolved.map(|p| p.display().to_string()),
            "config": cfg,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        if let Some(p) = &resolved {
            println!("# resolved from: {}", p.display());
        } else {
            println!("# no config file found — using compiled defaults");
        }
        println!("{}", toml::to_string(&cfg)?);
    }
    Ok(())
}
