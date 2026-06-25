//! A present-but-malformed config.toml must fail loud, not silently default.
use std::process::Command;

#[test]
fn malformed_config_exits_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "this is = not = valid = toml\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["--config", cfg.to_str().unwrap(), "version"])
        .output()
        .expect("run mnm");

    assert!(!out.status.success(), "expected non-zero exit on malformed config");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("parse"), "stderr should mention the parse failure: {stderr}");
}
