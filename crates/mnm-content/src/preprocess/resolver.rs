//! Walk-up license resolution (spec §License detection & resolution).
//!
//! From a document's directory up to the source root, manifest fields first,
//! then LICENSE-family files, first evidence level wins. Memoized per dir.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::detect;
use super::LicenseDetector;
use crate::ingest::filter::is_license_filename;

/// Memoizing per-source resolver. One instance per ingest run.
pub struct LicenseResolver {
    base: PathBuf,
    /// rel dir -> resolution AT that single level (not inherited).
    level_cache: HashMap<PathBuf, Option<Vec<String>>>,
}

impl LicenseResolver {
    /// `base` is the source checkout root; the walk never goes above it.
    #[must_use]
    pub fn new(base: &Path) -> Self {
        Self {
            base: base.to_path_buf(),
            level_cache: HashMap::new(),
        }
    }

    /// Resolve for a document living in `rel_dir` (relative to base).
    pub fn resolve_for(&mut self, rel_dir: &Path) -> Option<Vec<String>> {
        let mut dir = Some(rel_dir.to_path_buf());
        while let Some(d) = dir {
            if let Some(found) = self.level(&d) {
                return Some(found);
            }
            if d.as_os_str().is_empty() {
                break;
            }
            dir = d.parent().map(Path::to_path_buf);
        }
        None
    }

    /// The source-root resolution only.
    pub fn root_license(&mut self) -> Option<Vec<String>> {
        self.level(Path::new(""))
    }

    /// Evidence at exactly one directory level: manifest field first, then
    /// license files. Memoized.
    fn level(&mut self, rel_dir: &Path) -> Option<Vec<String>> {
        if let Some(cached) = self.level_cache.get(rel_dir) {
            return cached.clone();
        }
        let abs = self.base.join(rel_dir);
        let found = manifest_license(&abs).or_else(|| license_files(&abs));
        self.level_cache
            .insert(rel_dir.to_path_buf(), found.clone());
        found
    }
}

/// Push `e` onto `exprs` unless already present. Position-independent
/// dedup (unlike `Vec::dedup`, which only collapses adjacent duplicates);
/// first occurrence wins, order is otherwise preserved.
fn push_unique(exprs: &mut Vec<String>, e: Option<String>) {
    if let Some(e) = e {
        if !exprs.contains(&e) {
            exprs.push(e);
        }
    }
}

/// Parse + validate one candidate expression; `None` (logged) when invalid.
fn valid_expr(s: &str, origin: &Path) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    match spdx::Expression::parse(s) {
        Ok(_) => Some(s.to_owned()),
        Err(e) => {
            tracing::debug!(
                origin = %origin.display(),
                expr = s,
                error = %e,
                "invalid SPDX expression in manifest; skipping"
            );
            None
        }
    }
}

/// Cargo.toml / package.json / pyproject.toml license fields at `dir`.
fn manifest_license(dir: &Path) -> Option<Vec<String>> {
    cargo_toml_license(dir)
        .or_else(|| package_json_license(dir))
        .or_else(|| pyproject_toml_license(dir))
}

/// `Cargo.toml` -- `[package].license`, else `[workspace.package].license`.
fn cargo_toml_license(dir: &Path) -> Option<Vec<String>> {
    let cargo = dir.join("Cargo.toml");
    let body = std::fs::read_to_string(&cargo).ok()?;
    let table: toml::Table = body.parse().ok()?;

    let get = |tbl: &toml::Table, section: &str| -> Option<String> {
        tbl.get(section)?
            .as_table()?
            .get("license")?
            .as_str()
            .map(str::to_owned)
    };

    let expr = get(&table, "package").or_else(|| {
        table
            .get("workspace")
            .and_then(|w| w.as_table())
            .and_then(|w| get(w, "package"))
    })?;
    valid_expr(&expr, &cargo).map(|e| vec![e])
}

