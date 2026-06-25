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
    resolve_voyage_timeout_secs, Config, ConfigEnv, StdEnv,
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
    // server.url — flag (also surfaced from MIDNIGHT_MANUAL_SERVER by clap) >
    // env > existing config value. Trailing slash trimmed, matching
    // `shared::resolve_server_url`.
    if let Some(url) = server_flag
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| env.var("MIDNIGHT_MANUAL_SERVER").filter(|s| !s.is_empty()))
    {
        url.trim_end_matches('/').clone_into(&mut cfg.server.url);
    }

    // models.voyage_api_key — resolve flag > env > config, then redact. We only
    // surface *whether* a key is effective, never its value.
    let resolved_key = resolve_voyage_api_key(voyage_api_key_flag, &cfg.models, env);
    cfg.models.voyage_api_key = resolved_key.as_ref().map(|_| REDACTED.to_owned());

    // models.voyage_timeout_secs — env > config (no global flag for timeout).
    cfg.models.voyage_timeout_secs = resolve_voyage_timeout_secs(None, &cfg.models, env);

    // rerank.location / rerank.model — resolve "auto"/env into the concrete
    // placement and model the CLI would use. Placement keys off whether a
    // Voyage key is effective.
    let placement = resolve_rerank_placement(None, &cfg.rerank, env, resolved_key.is_some())?;
    cfg.rerank.location = Some(placement.wire().to_owned());
    if let Some(model) = resolve_rerank_model(None, &cfg.rerank, env)?.model_name() {
        cfg.rerank.model = Some(model.to_owned());
    }

    // models.cache_dir — show the directory `mnm` would actually use. The
    // env override (`MIDNIGHT_MANUAL_MODEL_CACHE_DIR`) sits above the config
    // value, which sits above the XDG/HOME default. `mnm config show` has no
    // `--cache-dir` flag, so the env is the top layer here.
    //
    // `resolve_with_override` applies config-over-env-chain; the env override
    // must still win over the config value, so it is checked first.
    let cache_env = CacheEnvAdapter(env);
    if let Some(dir) = env
        .var("MIDNIGHT_MANUAL_MODEL_CACHE_DIR")
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            mnm_embedding::cache::resolve_with_override(cfg.models.cache_dir.as_deref(), &cache_env)
        })
    {
        cfg.models.cache_dir = Some(dir);
    }

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
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_SERVER", "http://from-env:9");
        apply_effective_overrides(&mut cfg, Some("http://from-flag:8080/"), None, false, &env).unwrap();
        assert_eq!(cfg.server.url, "http://from-flag:8080");
    }

    #[test]
    fn server_env_applies_when_no_flag() {
        let mut cfg = Config::default();
        let env = FakeEnv::default().set("MIDNIGHT_MANUAL_SERVER", "http://localhost:8080/");
        apply_effective_overrides(&mut cfg, None, None, false, &env).unwrap();
        assert_eq!(cfg.server.url, "http://localhost:8080");
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
    fn cache_dir_env_override_wins_over_config() {
        let mut cfg = Config::default();
        cfg.models.cache_dir = Some(std::path::PathBuf::from("/from/config"));
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
