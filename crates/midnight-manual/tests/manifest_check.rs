//! CLI smoke tests for `mnm manifest check`.
//!
//! Two families of tests:
//!   * the historical exit-code smoke tests (the `manifest-smoke` CI contract),
//!   * the `--json` structured-output tests for issue #145.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

/// Run `mnm manifest check <args...>` with telemetry disabled so stderr is
/// deterministic, and capture the full output.
fn check(args: &[&std::ffi::OsStr]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mnm"));
    cmd.env("MIDNIGHT_MANUAL_DISABLE_TELEMETRY", "1");
    cmd.args(["manifest", "check"]);
    cmd.args(args);
    cmd.output().unwrap()
}

/// Convenience: `OsStr` from a str/path.
fn os(s: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(s)
}

/// Write a minimal single-URL sitemap to `path`.
fn write_sitemap(path: &Path, loc: &str) {
    std::fs::write(
        path,
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n\
             <url><loc>{loc}</loc></url>\n\
             </urlset>\n"
        ),
    )
    .unwrap();
}

/// Collect the `kind` strings from a parsed `--json` report.
fn kinds(v: &Value) -> Vec<String> {
    v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// The first issue object of the given `kind` (panics if absent).
fn issue_of<'a>(v: &'a Value, kind: &str) -> &'a Value {
    v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == kind)
        .unwrap_or_else(|| panic!("kind {kind} not found in {:?}", v["issues"]))
}

// ---------------------------------------------------------------------------
// Historical exit-code smoke tests (unchanged behaviour).
// ---------------------------------------------------------------------------

#[test]
fn manifest_check_passes_on_a_valid_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::write(base.join("a.md"), "# A").unwrap();
    let m_path = base.join("hierarchy.yaml");
    std::fs::write(&m_path, "manifest_version: 1\nroot:\n  children:\n    - file: a.md\n").unwrap();
    let out = check(&[m_path.as_os_str()]);
    assert!(out.status.success());
}

#[test]
fn manifest_check_fails_when_file_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let m_path = dir.path().join("hierarchy.yaml");
    std::fs::write(&m_path, "manifest_version: 1\nroot:\n  children:\n    - file: missing.md\n")
        .unwrap();
    let out = check(&[m_path.as_os_str()]);
    assert!(!out.status.success());
}

// ---------------------------------------------------------------------------
// Guard: the non-`--json` human path + exit codes are byte-for-byte unchanged.
// This is the `manifest-smoke` contract (it runs `check` WITHOUT `--json`).
// ---------------------------------------------------------------------------

#[test]
fn non_json_human_output_and_exit_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::write(base.join("a.md"), "# A").unwrap();

    // Success: exit 0, "ok" on stderr, nothing on stdout (stdout is reserved
    // for `--json` payloads; the human path must never write there).
    let ok_path = base.join("ok.yaml");
    std::fs::write(&ok_path, "manifest_version: 1\nroot:\n  children:\n    - file: a.md\n")
        .unwrap();
    let out = check(&[ok_path.as_os_str()]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "human path must not write to stdout");
    assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "ok");

    // Failure: exit non-zero, the `- missing file:` line on stderr, no stdout.
    let bad_path = base.join("bad.yaml");
    std::fs::write(&bad_path, "manifest_version: 1\nroot:\n  children:\n    - file: gone.md\n")
        .unwrap();
    let out = check(&[bad_path.as_os_str()]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "human path must not write to stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("- missing file: gone.md"), "stderr was: {stderr}");
}

#[test]
fn non_json_sitemap_coverage_line_pinned() {
    // The `manifest-smoke`-adjacent human coverage line must stay stable.
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::write(base.join("a.md"), "# A").unwrap();
    let m_path = base.join("hierarchy.yaml");
    std::fs::write(
        &m_path,
        "manifest_version: 1\nroot:\n  children:\n    - file: a.md\n      published_url: https://ex.com/a\n",
    )
    .unwrap();
    let sm = base.join("sitemap.xml");
    write_sitemap(&sm, "https://ex.com/a");

    let out = check(&[m_path.as_os_str(), os("--sitemap"), sm.as_os_str()]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty(), "human path must not write to stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("sitemap coverage: 1/1 (100%)"), "stderr was: {stderr}");
    assert!(stderr.contains("ok"), "stderr was: {stderr}");
}

// ---------------------------------------------------------------------------
// `--json`: structured output (#145).
// ---------------------------------------------------------------------------

#[test]
fn json_success_reports_ok_true_empty_issues_and_coverage() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::write(base.join("a.md"), "# A").unwrap();
    let m_path = base.join("hierarchy.yaml");
    std::fs::write(
        &m_path,
        "manifest_version: 1\nroot:\n  children:\n    - file: a.md\n      published_url: https://ex.com/a\n",
    )
    .unwrap();
    let sm = base.join("sitemap.xml");
    write_sitemap(&sm, "https://ex.com/a");

    let out = check(&[
        m_path.as_os_str(),
        os("--sitemap"),
        sm.as_os_str(),
        os("--json"),
    ]);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("stdout is one JSON document");

    assert_eq!(v["ok"], Value::Bool(true));
    // `manifest` echoes the path as given (identifying, not just the basename).
    assert!(
        v["manifest"].as_str().unwrap().ends_with("hierarchy.yaml"),
        "manifest was: {}",
        v["manifest"]
    );
    assert_eq!(v["issues"].as_array().unwrap().len(), 0, "no issues on success");
    assert_eq!(v["sitemap_coverage"]["matched"], 1);
    assert_eq!(v["sitemap_coverage"]["total"], 1);
    // Compare against the `Value` (not an extracted `f64`) to sidestep `float_cmp`.
    assert_eq!(v["sitemap_coverage"]["pct"], 100.0);
}

