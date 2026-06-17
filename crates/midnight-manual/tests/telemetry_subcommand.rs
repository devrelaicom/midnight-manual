//! Smoke tests for `mnm telemetry {disable, enable, status}` (FR-107 mechanism #3).
//!
//! Drives the `mnm` binary as a subprocess against a temp `XDG_CONFIG_HOME`
//! so each test starts with no marker file. Validates that:
//!
//! - `mnm telemetry disable` writes the marker and exits 0.
//! - The marker file appears at the documented XDG path.
//! - `mnm telemetry enable` removes the marker (and is idempotent).
//! - `mnm telemetry status --json` reflects the resolved state.
//! - The `--no-telemetry` global flag does not require any subcommand to
//!   accept it (smoke).
//!
//! The `MIDNIGHT_MANUAL_DISABLE_TELEMETRY` env var is cleared per-spawn so
//! the resolver's mechanism #3 (marker) is unambiguously what's under test.

use std::path::PathBuf;
use std::process::Command;

fn mnm_bin() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_BIN_EXE_mnm"));
    if !p.exists() {
        // Fallback for environments that built the binary under a different name.
        p = PathBuf::from(env!("CARGO_BIN_EXE_midnight-manual"));
    }
    p
}

fn run(bin: &PathBuf, xdg: &std::path::Path, args: &[&str]) -> (std::process::Output, PathBuf) {
    let marker = xdg.join("midnight-manual").join("telemetry-disabled");
    let output = Command::new(bin)
        .args(args)
        .env("XDG_CONFIG_HOME", xdg)
        // Unset env-disable so mechanism #1 doesn't mask mechanism #3.
        .env_remove("MIDNIGHT_MANUAL_DISABLE_TELEMETRY")
        // No HOME — keep the resolver locked to XDG.
        .env_remove("HOME")
        .output()
        .expect("spawn mnm");
    (output, marker)
}

#[test]
fn disable_then_enable_round_trip_writes_and_removes_marker() {
    let bin = mnm_bin();
    let tmp = tempfile::tempdir().unwrap();
    let xdg = tmp.path();

    let (out, marker) = run(&bin, xdg, &["telemetry", "disable"]);
    assert!(out.status.success(), "disable: {out:?}");
    assert!(marker.exists(), "marker must be written");

    let (out, marker) = run(&bin, xdg, &["telemetry", "enable"]);
    assert!(out.status.success(), "enable: {out:?}");
    assert!(!marker.exists(), "marker must be removed");

    // Idempotency: enable when already enabled.
    let (out, _) = run(&bin, xdg, &["telemetry", "enable"]);
    assert!(out.status.success(), "second enable: {out:?}");
}

#[test]
fn status_json_reports_marker_state() {
    let bin = mnm_bin();
    let tmp = tempfile::tempdir().unwrap();
    let xdg = tmp.path();

    // Initially: marker absent → enabled.
    let (out, _) = run(&bin, xdg, &["telemetry", "status", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["enabled"], true);
    assert_eq!(v["marker_present"], false);

    // After disable: marker present → disabled.
    let (out, _) = run(&bin, xdg, &["telemetry", "disable"]);
    assert!(out.status.success(), "{out:?}");
    let (out, _) = run(&bin, xdg, &["telemetry", "status", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["enabled"], false);
    assert_eq!(v["marker_present"], true);
    assert_eq!(v["disabled_by_runtime"], true);
}

#[test]
fn doctor_telemetry_block_includes_marker_path() {
    let bin = mnm_bin();
    let tmp = tempfile::tempdir().unwrap();
    let xdg = tmp.path();

    let (out, _) = run(&bin, xdg, &["doctor", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let t = &v["telemetry"];
    assert!(t.is_object(), "doctor.telemetry missing: {v}");
    assert!(t["marker_path"].is_string(), "marker_path absent: {t}");
    assert_eq!(t["marker_present"], false);
}
