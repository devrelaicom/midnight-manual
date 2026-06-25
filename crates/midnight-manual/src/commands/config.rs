//! `mnm config show [--effective]`.
//!
//! Plain `show` prints the discovered configuration (config file merged with
//! compiled-in defaults). `--effective` additionally layers the env-var and
//! global-flag overrides the rest of the CLI would actually apply at runtime,
//! so operators can see the values a command will really use. Secrets (the
//! Voyage API key) are redacted in `--effective` output.

use std::path::Path;

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};
use mnm_core::config::{
    resolve_rerank_model, resolve_rerank_placement, resolve_voyage_api_key,
    resolve_voyage_base_url, resolve_voyage_timeout_secs, Config, ConfigEnv, StdEnv,
};

/// Marker substituted for a resolved secret in `--effective` output.
const REDACTED: &str = "****";

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
    /// Print the resolved configuration (config file merged with defaults).
    Show {
        /// Also layer env + global-flag overrides into the output (the values
        /// the CLI would actually use). Secrets are redacted.
        #[arg(long)]
        effective: bool,
    },
}

/// Dispatch.
///
/// # Errors
///
/// Returns an error on config-discovery failure.
pub async fn run(
    args: Args,
    explicit_path: Option<&Path>,
    server_flag: Option<&str>,
    voyage_api_key_flag: Option<&str>,
    no_telemetry: bool,
    json: bool,
) -> Result<()> {
    let ConfigCmd::Show { effective } = args.cmd;
    let env = StdEnv;
    let (mut cfg, resolved) = Config::discover(explicit_path, &env)?;

    if effective {
        apply_effective_overrides(&mut cfg, server_flag, voyage_api_key_flag, no_telemetry, &env)?;
    }

    if json {
        let payload = serde_json::json!({
            "config_path": resolved.map(|p| p.display().to_string()),
            "effective": effective,
            "config": cfg,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        if let Some(p) = &resolved {
            println!("# resolved from: {}", p.display());
        } else {
            println!("# no config file found — using compiled defaults");
        }
        if effective {
            println!(
                "# --effective: env/flag overrides + derived defaults resolved (e.g. rerank \
                 auto→local/server); secrets redacted. Not a copy-paste config file."
            );
        }
        println!("{}", toml::to_string(&cfg)?);
    }
    Ok(())
}

/// Layer the runtime overrides (global flags + env vars) onto `cfg` so it
/// reflects the values the CLI would actually resolve. Mirrors the precedence
/// used elsewhere (flag > env > config). The Voyage API key is replaced with a
/// redaction marker rather than printed.
fn apply_effective_overrides(
    cfg: &mut Config,
    server_flag: Option<&str>,
    voyage_api_key_flag: Option<&str>,
    no_telemetry: bool,
    env: &impl ConfigEnv,
) -> anyhow::Result<()> {
    // server.url — single source of truth: the shared resolver (flag, also
    // surfaced from MIDNIGHT_MANUAL_SERVER by clap, > cfg.server.url; trailing
    // slash trimmed).
    let url = crate::shared::resolve_server_url_from(server_flag, cfg);
    cfg.server.url = url;

    // models.voyage_api_key — resolve flag > env > config, then redact. We only
    // surface *whether* a key is effective, never its value.
    let resolved_key = resolve_voyage_api_key(voyage_api_key_flag, &cfg.models, env);
    cfg.models.voyage_api_key = resolved_key.as_ref().map(|_| REDACTED.to_owned());

    // models.voyage_timeout_secs — env > config (no global flag for timeout).
    cfg.models.voyage_timeout_secs = resolve_voyage_timeout_secs(None, &cfg.models, env)?;

    // rerank.location / rerank.model — resolve "auto"/env into the concrete
    // placement and model the CLI would use. Placement keys off whether a
    // Voyage key is effective.
    let placement = resolve_rerank_placement(None, &cfg.rerank, env, resolved_key.is_some())?;
    cfg.rerank.location = Some(placement.wire().to_owned());
    if let Some(model) = resolve_rerank_model(None, &cfg.rerank, env)?.model_name() {
        cfg.rerank.model = Some(model.to_owned());
    }

    // models.cache_dir — single source of truth: the embedding cache resolver
    // (config dir > env-chain > XDG/HOME).
    let cache_env = CacheEnvAdapter(env);
    if let Some(dir) =
        mnm_embedding::cache::resolve_with_override(cfg.models.cache_dir.as_deref(), &cache_env)
    {
        cfg.models.cache_dir = Some(dir);
    }

    // models.voyage_base_url — env > config; surface the effective value.
    cfg.models.voyage_base_url = resolve_voyage_base_url(&cfg.models, env);

    // telemetry.enabled — the --no-telemetry flag (env MIDNIGHT_MANUAL_DISABLE_TELEMETRY,
    // already resolved by clap) forces it off for this invocation.
    if no_telemetry {
        cfg.telemetry.enabled = false;
    }
    Ok(())
}

/// Bridges the config-side [`ConfigEnv`] trait onto the env-lookup trait
/// [`mnm_embedding::cache::Env`] so the cache-dir resolver can reuse whatever
/// env the caller passed in (the real `StdEnv`, or a `FakeEnv` in tests).
struct CacheEnvAdapter<'a, E: ConfigEnv>(&'a E);

impl<E: ConfigEnv> mnm_embedding::cache::Env for CacheEnvAdapter<'_, E> {
    fn var(&self, name: &str) -> Option<String> {
        self.0.var(name)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct FakeEnv(HashMap<String, String>);

    impl FakeEnv {
        fn set(mut self, k: &str, v: &str) -> Self {
            self.0.insert(k.into(), v.into());
            self
        }
    }

    impl ConfigEnv for FakeEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn server_flag_beats_env_and_trims_trailing_slash() {
        let mut cfg = Config::default();
        // In production, clap surfaces MIDNIGHT_MANUAL_SERVER → server_flag, so
        // the env var is already resolved before this function is called. Passing
        // the flag value directly mirrors that.
        let env = FakeEnv::default();
        apply_effective_overrides(&mut cfg, Some("http://from-flag:8080/"), None, false, &env).unwrap();
        assert_eq!(cfg.server.url, "http://from-flag:8080");
    }

    #[test]
    fn server_env_applies_when_no_flag() {
        // MIDNIGHT_MANUAL_SERVER is surfaced by clap as server_flag; unit tests
        // pass it directly to mirror what the binary does.
        let mut cfg = Config::default();
        let env = FakeEnv::default();
        apply_effective_overrides(&mut cfg, Some("http://localhost:8080/"), None, false, &env).unwrap();
        assert_eq!(cfg.server.url, "http://localhost:8080");
    }

    #[test]
    fn effective_server_url_uses_shared_resolver() {
        let mut cfg = Config::default();
        cfg.server.url = "https://from-config/".into();
        let env = FakeEnv::default();
        apply_effective_overrides(&mut cfg, Some("https://flag.example/"), None, false, &env).unwrap();
        // Flag wins, trailing slash trimmed — exactly resolve_server_url_from's contract.
        assert_eq!(cfg.server.url, "https://flag.example");
    }

    #[test]
    fn server_url_unchanged_when_no_override() {
        let mut cfg = Config::default();
        let default_url = cfg.server.url.clone();
        let env = FakeEnv::default();
        apply_effective_overrides(&mut cfg, None, None, false, &env).unwrap();
        assert_eq!(cfg.server.url, default_url);
    }

    #[test]
    fn voyage_key_is_redacted_not_leaked() {
        let mut cfg = Config::default();
        let env = FakeEnv::default().set("VOYAGE_API_KEY", "super-secret-value");
        apply_effective_overrides(&mut cfg, None, None, false, &env).unwrap();
        assert_eq!(cfg.models.voyage_api_key.as_deref(), Some(REDACTED));
        // The real secret never appears in the resolved config.
        let dumped = toml::to_string(&cfg).unwrap();
        assert!(!dumped.contains("super-secret-value"));
    }

    #[test]
    fn voyage_key_absent_stays_none() {
        let mut cfg = Config::default();
        let env = FakeEnv::default();
        apply_effective_overrides(&mut cfg, None, None, false, &env).unwrap();
        assert!(cfg.models.voyage_api_key.is_none());
    }

    #[test]
    fn voyage_timeout_from_env() {
        let mut cfg = Config::default();
        let env = FakeEnv::default().set("VOYAGE_TIMEOUT_SECS", "45");
        apply_effective_overrides(&mut cfg, None, None, false, &env).unwrap();
        assert_eq!(cfg.models.voyage_timeout_secs, 45);
    }

    #[test]
    fn rerank_placement_resolves_to_concrete_value() {
        let mut cfg = Config::default();
        // No key, auto everywhere → server.
        let env = FakeEnv::default();
        apply_effective_overrides(&mut cfg, None, None, false, &env).unwrap();
        assert_eq!(cfg.rerank.location.as_deref(), Some("server"));
        assert_eq!(cfg.rerank.model.as_deref(), Some("rerank-2.5"));

        // A key present (via flag) flips auto placement to local.
        let mut cfg2 = Config::default();
        apply_effective_overrides(&mut cfg2, None, Some("byok-key"), false, &FakeEnv::default()).unwrap();
        assert_eq!(cfg2.rerank.location.as_deref(), Some("local"));
    }

    #[test]
    fn no_telemetry_flag_disables_telemetry() {
        let mut cfg = Config::default();
        assert!(cfg.telemetry.enabled); // default on
        apply_effective_overrides(&mut cfg, None, None, true, &FakeEnv::default()).unwrap();
        assert!(!cfg.telemetry.enabled);
    }

    #[test]
    fn cache_dir_config_wins_over_env() {
        // resolve_with_override places the config value above the env chain
        // (MIDNIGHT_MANUAL_MODEL_CACHE_DIR > XDG > HOME). A config-set dir is
        // the most explicit user intent and wins outright.
        let mut cfg = Config::default();
        cfg.models.cache_dir = Some(std::path::PathBuf::from("/from/config"));
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_MODEL_CACHE_DIR", "/from/env");
        apply_effective_overrides(&mut cfg, None, None, false, &env).unwrap();
        assert_eq!(cfg.models.cache_dir.as_deref(), Some(std::path::Path::new("/from/config")));
    }

    #[test]
    fn cache_dir_env_applies_when_no_config_dir() {
        // When no config dir is set, MIDNIGHT_MANUAL_MODEL_CACHE_DIR is the
        // effective cache dir (env-chain top layer).
        let mut cfg = Config::default();
        assert!(cfg.models.cache_dir.is_none());
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_MODEL_CACHE_DIR", "/from/env");
        apply_effective_overrides(&mut cfg, None, None, false, &env).unwrap();
        assert_eq!(cfg.models.cache_dir.as_deref(), Some(std::path::Path::new("/from/env")));
    }

    #[test]
    fn cache_dir_config_value_surfaces_when_no_env() {
        let mut cfg = Config::default();
        cfg.models.cache_dir = Some(std::path::PathBuf::from("/from/config"));
        // No cache-dir env at all → the config value is the effective dir.
        let env = FakeEnv::default();
        apply_effective_overrides(&mut cfg, None, None, false, &env).unwrap();
        assert_eq!(cfg.models.cache_dir.as_deref(), Some(std::path::Path::new("/from/config")));
    }

    #[test]
    fn cache_dir_falls_back_to_xdg_default() {
        let mut cfg = Config::default();
        assert!(cfg.models.cache_dir.is_none());
        // No env override, no config value → resolves the XDG default location.
        let env = FakeEnv::default().set("XDG_DATA_HOME", "/xdg");
        apply_effective_overrides(&mut cfg, None, None, false, &env).unwrap();
        assert_eq!(
            cfg.models.cache_dir.as_deref(),
            Some(std::path::Path::new("/xdg/midnight-manual/models"))
        );
    }
}
