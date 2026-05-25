//! Regression guard for the in-repo sample corpus.
//!
//! `corpus/sample/hierarchy.yaml` is shipped as a smoke-test fixture for
//! freshly-deployed servers (see `docs/README-deploy.md` §9a). If the manifest
//! schema in `crates/mn-content/src/manifest.rs` ever changes shape, this test
//! catches the drift before someone runs into it during a production deploy.

use std::path::PathBuf;

use mn_content::manifest::Manifest;

fn sample_manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root above CARGO_MANIFEST_DIR")
        .join("corpus")
        .join("sample")
        .join("hierarchy.yaml")
}

#[test]
fn sample_manifest_parses_against_current_schema() {
    let path = sample_manifest_path();
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let manifest =
        Manifest::parse(&body).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    assert_eq!(manifest.manifest_version, 1);
    assert_eq!(manifest.root.name.as_deref(), Some("Sample"));
    let groups = &manifest.root.children;
    assert_eq!(groups.len(), 2, "sample manifest should have two groups");
    let files: Vec<_> = groups
        .iter()
        .flat_map(|g| g.children.iter().filter_map(|n| n.file.as_deref()))
        .collect();
    assert_eq!(
        files,
        vec![
            std::path::Path::new("welcome.md"),
            std::path::Path::new("how-it-works.md")
        ],
        "sample manifest file order changed"
    );
}

#[test]
fn sample_files_exist_on_disk() {
    let dir = sample_manifest_path()
        .parent()
        .expect("manifest has a parent")
        .to_path_buf();
    for f in ["welcome.md", "how-it-works.md"] {
        let p = dir.join(f);
        assert!(p.exists(), "sample file missing: {}", p.display());
    }
}