#[test]
fn json_success_without_sitemap_has_null_coverage() {
    // The everyday case: no `--sitemap` → `sitemap_coverage: null`, empty issues.
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::write(base.join("a.md"), "# A").unwrap();
    let m_path = base.join("hierarchy.yaml");
    std::fs::write(&m_path, "manifest_version: 1\nroot:\n  children:\n    - file: a.md\n").unwrap();

    let out = check(&[m_path.as_os_str(), os("--json")]);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("stdout is one JSON document");
    assert_eq!(v["ok"], Value::Bool(true));
    assert_eq!(v["issues"].as_array().unwrap().len(), 0);
    assert_eq!(v["sitemap_coverage"], Value::Null);
}

#[test]
fn json_reports_every_issue_kind() {
    // `validate()` short-circuits (unsafe checked before duplicate), so no
    // single manifest can surface both `unsafe_path` and `duplicate_file` in
    // one run. Two fixtures together exercise all four validation kinds.
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let sm = base.join("sitemap.xml");
    write_sitemap(&sm, "https://ex.com/other"); // matches neither fixture's URLs

    // Fixture A: unsafe_path (+ missing_file + unmatched_url).
    let a_path = base.join("a.yaml");
    std::fs::write(
        &a_path,
        "manifest_version: 1\n\
         root:\n\
         \x20 name: docs\n\
         \x20 children:\n\
         \x20   - path: ../does-not-exist-xyzzy\n\
         \x20   - file: missing.md\n\
         \x20     published_url: https://ex.com/orphan\n",
    )
    .unwrap();
    let out_a = check(&[
        a_path.as_os_str(),
        os("--sitemap"),
        sm.as_os_str(),
        os("--json"),
    ]);
    assert!(!out_a.status.success(), "unsafe_path + missing_file are blocking");
    let va: Value = serde_json::from_slice(&out_a.stdout).expect("A: one JSON document");
    assert_eq!(va["ok"], Value::Bool(false));

    // unsafe_path: exact offending path, blocking.
    let unsafe_i = issue_of(&va, "unsafe_path");
    assert_eq!(unsafe_i["path"], "../does-not-exist-xyzzy");
    assert_eq!(unsafe_i["blocking"], Value::Bool(true));
    // missing_file: exact path, blocking.
    let missing_i = issue_of(&va, "missing_file");
    assert_eq!(missing_i["path"], "missing.md");
    assert_eq!(missing_i["blocking"], Value::Bool(true));
    // unmatched_url: the leaf rel_path, advisory, detail names the missed URL.
    let unmatched_i = issue_of(&va, "unmatched_url");
    assert_eq!(unmatched_i["path"], "missing.md");
    assert_eq!(unmatched_i["blocking"], Value::Bool(false));
    assert!(
        unmatched_i["detail"]
            .as_str()
            .unwrap()
            .contains("https://ex.com/orphan"),
        "unmatched detail was: {}",
        unmatched_i["detail"]
    );
    // ok mirrors the blocking discriminator: at least one blocking issue -> ok:false.
    assert!(va["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["blocking"] == Value::Bool(true)));
    // sitemap_coverage fields present and well-formed.
    assert_eq!(va["sitemap_coverage"]["matched"], 0);
    assert_eq!(va["sitemap_coverage"]["total"], 1);
    assert_eq!(va["sitemap_coverage"]["pct"], 0.0);

    // Fixture B: duplicate_file (+ missing_file + unmatched_url).
    let b_path = base.join("b.yaml");
    std::fs::write(
        &b_path,
        "manifest_version: 1\n\
         root:\n\
         \x20 name: docs\n\
         \x20 children:\n\
         \x20   - file: dup.md\n\
         \x20     published_url: https://ex.com/orphan\n\
         \x20   - file: dup.md\n\
         \x20     published_url: https://ex.com/orphan2\n",
    )
    .unwrap();
    let out_b = check(&[
        b_path.as_os_str(),
        os("--sitemap"),
        sm.as_os_str(),
        os("--json"),
    ]);
    assert!(!out_b.status.success(), "duplicate_file + missing_file are blocking");
    let vb: Value = serde_json::from_slice(&out_b.stdout).expect("B: one JSON document");
    assert_eq!(vb["ok"], Value::Bool(false));

    let dup_i = issue_of(&vb, "duplicate_file");
    assert_eq!(dup_i["path"], "dup.md");
    assert_eq!(dup_i["blocking"], Value::Bool(true));
    assert_eq!(issue_of(&vb, "missing_file")["path"], "dup.md");
    let unmatched_b = issue_of(&vb, "unmatched_url");
    assert_eq!(unmatched_b["path"], "dup.md");
    assert_eq!(unmatched_b["blocking"], Value::Bool(false));

    // The union of both fixtures covers the full closed validation vocabulary.
    let mut all: Vec<String> = kinds(&va);
    all.extend(kinds(&vb));
    for expected in [
        "unsafe_path",
        "duplicate_file",
        "missing_file",
        "unmatched_url",
    ] {
        assert!(all.contains(&expected.to_owned()), "missing kind {expected}; got {all:?}");
    }
}

#[test]
fn json_parse_failure_emits_parse_error_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let m_path = dir.path().join("broken.yaml");
    // Unterminated YAML flow sequence -> parse error before any validation.
    std::fs::write(&m_path, "manifest_version: 1\nroot: [unterminated\n").unwrap();

    let out = check(&[m_path.as_os_str(), os("--json")]);
    assert!(!out.status.success(), "parse failure must still exit non-zero");
    let v: Value = serde_json::from_slice(&out.stdout).expect("envelope is one JSON document");
    assert_eq!(v["ok"], Value::Bool(false));
    assert!(
        v["manifest"].as_str().unwrap().ends_with("broken.yaml"),
        "manifest was: {}",
        v["manifest"]
    );
    assert_eq!(v["sitemap_coverage"], Value::Null);
    let issues = v["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1, "single-issue envelope");
    assert_eq!(issues[0]["kind"], "parse_error");
    assert_eq!(issues[0]["blocking"], Value::Bool(true));
    assert!(issues[0]["path"].as_str().unwrap().ends_with("broken.yaml"));
    assert!(issues[0]["detail"].as_str().unwrap().contains("parse"));
}

#[test]
fn json_sitemap_load_failure_degrades_to_null_but_non_json_errors() {
    // A valid manifest with an unreachable local sitemap file.
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    std::fs::write(base.join("a.md"), "# A").unwrap();
    let m_path = base.join("hierarchy.yaml");
    std::fs::write(&m_path, "manifest_version: 1\nroot:\n  children:\n    - file: a.md\n").unwrap();
    let missing_sitemap = base.join("nope.xml"); // never created -> load fails

    // Under `--json`: coverage is advisory -> degrade to null, still emit JSON,
    // exit 0 (the manifest itself is valid).
    let out = check(&[
        m_path.as_os_str(),
        os("--sitemap"),
        missing_sitemap.as_os_str(),
        os("--json"),
    ]);
    assert!(out.status.success(), "valid manifest -> exit 0 despite sitemap load failure");
    let v: Value = serde_json::from_slice(&out.stdout).expect("stdout is one JSON document");
    assert_eq!(v["ok"], Value::Bool(true));
    assert_eq!(v["issues"].as_array().unwrap().len(), 0);
    assert_eq!(v["sitemap_coverage"], Value::Null);

    // Without `--json`: the sitemap load error propagates unchanged (non-zero).
    let out = check(&[
        m_path.as_os_str(),
        os("--sitemap"),
        missing_sitemap.as_os_str(),
    ]);
    assert!(!out.status.success(), "non-json path must still surface the load error");
}

#[test]
fn help_lists_all_issue_kinds() {
    // Lock the documented closed vocabulary against drift (contract-test style).
    let out = Command::new(env!("CARGO_BIN_EXE_mnm"))
        .env("MIDNIGHT_MANUAL_DISABLE_TELEMETRY", "1")
        .args(["manifest", "check", "--help"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    for kind in [
        "parse_error",
        "unsafe_path",
        "duplicate_file",
        "missing_file",
        "unmatched_url",
    ] {
        assert!(help.contains(kind), "--help missing kind `{kind}`:\n{help}");
    }
}
