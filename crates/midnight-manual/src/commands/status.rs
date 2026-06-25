//! `mnm status` — quick "can I search and who am I" check. Renders the same
//! `StatusReport` the MCP `status` tool returns.

use anyhow::Result;
use clap::Args as ClapArgs;
use mnm_mcp::cloud_client::CloudClient;
use mnm_mcp::status::{assemble, CloudState, StatusReport, VoyageState};
use time::OffsetDateTime;

/// Arguments for `mnm status` (none beyond the globals).
#[derive(Debug, ClapArgs)]
pub struct Args {}

/// Run `mnm status`.
///
/// # Errors
///
/// Returns an error (non-zero exit) when the cloud is unreachable, so the
/// command is scriptable as a health probe.
pub async fn run(
    _args: Args,
    server: Option<&str>,
    voyage_api_key_flag: Option<&str>,
    json: bool,
) -> Result<()> {
    let url = crate::shared::resolve_server_url(server);
    let bearer = crate::shared::resolve_read_uplift_token();
    let cloud = CloudClient::new(&url, bearer)
        .map_err(|e| anyhow::anyhow!("cloud client init failed: {e}"))?;
    // Same resolution the embed-capable commands use (flag > env > config).
    let voyage_key = {
        let cfg_env = mnm_core::config::StdEnv;
        let (core_cfg, _) = mnm_core::config::Config::discover(None, &cfg_env)?;
        mnm_core::config::resolve_voyage_api_key(voyage_api_key_flag, &core_cfg.models, &cfg_env)
    };
    let report = assemble(&cloud, voyage_key.as_deref()).await;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report, &url);
    }
    if matches!(report.cloud, CloudState::Unreachable) {
        anyhow::bail!("cloud server unreachable at {url}");
    }
    Ok(())
}

/// Render the human-formatted report. Exposed for integration tests.
pub fn print_human(r: &StatusReport, url: &str) {
    println!("mnm status");
    println!(
        "  cloud:        {} ({url})",
        match r.cloud {
            CloudState::Reachable => "reachable",
            CloudState::Degraded => "degraded",
            CloudState::Unreachable => "UNREACHABLE",
        }
    );
    if let Some(v) = &r.cloud_version {
        println!("  server:       v{v}");
    }
    if r.authenticated {
        println!(
            "  auth:         {} as {} ({})",
            r.auth_type,
            r.identity.as_deref().unwrap_or("?"),
            r.permission_level,
        );
    } else {
        println!("  auth:         anonymous (read) — run `mnm auth github` for higher limits");
    }
    if let Some(rl) = &r.rate_limit {
        // `reset_secs` is a RELATIVE duration: seconds until the bucket refills.
        println!(
            "  requests:     {}/{} remaining ({} tier, resets in {}s)",
            rl.remaining, rl.limit, rl.tier, rl.reset_secs,
        );
    }
    if let Some(tl) = &r.token_limits {
        // `reset_at_secs` is an ABSOLUTE unix timestamp; render the windows'
        // budgets plus how far out (in minutes) the soonest window resets,
        // computed against the current wall clock.
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let mins_until = |reset_at_secs: i64| ((reset_at_secs - now).max(0) + 59) / 60;
        println!(
            "  embed tokens: {}/{} this hour (resets in ~{}m), {}/{} today (resets in ~{}m)",
            tl.hourly.remaining,
            tl.hourly.limit,
            mins_until(tl.hourly.reset_at_secs),
            tl.daily.remaining,
            tl.daily.limit,
            mins_until(tl.daily.reset_at_secs),
        );
    }
    println!(
        "  voyage key:   {}",
        match r.voyage {
            VoyageState::Valid => "valid",
            VoyageState::InvalidKey => "INVALID — check VOYAGE_API_KEY",
            VoyageState::Unreachable => "unreachable (could not verify)",
            VoyageState::NotConfigured => "not configured (server-proxy embedding)",
        }
    );
    println!(
        "  reranker:     {} ({})",
        r.reranker,
        if r.reranker_loaded {
            "loaded"
        } else {
            "loads on first reranked search"
        },
    );
}
