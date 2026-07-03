//! midnight-manual telemetry: a thin adapter over `gauge-telemetry`.
//!
//! Defines the app's seven event types ([`events`]), a [`build`] helper that
//! wires the Gauge [`Telemetry`] handle with midnight-manual's opt-out,
//! identity, and endpoint, and a slim opt-out marker module ([`optout`]).

pub mod canary;
pub mod events;
pub mod optout;

use std::path::PathBuf;

pub use gauge_telemetry::client::DEFAULT_FLUSH_TIMEOUT;
pub use gauge_telemetry::common::Surface;
pub use gauge_telemetry::{Flusher, Telemetry};

pub use events::{
    CliCommand, CliCommandName, IngestComplete, McpParamAliasRewrite, McpShutdown, McpStartup,
    McpToolCall, McpToolName, ModelState, Outcome, ParamAlias, PullModels, Rerank,
};

/// Crate version, stamped at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The Gauge `service.name` for this app. Must match the server allowlist.
pub const APP_NAME: &str = "midnight-manual";

/// Args the binary re-execs for a detached flush (the hidden subcommand).
pub const FLUSH_ARGS: &[&str] = &["telemetry", "flush"];

/// Inputs to [`build`]. The caller resolves opt-out and the endpoint first.
pub struct BuildParams {
    /// The consuming binary's version (e.g. `env!("CARGO_PKG_VERSION")`).
    pub app_version: String,
    /// Resolved Gauge endpoint (from `mnm_core::config::resolve_telemetry_endpoint`).
    pub endpoint: String,
    /// Install-id path (`mnm_core::paths::telemetry_install_id_path`); `None` disables.
    pub install_id_path: Option<PathBuf>,
    /// Config `[telemetry].enabled` (mechanism #2).
    pub config_enabled: bool,
    /// `true` unless env opt-out (#1), marker (#3), or `--no-telemetry` apply.
    pub runtime_enabled: bool,
    /// Detached-flush args ([`FLUSH_ARGS`]); empty disables detached flush.
    pub flush_args: Vec<String>,
}

/// Build the Gauge telemetry handle. Never fails the caller: on a missing
/// install path or a build error (e.g. a misconfigured endpoint) it logs at
/// `warn` and returns a no-op handle.
#[must_use]
pub fn build(p: BuildParams) -> Telemetry {
    let Some(install_id_path) = p.install_id_path else {
        return disabled(&p.app_version);
    };
    match Telemetry::builder()
        .app(APP_NAME)
        .app_version(p.app_version.clone())
        .endpoint(p.endpoint)
        .install_id_path(install_id_path)
        .config_enabled(p.config_enabled)
        .runtime_enabled(p.runtime_enabled)
        .flush_args(p.flush_args)
        .build()
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "telemetry disabled: failed to build handle");
            disabled(&p.app_version)
        }
    }
}

/// A guaranteed no-op handle. `config_enabled(false)` makes `build()` resolve
/// consent to off and return before any endpoint check or filesystem work.
fn disabled(app_version: &str) -> Telemetry {
    Telemetry::builder()
        .app(APP_NAME)
        .app_version(app_version.to_owned())
        .endpoint("https://telemetry.disabled.invalid")
        .install_id_path(PathBuf::from("/nonexistent/mnm-telemetry-id"))
        .config_enabled(false)
        .build()
        .expect("disabled telemetry handle has all required fields")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_returns_disabled_when_config_off() {
        let t = build(BuildParams {
            app_version: "0.0.0".into(),
            endpoint: "https://gauge-telemetry.fly.dev".into(),
            install_id_path: Some(std::env::temp_dir().join("mnm-test-id-unused")),
            config_enabled: false,
            runtime_enabled: true,
            flush_args: FLUSH_ARGS.iter().map(|s| (*s).to_owned()).collect(),
        });
        // A disabled handle is a no-op: emit/flush must not panic or write.
        t.emit(&events::CliCommand {
            command: events::CliCommandName::Version,
            duration_ms: 0,
            outcome: events::Outcome::Ok,
        });
        t.flush_blocking(DEFAULT_FLUSH_TIMEOUT);
    }

    #[test]
    fn build_returns_disabled_when_no_install_path() {
        let t = build(BuildParams {
            app_version: "0.0.0".into(),
            endpoint: "https://gauge-telemetry.fly.dev".into(),
            install_id_path: None,
            config_enabled: true,
            runtime_enabled: true,
            flush_args: vec![],
        });
        t.emit(&events::McpShutdown { uptime_s: 1, tools_served: 0 });
    }

    #[test]
    fn flush_args_are_the_hidden_subcommand() {
        assert_eq!(FLUSH_ARGS, &["telemetry", "flush"]);
    }
}
