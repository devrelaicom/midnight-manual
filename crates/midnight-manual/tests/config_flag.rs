//! End-to-end coverage for the global `--config` / `MIDNIGHT_MANUAL_CONFIG`
//! contract (issue #163). A config path the user specified but that does not
//! exist must fail loud (non-zero exit) rather than silently defaulting to the
//! compiled-in production server. These tests drive the real `mnm` binary the
//! way it actually runs — the top-level `Config::discover` in `cli::run` is the
//! single loud guard, so *any* subcommand (here `version`) aborts.

use std::path::PathBuf;
use std::process::Command;

fn mnm_bin() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_BIN_EXE_mnm"));
    if p.exists() {
        p
    } else {
        // Fallback for environments that built the binary under the long name.
        PathBuf::from(env!("CARGO_BIN_EXE_midnight-manual"))
    }
}

/// `MIDNIGHT_MANUAL_CONFIG` pointing at a missing file → non-zero exit + a
/// `NotFound` diagnostic. The CLI surfaces this env var into `--config` via
/// clap, so it exercises the same authoritative path the flag does. This is the
/// scenario the earlier unit test asserted the *opposite* of.
#[test]
fn missing_env_config_path_exits_nonzero() {
    let bin = mnm_bin();
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("typo.toml");

    let out = Command::new(&bin)
        .args(["version"])
        .env("MIDNIGHT_MANUAL_CONFIG", &missing)
        .output()
        .expect("spawn mnm");

    assert!(
        !out.status.success(),
        "set-but-missing MIDNIGHT_MANUAL_CONFIG must fail loud: {out:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not found"), "stderr should name the missing config: {stderr}");
}

/// The explicit `--config <missing>` flag likewise exits non-zero rather than
/// silently defaulting to the production endpoint.
#[test]
fn missing_flag_config_path_exits_nonzero() {
    let bin = mnm_bin();
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("typo.toml");

    let out = Command::new(&bin)
        .args(["--config", missing.to_str().unwrap(), "version"])
        // Isolate the flag from any ambient env so only `--config` is in play.
        .env_remove("MIDNIGHT_MANUAL_CONFIG")
        .output()
        .expect("spawn mnm");

    assert!(!out.status.success(), "--config at a missing path must fail loud: {out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not found"), "stderr should name the missing config: {stderr}");
}
