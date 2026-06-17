//! Version-provenance extraction from code manifests (spec §1.1). Pure
//! filesystem readers — called from the ingest CLI alongside package detection.

use std::path::Path;

use mnm_core::provenance::SdkDependency;

/// npm scopes/prefixes whose dependencies are compatibility-relevant.
///
/// `@openzeppelin/compact-` (not `@openzeppelin/`) targets only the
/// `OpenZeppelin` Compact packages (`compact-contracts`, `compact-cli`,
/// `compact-builder`) and excludes the much larger Solidity-oriented
/// `@openzeppelin/` ecosystem.
pub const NPM_ALLOWLIST_PREFIXES: &[&str] = &["@midnight-ntwrk/", "@openzeppelin/compact-"];
/// cargo crate-name prefixes whose dependencies are compatibility-relevant.
pub const CARGO_ALLOWLIST_PREFIXES: &[&str] = &["midnight-", "mn-"];

fn allowlisted(kind: &str, name: &str) -> bool {
    let prefixes = if kind == "npm" {
        NPM_ALLOWLIST_PREFIXES
    } else {
        CARGO_ALLOWLIST_PREFIXES
    };
    prefixes.iter().any(|p| name.starts_with(p))
}

/// Extract allowlisted dependencies from one manifest (spec §1.1).
///
/// Reads npm `dependencies` / cargo `[dependencies]` only (dev-deps excluded).
/// `source_root` bounds the upward walk for cargo `workspace = true`
/// resolution. Read or parse failures return empty — extraction is never fatal
/// (spec §6).
#[must_use]
pub fn extract_manifest_deps(manifest_abs: &Path, source_root: &Path) -> Vec<SdkDependency> {
    match manifest_abs.file_name().and_then(|n| n.to_str()) {
        Some("package.json") => extract_npm(manifest_abs),
        Some("Cargo.toml") => extract_cargo(manifest_abs, source_root),
        _ => Vec::new(),
    }
}

fn extract_npm(path: &Path) -> Vec<SdkDependency> {
    let Ok(txt) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return Vec::new();
    };
    let Some(deps) = v.get("dependencies").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    deps.iter()
        .filter(|(name, _)| allowlisted("npm", name))
        .map(|(name, range)| SdkDependency {
            kind: "npm".into(),
            name: name.clone(),
            version_constraint: range.as_str().map(str::to_owned),
        })
        .collect()
}

fn extract_cargo(path: &Path, source_root: &Path) -> Vec<SdkDependency> {
    let Ok(txt) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(v) = txt.parse::<toml::Value>() else {
        return Vec::new();
    };
    let Some(deps) = v.get("dependencies").and_then(toml::Value::as_table) else {
        return Vec::new();
    };
    deps.iter()
        .filter(|(name, _)| allowlisted("cargo", name))
        .map(|(name, spec)| SdkDependency {
            kind: "cargo".into(),
            name: name.clone(),
            version_constraint: cargo_constraint(spec)
                .or_else(|| workspace_constraint(path, source_root, name)),
        })
        .collect()
}

/// `"1.4"` or `{ version = "1.4", ... }` → the constraint string.
fn cargo_constraint(spec: &toml::Value) -> Option<String> {
    match spec {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Table(t) => t
            .get("version")
            .and_then(toml::Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

/// Resolve `{ workspace = true }` by walking up to the nearest
/// `[workspace.dependencies]` table within `source_root`.
fn workspace_constraint(manifest: &Path, source_root: &Path, dep: &str) -> Option<String> {
    let mut dir = manifest.parent()?.parent();
    while let Some(d) = dir {
        let candidate = d.join("Cargo.toml");
        if candidate.is_file() {
            if let Ok(txt) = std::fs::read_to_string(&candidate) {
                if let Ok(v) = txt.parse::<toml::Value>() {
                    if let Some(spec) = v
                        .get("workspace")
                        .and_then(|w| w.get("dependencies"))
                        .and_then(|t| t.get(dep))
                    {
                        return cargo_constraint(spec);
                    }
                }
            }
        }
        if d == source_root {
            break;
        }
        dir = d.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn npm_deps_filtered_by_allowlist() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name":"app","version":"1.0.0","dependencies":{
                "@midnight-ntwrk/midnight-js":"^1.4.0","react":"^19.0.0",
                "@openzeppelin/compact-contracts":"^0.2.0",
                "@openzeppelin/contracts":"^5.0.0"},
                "devDependencies":{"@midnight-ntwrk/compact-runtime":"^0.9.0"}}"#,
        )
        .unwrap();
        let deps = extract_manifest_deps(&dir.path().join("package.json"), dir.path());
        // Kept: `@midnight-ntwrk/*` and the `@openzeppelin/compact-*` scope.
        // Excluded: react (not allowlisted), `@openzeppelin/contracts` (the
        // Solidity ecosystem, which lacks the `compact-` prefix), and everything
        // under devDependencies.
        let find = |name: &str| deps.iter().find(|d| d.name == name);
        assert_eq!(deps.len(), 2, "dev-deps and non-allowlisted excluded");
        let mjs = find("@midnight-ntwrk/midnight-js").expect("midnight-js kept");
        assert_eq!(mjs.kind, "npm");
        assert_eq!(mjs.version_constraint.as_deref(), Some("^1.4.0"));
        let ozc = find("@openzeppelin/compact-contracts").expect("oz compact kept");
        assert_eq!(ozc.version_constraint.as_deref(), Some("^0.2.0"));
        assert!(find("@openzeppelin/contracts").is_none(), "Solidity OZ excluded");
        assert!(find("react").is_none(), "non-allowlisted excluded");
        assert!(find("@midnight-ntwrk/compact-runtime").is_none(), "dev-deps excluded");
    }

    #[test]
    fn cargo_deps_with_workspace_inheritance() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers=[\"a\"]\n[workspace.dependencies]\nmidnight-ledger = \"2.1\"\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("a")).unwrap();
        fs::write(
            dir.path().join("a/Cargo.toml"),
            "[package]\nname=\"a\"\nversion=\"0.1.0\"\n[dependencies]\nmidnight-ledger = { workspace = true }\nserde = \"1\"\n",
        )
        .unwrap();
        let deps = extract_manifest_deps(&dir.path().join("a/Cargo.toml"), dir.path());
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].kind, "cargo");
        assert_eq!(deps[0].name, "midnight-ledger");
        assert_eq!(deps[0].version_constraint.as_deref(), Some("2.1"));
    }
}
