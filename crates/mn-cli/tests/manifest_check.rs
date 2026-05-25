#[test]
fn manifest_check_passes_on_a_valid_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::write(base.join("a.md"), "# A").unwrap();
    let m_path = base.join("hierarchy.yaml");
    std::fs::write(
        &m_path,
        "manifest_version: 1\nroot:\n  children:\n    - file: a.md\n",
    )
    .unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["manifest", "check"])
        .arg(&m_path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn manifest_check_fails_when_file_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let m_path = dir.path().join("hierarchy.yaml");
    std::fs::write(
        &m_path,
        "manifest_version: 1\nroot:\n  children:\n    - file: missing.md\n",
    )
    .unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_mnm"))
        .args(["manifest", "check"])
        .arg(&m_path)
        .status()
        .unwrap();
    assert!(!status.success());
}
