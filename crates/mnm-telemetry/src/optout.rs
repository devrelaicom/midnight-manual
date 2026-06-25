//! Opt-out mechanisms for midnight-manual telemetry (FR-107):
//!
//! 1. env var `MIDNIGHT_MANUAL_DISABLE_TELEMETRY` (truthy disables),
//! 2. config `[telemetry].enabled = false`,
//! 3. a persistent marker file (`mnm telemetry disable`).
//!
//! Mechanisms 1 & 3 are resolved into the Gauge builder's `runtime_enabled`
//! input by [`crate::build`]; mechanism 2 maps to `config_enabled`.

use std::path::Path;

use mnm_core::config::ConfigEnv;

/// Canonical name of the disable-by-env-var environment variable.
pub const DISABLE_ENV_VAR: &str = "MIDNIGHT_MANUAL_DISABLE_TELEMETRY";

/// User-facing help text for the three opt-out mechanisms.
pub const HELP_TEXT: &str = "\
Telemetry is opt-out. To disable, do any of:
  1. Set MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1 in the environment.
  2. Set `telemetry.enabled = false` in your config.toml.
  3. Run `mnm telemetry disable` (writes a runtime marker).
When disabled, zero events leave your machine and no connection to the
telemetry endpoint is opened. See the README's 'Telemetry & Privacy'
section for what is collected.";

/// True when the disable-by-env-var is set to a truthy value.
#[must_use]
pub fn env_disabled(env: &impl ConfigEnv) -> bool {
    env.var(DISABLE_ENV_VAR).is_some_and(|v| is_truthy(&v))
}

fn is_truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

/// True when the persistent opt-out marker file exists.
#[must_use]
pub fn marker_present(path: &Path) -> bool {
    path.exists()
}

/// Write the persistent opt-out marker (creating parent dirs).
///
/// # Errors
/// Returns an error if the parent directory or file cannot be created.
pub fn write_marker(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        "# Presence of this file disables midnight-manual telemetry.\n\
         # Reverse with: mnm telemetry enable\n",
    )
}

/// Remove the persistent opt-out marker. Idempotent (absent = success).
///
/// # Errors
/// Returns an error only on a real filesystem failure (not "not found").
pub fn remove_marker(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_truthy_values_disable() {
        struct E(&'static str);
        impl mnm_core::config::ConfigEnv for E {
            fn var(&self, n: &str) -> Option<String> {
                (n == DISABLE_ENV_VAR).then(|| self.0.to_owned())
            }
        }
        for v in ["1", "true", "YES", " on "] {
            assert!(env_disabled(&E(v)), "{v:?} should disable");
        }
        for v in ["0", "false", "no", ""] {
            assert!(!env_disabled(&E(v)), "{v:?} should not disable");
        }
    }

    #[test]
    fn marker_roundtrip() {
        let dir = std::env::temp_dir().join(format!("mnm-optout-{}", std::process::id()));
        let path = dir.join("telemetry-disabled");
        let _ = std::fs::remove_file(&path);
        assert!(!marker_present(&path));
        write_marker(&path).unwrap();
        assert!(marker_present(&path));
        remove_marker(&path).unwrap();
        assert!(!marker_present(&path));
        remove_marker(&path).unwrap(); // idempotent
    }
}
