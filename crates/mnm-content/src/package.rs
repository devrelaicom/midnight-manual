//! Package membership detection: Rust (`Cargo.toml` `[package]`) and TS/JS
//! (`package.json` `.name`). Everything else → `None` (FR-006).

use std::path::{Path, PathBuf};

/// Detected package membership for a code file.
pub struct DetectedPackage {
    /// Ecosystem: `"rust"` or `"npm"`.
    pub kind: String,
    /// Package name (`[package].name` or `package.json` `.name`).
    pub name: String,
    /// `[package].version` / `.version` from the manifest, when declared.
    pub version: Option<String>,
    /// Manifest path relative to `root`.
    pub manifest_path: PathBuf,
}

/// Walk up from `file` to `root` looking for the nearest manifest.
///
/// `Cargo.toml` with a `[package]` table → rust; `package.json` with `.name` →
/// npm. A workspace-only `Cargo.toml` (`[workspace]`, no `[package]`) is skipped
/// (the walk continues upward).
#[must_use]
pub fn detect(file: &Path, root: &Path) -> Option<DetectedPackage> {
    let mut dir = file.parent();
    while let Some(d) = dir {
        let cargo = d.join("Cargo.toml");
        if cargo.is_file() {
            if let Ok(txt) = std::fs::read_to_string(&cargo) {
                if let Ok(v) = txt.parse::<toml::Value>() {
                    if let Some(name) = v
                        .get("package")
                        .and_then(|p| p.get("name"))
                        .and_then(toml::Value::as_str)
                    {
                        return Some(DetectedPackage {
                            kind: "rust".into(),
                            name: name.into(),
                            version: v
                                .get("package")
                                .and_then(|p| p.get("version"))
                                .and_then(toml::Value::as_str)
                                .map(str::to_owned),
                            manifest_path: rel(&cargo, root),
                        });
                    }
                    // `[workspace]`-only → keep walking up.
                }
            }
        }
        let pkg = d.join("package.json");
        if pkg.is_file() {
            if let Ok(txt) = std::fs::read_to_string(&pkg) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                    if let Some(name) = v.get("name").and_then(serde_json::Value::as_str) {
                        return Some(DetectedPackage {
                            kind: "npm".into(),
                            name: name.into(),
                            version: v
                                .get("version")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned),
                            manifest_path: rel(&pkg, root),
                        });
                    }
                }
            }
        }
        if d == root {
            break;
        }
        dir = d.parent();
    }
    None
}

fn rel(p: &Path, root: &Path) -> PathBuf {
    p.strip_prefix(root).unwrap_or(p).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rust_package_from_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"midnight-foo\"\n").unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let f = dir.path().join("src/lib.rs");
        fs::write(&f, "fn x() {}").unwrap();
        let pkg = detect(&f, dir.path()).unwrap();
        assert_eq!(pkg.kind, "rust");
        assert_eq!(pkg.name, "midnight-foo");
    }

    #[test]
    fn workspace_root_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\nmembers=[\"a\"]\n").unwrap();
        let f = dir.path().join("a/src/lib.rs");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(dir.path().join("a/Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
        fs::write(&f, "fn x(){}").unwrap();
        let pkg = detect(&f, dir.path()).unwrap();
        assert_eq!(pkg.name, "a"); // nearest [package], not the workspace root
    }

    #[test]
    fn npm_package_from_package_json() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), r#"{"name":"@scope/web"}"#).unwrap();
        let f = dir.path().join("src/index.ts");
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, "export const x=1;").unwrap();
        let pkg = detect(&f, dir.path()).unwrap();
        assert_eq!(pkg.kind, "npm");
        assert_eq!(pkg.name, "@scope/web");
    }

    #[test]
    fn version_extracted_from_manifests() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"midnight-foo\"\nversion = \"0.3.1\"\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        let f = dir.path().join("src/lib.rs");
        fs::write(&f, "fn x() {}").unwrap();
        assert_eq!(detect(&f, dir.path()).unwrap().version.as_deref(), Some("0.3.1"));

        let dir2 = tempfile::tempdir().unwrap();
        fs::write(dir2.path().join("package.json"), r#"{"name":"@scope/web","version":"2.1.0"}"#)
            .unwrap();
        let f2 = dir2.path().join("src/index.ts");
        fs::create_dir_all(f2.parent().unwrap()).unwrap();
        fs::write(&f2, "export const x=1;").unwrap();
        assert_eq!(detect(&f2, dir2.path()).unwrap().version.as_deref(), Some("2.1.0"));
    }
}
