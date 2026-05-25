//! Shared helpers used across multiple subcommands.
//!
//! Hoists the previously-duplicated server-URL resolver out of
//! `commands/sources.rs` so `login`, `users`, and any future write-side
//! command can share the same precedence walk.

/// Resolve the cloud server URL.
///
/// Precedence (D17):
///
/// 1. The explicit `--server` flag (or `MIDNIGHT_MANUAL_SERVER` env, surfaced
///    by clap on the global flag).
///
/// 2. The resolved config's `[server].url`.
///
/// 3. The compiled-in default
///    (`<https://midnight-manual.midnightntwrk.expert>`).
///
/// Trailing `/` is stripped so callers can safely format `format!("{url}/v1/...")`.
#[must_use]
pub fn resolve_server_url(flag: Option<&str>) -> String {
    if let Some(s) = flag {
        return s.trim_end_matches('/').to_owned();
    }
    let env = mn_core::config::StdEnv;
    let (cfg, _) = mn_core::config::Config::discover(None, &env).unwrap_or_default();
    cfg.server.url.trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_wins_and_strips_trailing_slash() {
        let url = resolve_server_url(Some("http://localhost:8080/"));
        assert_eq!(url, "http://localhost:8080");
    }

    #[test]
    fn flag_passthrough_when_no_trailing_slash() {
        let url = resolve_server_url(Some("http://localhost:8080"));
        assert_eq!(url, "http://localhost:8080");
    }
}
