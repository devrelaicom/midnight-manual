//! `mnm version` — prints `{version, build_date}` JSON or a short line.

use anyhow::Result;
use serde::Serialize;

/// Build-time stamped metadata.
#[derive(Debug, Serialize)]
pub struct VersionInfo {
    /// Crate version from Cargo.toml.
    pub version: &'static str,
    /// Build host triple (filled at compile time via `env!("HOST")` when available;
    /// falls back to the target triple).
    pub host: &'static str,
}

impl VersionInfo {
    /// Read the build-time stamps.
    #[must_use]
    pub fn current() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            host: option_env!("HOST").unwrap_or(env!("TARGET_TRIPLE")),
        }
    }
}

/// Run the `version` subcommand.
///
/// # Errors
///
/// Returns an error if writing to stdout fails.
pub fn run(json: bool) -> Result<()> {
    let info = VersionInfo::current();
    if json {
        println!("{}", serde_json::to_string(&info)?);
    } else {
        println!("mnm {} ({})", info.version, info.host);
    }
    Ok(())
}
