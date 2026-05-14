//! Three-mechanism opt-out resolver (FR-107 / FR-108).
//!
//! Telemetry is opt-out, with three equivalent ways to disable it:
//!
//! 1. **Environment**: `MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1` (or any truthy
//!    value: `1`, `true`, `yes`).
//! 2. **Config**: `telemetry.enabled = false` in the user's `config.toml`.
//! 3. **Runtime toggle**: `mnm telemetry disable` writes a marker the client
//!    library reads at every emit (Phase 8b lands the writer).
//!
//! The resolver is queried on every event emit; FR-108 requires that when
//! disabled the client MUST send zero events, NOT open a connection to
//! `/v1/telemetry`, and MUST discard any in-memory queue.
//!
//! Discoverability — every component that ships telemetry MUST document all
//! three mechanisms in its `--help` output AND in its README (FR-107). This
//! module exposes [`DISABLE_ENV_VAR`] and [`HELP_TEXT`] so the CLI / MCP /
//! server can render the same canonical strings.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Canonical name of the disable-by-env-var environment variable.
pub const DISABLE_ENV_VAR: &str = "MIDNIGHT_MANUAL_DISABLE_TELEMETRY";

/// Canonical paragraph for `--help` output. Components SHOULD emit this
/// verbatim so the three mechanisms are documented identically everywhere.
pub const HELP_TEXT: &str = "\
Telemetry is opt-out. To disable, do any of:
  1. Set MIDNIGHT_MANUAL_DISABLE_TELEMETRY=1 in the environment.
  2. Set `telemetry.enabled = false` in your config.toml.
  3. Run `mnm telemetry disable` (writes a runtime marker).
When disabled, zero events leave your machine and no connection to the
telemetry endpoint is opened. See the README's 'Telemetry & Privacy'
section for what is collected.";

/// Abstraction over env-var lookup so the resolver is testable without
/// poisoning the process environment.
pub trait OptOutEnv {
    /// Read an env var, returning `None` if unset.
    fn var(&self, name: &str) -> Option<String>;
}

/// Default implementation backed by `std::env`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdEnv;

impl OptOutEnv for StdEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// Process-wide runtime toggle. `mnm telemetry disable` sets this to `true`
/// for the remainder of the process (and writes the persistent marker that
/// Phase 8b will land); `mnm telemetry enable` clears it. The static atomic
/// makes the check lock-free on every emit.
static RUNTIME_DISABLED: AtomicBool = AtomicBool::new(false);

/// Programmatic runtime opt-out — primarily used by tests, but also by the
/// CLI subcommand once Phase 8b adds it.
pub fn set_runtime_disabled(disabled: bool) {
    RUNTIME_DISABLED.store(disabled, Ordering::Release);
}

/// Read the runtime-disabled flag.
pub fn runtime_disabled() -> bool {
    RUNTIME_DISABLED.load(Ordering::Acquire)
}

/// Load the persistent runtime-disabled marker.
///
/// The presence of the file (regardless of its contents) sets the
/// runtime-disabled flag; absence clears it. Pass `None` to treat as "no
/// marker" — useful when the path can't be resolved (no `HOME` /
/// `XDG_CONFIG_HOME`).
///
/// This is mechanism #3 of FR-107 — the `mnm telemetry disable` runtime
/// toggle persists by writing this marker, and every component consults it
/// at startup so the choice survives across invocations.
pub fn load_persistent_marker(path: Option<&Path>) {
    let disabled = path.is_some_and(Path::exists);
    set_runtime_disabled(disabled);
}

/// Write the persistent marker (idempotent). The parent directory is
/// created if missing.
///
/// # Errors
///
/// Returns the underlying `std::io::Error` if the directory or file cannot
/// be created.
pub fn write_persistent_marker(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Content is irrelevant — we just need the file to exist. A short
    // self-documenting body helps future readers who stumble on it.
    std::fs::write(
        path,
        "# Presence of this file disables midnight-manual telemetry.\n\
         # Reverse with: mnm telemetry enable\n",
    )?;
    set_runtime_disabled(true);
    Ok(())
}

/// Remove the persistent marker (idempotent — missing is fine).
///
/// # Errors
///
/// Returns the underlying `std::io::Error` for any error other than
/// `NotFound`.
pub fn remove_persistent_marker(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    set_runtime_disabled(false);
    Ok(())
}