/// `package.json` -- `"license"` string, or legacy object/array forms.
fn package_json_license(dir: &Path) -> Option<Vec<String>> {
    let pkg = dir.join("package.json");
    let body = std::fs::read_to_string(&pkg).ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;

    let mut exprs: Vec<String> = Vec::new();
    match v.get("license") {
        Some(serde_json::Value::String(s)) => push_unique(&mut exprs, valid_expr(s, &pkg)),
        Some(serde_json::Value::Object(o)) => {
            push_unique(
                &mut exprs,
                o.get("type")
                    .and_then(|t| t.as_str())
                    .and_then(|s| valid_expr(s, &pkg)),
            );
        }
        _ => {}
    }
    if let Some(arr) = v.get("licenses").and_then(|l| l.as_array()) {
        for entry in arr {
            push_unique(
                &mut exprs,
                entry
                    .get("type")
                    .and_then(|t| t.as_str())
                    .and_then(|s| valid_expr(s, &pkg)),
            );
        }
    }
    if exprs.is_empty() {
        None
    } else {
        Some(exprs)
    }
}

/// `pyproject.toml` -- `[project].license` string, `{text = ...}`, or
/// `{file = ...}` (matched via detection).
fn pyproject_toml_license(dir: &Path) -> Option<Vec<String>> {
    let py = dir.join("pyproject.toml");
    let body = std::fs::read_to_string(&py).ok()?;
    let table: toml::Table = body.parse().ok()?;
    let lic = table.get("project")?.as_table()?.get("license")?;

    let expr = match lic {
        toml::Value::String(s) => valid_expr(s, &py),
        toml::Value::Table(t) => t.get("text").and_then(|x| x.as_str()).map_or_else(
            || {
                t.get("file")
                    .and_then(|x| x.as_str())
                    .and_then(|file| detect_file(&dir.join(file)))
            },
            |text| valid_expr(text, &py),
        ),
        _ => None,
    }?;
    Some(vec![expr])
}

/// LICENSE-family files at `dir`, fuzzy-matched. Multiple files -> multiple
/// elements; no invented OR (spec §Field shape).
fn license_files(dir: &Path) -> Option<Vec<String>> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_license_filename(n))
        .collect();
    names.sort(); // deterministic order

    let mut exprs = Vec::new();
    for name in names {
        if let Some(e) = detect_file(&dir.join(name)) {
            if !exprs.contains(&e) {
                exprs.push(e);
            }
        }
    }
    if exprs.is_empty() {
        None
    } else {
        Some(exprs)
    }
}

