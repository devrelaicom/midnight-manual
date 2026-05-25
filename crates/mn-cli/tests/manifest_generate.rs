use std::process::Command;

#[test]
fn manifest_generate_walks_a_tempdir_and_writes_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::create_dir_all(base.join("docs")).unwrap();
    std::fs::write(base.join("docs/a.md"), "# A").unwrap();
    std::fs::write(base.join("docs/b.md"), "# B").unwrap();

    let out = base.join("hierarchy.yaml");
    let exe = env!("CARGO_BIN_EXE_mnm");
    let status = Command::new(exe)
        .args([
            "manifest",
            "generate",
            "docs/**/*.md",
            "--base",
        ])
        .arg(base)
        .args(["-o"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    let body = std::fs::read_to_string(&out).unwrap();
    assert!(body.contains("a.md"));
    assert!(body.contains("b.md"));
    assert!(body.contains("manifest_version: 1"));
}