/// Single source of truth: is telemetry enabled right now?
///
/// `config_enabled = false` is one disable path; `env` reading the disable
/// var (with a truthy value) is another; the runtime toggle is the third.
/// All three are equivalent (FR-107) — any one of them disables the
/// component.
#[must_use]
pub fn is_enabled(env: &impl OptOutEnv, config_enabled: bool) -> bool {
    if !config_enabled {
        return false;
    }
    if let Some(v) = env.var(DISABLE_ENV_VAR) {
        if is_truthy(&v) {
            return false;
        }
    }
    if runtime_disabled() {
        return false;
    }
    true
}

fn is_truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeEnv(HashMap<String, String>);

    impl FakeEnv {
        fn with(mut self, k: &str, v: &str) -> Self {
            self.0.insert(k.into(), v.into());
            self
        }
    }

    impl OptOutEnv for FakeEnv {
        fn var(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }
    }

    // Every test that calls `is_enabled` must hold this lock — the function
    // reads a process-wide static atomic that the runtime-toggle test (and
    // sibling tests in `client::tests`) mutate, and cargo-test runs tests
    // in parallel by default.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        crate::test_lock::lock()
    }

    #[test]
    fn enabled_by_default_when_config_enabled_and_no_env() {
        let _g = lock();
        set_runtime_disabled(false);
        assert!(is_enabled(&FakeEnv::default(), true));
    }

    #[test]
    fn config_disable_wins() {
        let _g = lock();
        set_runtime_disabled(false);
        assert!(!is_enabled(&FakeEnv::default(), false));
    }

    #[test]
    fn env_var_truthy_values_disable() {
        let _g = lock();
        set_runtime_disabled(false);
        for v in ["1", "true", "TRUE", "yes", "on", " yes "] {
            let env = FakeEnv::default().with(DISABLE_ENV_VAR, v);
            assert!(!is_enabled(&env, true), "value {v:?} must disable");
        }
    }

    #[test]
    fn env_var_falsy_or_empty_does_not_disable() {
        let _g = lock();
        set_runtime_disabled(false);
        for v in ["0", "false", "", "no", "off"] {
            let env = FakeEnv::default().with(DISABLE_ENV_VAR, v);
            assert!(is_enabled(&env, true), "value {v:?} must not disable");
        }
    }

    #[test]
    fn runtime_toggle_disables() {
        struct ResetGuard;
        impl Drop for ResetGuard {
            fn drop(&mut self) {
                set_runtime_disabled(false);
            }
        }
        let _g = lock();
        let _r = ResetGuard;

        set_runtime_disabled(true);
        assert!(!is_enabled(&FakeEnv::default(), true));
        set_runtime_disabled(false);
        assert!(is_enabled(&FakeEnv::default(), true));
    }

    #[test]
    fn persistent_marker_round_trip_toggles_runtime_flag() {
        struct ResetGuard;
        impl Drop for ResetGuard {
            fn drop(&mut self) {
                set_runtime_disabled(false);
            }
        }
        let _g = lock();
        let _r = ResetGuard;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/telemetry-disabled");

        // Boot path: marker absent → flag clear.
        set_runtime_disabled(true);
        load_persistent_marker(Some(&path));
        assert!(!runtime_disabled(), "absent marker must clear the flag");

        // Write: flag set + file present.
        write_persistent_marker(&path).unwrap();
        assert!(runtime_disabled());
        assert!(path.exists());

        // Boot path after write: flag re-resolved from disk.
        set_runtime_disabled(false);
        load_persistent_marker(Some(&path));
        assert!(runtime_disabled());

        // Remove: flag clear + file gone.
        remove_persistent_marker(&path).unwrap();
        assert!(!runtime_disabled());
        assert!(!path.exists());

        // Remove again: idempotent.
        remove_persistent_marker(&path).unwrap();
    }

    #[test]
    fn load_persistent_marker_with_none_is_noop() {
        let _g = lock();
        set_runtime_disabled(true);
        // None path → resolver picks the "no path → no marker" branch,
        // which clears the flag.
        load_persistent_marker(None);
        assert!(!runtime_disabled());
    }

    #[test]
    fn help_text_lists_all_three_mechanisms() {
        // FR-107 requires every component's --help to mention all three
        // mechanisms. We assert by string-presence rather than parse the
        // text so that copy edits stay easy.
        assert!(HELP_TEXT.contains(DISABLE_ENV_VAR), "env var must be in HELP_TEXT");
        assert!(HELP_TEXT.contains("config.toml"), "config mechanism must be in HELP_TEXT");
        assert!(
            HELP_TEXT.contains("mnm telemetry disable"),
            "runtime command must be in HELP_TEXT"
        );
    }
}