/// Fuzzy-detect one file's license. Reads at most `READ_CAP` bytes off disk
/// (the detector caps its own analysis window further, at 16 KiB) so a
/// pathologically large `LICENSE`-named file cannot blow up ingest memory.
fn detect_file(path: &Path) -> Option<String> {
    use std::io::Read as _;

    const READ_CAP: u64 = 64 * 1024;
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(READ_CAP).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    detect::global()?.detect(&text)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn setup(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let abs = dir.path().join(rel);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, body).unwrap();
        }
        dir
    }

    #[test]
    fn cargo_toml_package_license_wins_at_root() {
        let dir =
            setup(&[("Cargo.toml", "[package]\nname = \"x\"\nlicense = \"Apache-2.0 OR MIT\"\n")]);
        let mut r = LicenseResolver::new(dir.path());
        assert_eq!(r.resolve_for(Path::new("src")), Some(vec!["Apache-2.0 OR MIT".into()]));
        assert_eq!(r.root_license(), Some(vec!["Apache-2.0 OR MIT".into()]));
    }

    #[test]
    fn nearer_package_json_overrides_root() {
        let dir = setup(&[
            ("Cargo.toml", "[package]\nname=\"x\"\nlicense = \"MIT\"\n"),
            ("pkgs/web/package.json", "{\"name\":\"web\",\"license\":\"BSD-3-Clause\"}"),
        ]);
        let mut r = LicenseResolver::new(dir.path());
        assert_eq!(r.resolve_for(Path::new("pkgs/web/src")), Some(vec!["BSD-3-Clause".into()]));
        assert_eq!(r.resolve_for(Path::new("other")), Some(vec!["MIT".into()]));
    }

    #[test]
    fn manifest_beats_license_file_at_same_level() {
        // LICENSE holds real, detectable Apache-2.0 text -- a *different*
        // license than the manifest declares -- so this test only passes
        // under correct manifest-first precedence. Reversed precedence
        // (license file checked before/instead of manifest) would detect
        // Apache-2.0 from the file and fail this assertion.
        let apache = spdx::text::LICENSE_TEXTS
            .iter()
            .find(|(n, _)| *n == "Apache-2.0")
            .unwrap()
            .1;
        let dir = setup(&[
            ("Cargo.toml", "[package]\nname=\"x\"\nlicense = \"MIT\"\n"),
            ("LICENSE", apache),
        ]);
        let mut r = LicenseResolver::new(dir.path());
        assert_eq!(r.resolve_for(Path::new("")), Some(vec!["MIT".to_owned()]));
    }

    #[test]
    fn sibling_license_files_yield_multiple_elements() {
        // Real MIT + Apache-2.0 texts required for detection >= 0.9; use the
        // spdx crate's own canonical texts to build the fixtures.
        let mit = spdx::text::LICENSE_TEXTS
            .iter()
            .find(|(n, _)| *n == "MIT")
            .unwrap()
            .1;
        let apache = spdx::text::LICENSE_TEXTS
            .iter()
            .find(|(n, _)| *n == "Apache-2.0")
            .unwrap()
            .1;
        let dir = setup(&[("LICENSE-MIT", mit), ("LICENSE-APACHE", apache)]);
        let mut r = LicenseResolver::new(dir.path());
        let got = r.resolve_for(Path::new("")).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.contains(&"MIT".to_owned()));
        assert!(got.contains(&"Apache-2.0".to_owned()));
    }

    #[test]
    fn workspace_package_license_honored() {
        let dir = setup(&[(
            "Cargo.toml",
            "[workspace]\nmembers=[\"a\"]\n[workspace.package]\nlicense = \"Apache-2.0\"\n",
        )]);
        let mut r = LicenseResolver::new(dir.path());
        assert_eq!(r.root_license(), Some(vec!["Apache-2.0".into()]));
    }

    #[test]
    fn invalid_expression_is_skipped_and_walk_continues() {
        let dir = setup(&[
            ("Cargo.toml", "[package]\nname=\"x\"\nlicense = \"MIT\"\n"),
            ("sub/package.json", "{\"license\": \"SEE LICENSE IN LICENSE\"}"),
        ]);
        let mut r = LicenseResolver::new(dir.path());
        // Invalid at sub level -> falls through to root MIT.
        assert_eq!(r.resolve_for(Path::new("sub")), Some(vec!["MIT".into()]));
    }

    #[test]
    fn nothing_found_is_none_and_memoized() {
        let dir = setup(&[("src/a.rs", "fn a() {}")]);
        let mut r = LicenseResolver::new(dir.path());
        assert_eq!(r.resolve_for(Path::new("src")), None);

        // Introduce a license source at base *after* the first (negative)
        // resolution -- a fresh disk read from "src" would now find it via
        // the walk-up to "". If per-level memoization were removed, this
        // second call would return Some(["MIT"]) instead of None.
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\nlicense = \"MIT\"\n")
            .unwrap();

        assert_eq!(
            r.resolve_for(Path::new("src")),
            None,
            "memoized None must survive a disk change"
        );
    }

    #[test]
    fn package_json_license_dedups_position_independently() {
        // `licenses` repeats "MIT" (already the primary `license`) after
        // "Apache-2.0"; adjacent-only dedup (`Vec::dedup`) would miss this
        // non-adjacent duplicate and yield ["MIT", "Apache-2.0", "MIT"].
        let dir = setup(&[(
            "package.json",
            r#"{"license":"MIT","licenses":[{"type":"Apache-2.0"},{"type":"MIT"}]}"#,
        )]);
        let got = package_json_license(dir.path()).unwrap();
        assert_eq!(got, vec!["MIT".to_owned(), "Apache-2.0".to_owned()]);
    }
}
