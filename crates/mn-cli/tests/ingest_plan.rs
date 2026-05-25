//! Smoke test for `mnm ingest plan` that exercises the full binary path
//! without a live server. The server URL is pointed at an unreachable address
//! (`127.0.0.1:1`) to verify that `fetch_prior_state` silently falls back to
//! an empty prior state rather than propagating the connection error.

#[test]
fn ingest_plan_runs_offline_with_empty_prior_state() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::write(base.join("a.md"), "# A").unwrap();
    let m_path = base.join("hierarchy.yaml");
    std::fs::write(&m_path, "manifest_version: 1\nroot:\n  children:\n    - file: a.md\n").unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["ingest", "plan"])
        .arg(&m_path)
        .args(["--source-slug", "test"])
        .env("MIDNIGHT_MANUAL_SERVER", "http://127.0.0.1:1") // unreachable; tolerated
        .status()
        .unwrap();
    assert!(status.success());
}
