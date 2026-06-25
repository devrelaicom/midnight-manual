//! Shared XDG path helpers (D18).
//!
//! Every binary in the workspace (CLI, MCP, server) walks the same path
//! precedence to find on-disk state: explicit env override first, then
//! `$XDG_CONFIG_HOME/midnight-manual/`, then `$HOME/.config/midnight-manual/`.
//! These helpers consolidate that walk so the CLI commands, the MCP server,
//! and tests all agree on where files live.
//!
//! Each helper takes a [`crate::config::ConfigEnv`] so tests can drive the
//! resolver with a fake environment.
//!
//! Files this module resolves:
//!
//! - `auth.toml` — bearer / JWT store (see [`crate::auth_file`]).
//! - `keys/<user_id>.private` — Ed25519 signing-key seed for `mnm login`.
//! - `users.toml` — local user-store (CLI mutates; server reads as TOML
//!   *body* via env per FR-057, not as a path).

use std::path::PathBuf;

use crate::config::ConfigEnv;

/// Resolve the on-disk directory that holds CLI / server state.
///
/// Precedence:
///
/// 1. `$XDG_CONFIG_HOME/midnight-manual/`
///
/// 2. `$HOME/.config/midnight-manual/`
///
/// 3. `None` when neither env var is set.
#[must_use]
pub fn config_home(env: &impl ConfigEnv) -> Option<PathBuf> {
    if let Some(xdg) = env.var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg).join("midnight-manual"));
    }
    if let Some(home) = env.var("HOME") {
        return Some(PathBuf::from(home).join(".config").join("midnight-manual"));
    }
    None
}

/// Resolve the auth-file path (`<config_home>/auth.toml`).
#[must_use]
pub fn auth_file_path(env: &impl ConfigEnv) -> Option<PathBuf> {
    config_home(env).map(|p| p.join("auth.toml"))
}

/// Resolve the keypair-storage directory (`<config_home>/keys/`).
///
/// Callers should `create_dir_all(...)` before writing into it.
#[must_use]
pub fn keys_dir(env: &impl ConfigEnv) -> Option<PathBuf> {
    config_home(env).map(|p| p.join("keys"))
}

/// Resolve the on-disk path to a user's signing-key seed
/// (`<config_home>/keys/<user_id>.private`).
#[must_use]
pub fn private_key_path(env: &impl ConfigEnv, user_id: &str) -> Option<PathBuf> {
    keys_dir(env).map(|p| p.join(format!("{user_id}.private")))
}

/// Resolve the persistent telemetry-opt-out marker path.
///
/// Returns `<config_home>/telemetry-disabled`. The presence of this file is
/// the third opt-out mechanism (FR-107 mechanism #3); the CLI / MCP / server
/// consult it at startup. The file contents are irrelevant — only existence
/// matters.
#[must_use]
pub fn telemetry_marker_path(env: &impl ConfigEnv) -> Option<PathBuf> {
    config_home(env).map(|p| p.join("telemetry-disabled"))
}

/// Resolve the persistent telemetry install-id path (`<config_home>/telemetry/id`).
///
/// Gauge mints an anonymous UUID here on first run; the queue file defaults
/// alongside it. Returns `None` when no config home can be resolved (telemetry
/// then stays disabled).
#[must_use]
pub fn telemetry_install_id_path(env: &impl ConfigEnv) -> Option<PathBuf> {
    config_home(env).map(|p| p.join("telemetry").join("id"))
}

/// Resolve the user-store path on the CLI side.
///
/// On the CLI side, `MIDNIGHT_MANUAL_USER_STORE` is read as a **file path**;
/// the server boot path treats the same env var as the in-memory TOML body
/// (see `crates/midnight-manual-server/src/config.rs`). This asymmetry is deliberate per
/// D18 so deployed servers can boot from a single secret-mounted env var
/// while admin operators edit a real file locally.
#[must_use]
pub fn user_store_path(env: &impl ConfigEnv) -> Option<PathBuf> {
    if let Some(p) = env.var("MIDNIGHT_MANUAL_USER_STORE") {
        return Some(PathBuf::from(p));
    }
    config_home(env).map(|p| p.join("users.toml"))
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
    fn xdg_beats_home_for_config_dir() {
        let env = FakeEnv::default()
            .set("XDG_CONFIG_HOME", "/x")
            .set("HOME", "/h");
        assert_eq!(config_home(&env), Some(PathBuf::from("/x/midnight-manual")));
    }

    #[test]
    fn home_fallback_for_config_dir() {
        let env = FakeEnv::default().set("HOME", "/h");
        assert_eq!(config_home(&env), Some(PathBuf::from("/h/.config/midnight-manual")),);
    }

    #[test]
    fn none_when_no_env_at_all() {
        let env = FakeEnv::default();
        assert!(config_home(&env).is_none());
        assert!(auth_file_path(&env).is_none());
        assert!(keys_dir(&env).is_none());
        assert!(private_key_path(&env, "aaron").is_none());
    }

    #[test]
    fn auth_file_under_config_home() {
        let env = FakeEnv::default().set("HOME", "/h");
        assert_eq!(
            auth_file_path(&env),
            Some(PathBuf::from("/h/.config/midnight-manual/auth.toml")),
        );
    }

    #[test]
    fn private_key_path_uses_user_id() {
        let env = FakeEnv::default().set("XDG_CONFIG_HOME", "/x");
        assert_eq!(
            private_key_path(&env, "aaron"),
            Some(PathBuf::from("/x/midnight-manual/keys/aaron.private")),
        );
    }

    #[test]
    fn user_store_env_var_wins() {
        let env = FakeEnv::default()
            .set("MIDNIGHT_MANUAL_USER_STORE", "/etc/users.toml")
            .set("XDG_CONFIG_HOME", "/x");
        assert_eq!(user_store_path(&env), Some(PathBuf::from("/etc/users.toml")));
    }

    #[test]
    fn telemetry_marker_under_config_home() {
        let env = FakeEnv::default().set("XDG_CONFIG_HOME", "/x");
        assert_eq!(
            telemetry_marker_path(&env),
            Some(PathBuf::from("/x/midnight-manual/telemetry-disabled")),
        );
    }

    #[test]
    fn telemetry_marker_none_without_env() {
        let env = FakeEnv::default();
        assert!(telemetry_marker_path(&env).is_none());
    }

    #[test]
    fn user_store_xdg_fallback() {
        let env = FakeEnv::default().set("XDG_CONFIG_HOME", "/x");
        assert_eq!(user_store_path(&env), Some(PathBuf::from("/x/midnight-manual/users.toml")),);
    }

    #[test]
    fn install_id_path_under_config_home() {
        struct E;
        impl crate::config::ConfigEnv for E {
            fn var(&self, name: &str) -> Option<String> {
                (name == "XDG_CONFIG_HOME").then(|| "/tmp/xdg".to_owned())
            }
        }
        let p = telemetry_install_id_path(&E).expect("path");
        assert!(p.ends_with("midnight-manual/telemetry/id"), "got {p:?}");
    }

    #[test]
    fn install_id_path_none_without_home() {
        struct E;
        impl crate::config::ConfigEnv for E {
            fn var(&self, _: &str) -> Option<String> {
                None
            }
        }
        assert!(telemetry_install_id_path(&E).is_none());
    }
}
